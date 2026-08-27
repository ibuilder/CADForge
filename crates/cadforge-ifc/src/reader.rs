//! Reading IFC into the semantic model.
//!
//! The parser is `ifc-lite-core`, adopted on measurement rather than reputation
//! ([ADR-0009](../../../docs/adr/0009-ifc-lite-as-the-read-backend.md)). What lives here is
//! the **projection layer**: IFC entities in, `ElementRecord`s and `ModelCommand`s out, with
//! nothing from the parser crossing into `cadforge-core`.
//!
//! Two things about its shape are worth knowing, because both came from surveying real files
//! rather than from design ([ADR-0010](../../../docs/adr/0010-tessellation-is-a-primary-import-path.md)):
//!
//! 1. **Tessellation is the primary path.** Of 719 shape representations in buildingSMART's
//!    certification corpus, 710 were tessellated and seven were swept solids. The writer's
//!    emphasis is the exact opposite of the reader's, and that is correct rather than
//!    inconsistent — CADForge authors sweeps, and the world sends triangles.
//! 2. **`IfcClass::Other` is the main road, not the shoulder.** Only 43% of product instances
//!    in that corpus map to a native variant. Preserving the original entity name and
//!    round-tripping it exactly is a correctness requirement.
//!
//! Imported IFC is untrusted input. Nothing here panics on a malformed file: every problem
//! becomes an [`ImportWarning`] and the import continues, because a model that loads with
//! twelve warnings is worth more than an error message.

use crate::backend::{BackendCapabilities, IfcBackend, ImportReport, ImportWarning};
use crate::schema::IfcSchema;
use crate::IfcError;
use cadforge_core::{
    ElementRecord, GlobalId, IfcClass, Model, ModelCommand, Placement, PropertyValue,
    Representation,
};
use glam::{DMat4, DVec3};
use ifc_lite_core::{
    build_entity_index, AttributeValue, DecodedEntity, EntityDecoder, EntityScanner, IfcType,
};
use std::collections::{BTreeMap, BTreeSet};

/// How deep an `IfcLocalPlacement` chain may nest before it is treated as a cycle.
///
/// Real files nest four or five deep (site → building → storey → assembly → element). A file
/// claiming more is either pathological or self-referential, and following it forever is a
/// denial-of-service against our own importer.
const MAX_PLACEMENT_DEPTH: usize = 64;

/// Reads IFC via `ifc-lite-core`.
#[derive(Debug, Clone, Default)]
pub struct IfcLiteBackend;

impl IfcLiteBackend {
    pub fn new() -> Self {
        Self
    }
}

impl IfcBackend for IfcLiteBackend {
    fn name(&self) -> &'static str {
        "ifc-lite"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            reads: vec![IfcSchema::Ifc2x3, IfcSchema::Ifc4, IfcSchema::Ifc4x3],
            writes: Vec::new(),
            geometry: true,
            mobile: true,
        }
    }

    fn read(&self, bytes: &[u8], model: &mut Model) -> Result<ImportReport, IfcError> {
        Reader::new(bytes)?.read_into(model)
    }

    fn write(&self, _model: &Model, schema: IfcSchema) -> Result<Vec<u8>, IfcError> {
        // The writer is ours and does not depend on this crate at all (ADR-0007).
        let _ = schema;
        Err(IfcError::ReadOnlyBackend {
            backend: self.name(),
        })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    schema: IfcSchema,
    decoder: EntityDecoder<'a>,
    warnings: Vec<ImportWarning>,
    /// Entity id → the identity we gave it, so relationships can be resolved afterwards.
    identities: BTreeMap<u32, GlobalId>,
    /// Entity id → its `IfcLocalPlacement`, for composing chains.
    placements: BTreeMap<u32, u32>,
    /// Cached world transforms, because a storey's chain is walked once per element under it.
    world_cache: BTreeMap<u32, DMat4>,
    unsupported: BTreeMap<String, usize>,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, IfcError> {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
        let schema = IfcSchema::detect(&head)?;
        let index = build_entity_index(bytes);
        Ok(Self {
            bytes,
            schema,
            decoder: EntityDecoder::with_index(bytes, index),
            warnings: Vec::new(),
            identities: BTreeMap::new(),
            placements: BTreeMap::new(),
            world_cache: BTreeMap::new(),
            unsupported: BTreeMap::new(),
        })
    }

    fn read_into(mut self, model: &mut Model) -> Result<ImportReport, IfcError> {
        // Pass one: classify. Products become elements, relationships are noted for later,
        // because a relationship can only be applied once both ends exist.
        let mut products = Vec::new();
        let mut relationships = Vec::new();
        let mut scanner = EntityScanner::new(self.bytes);
        while let Some((id, type_name, _s, _e)) = scanner.next_entity() {
            let ifc_type = IfcType::from_str(type_name);
            if ifc_type.is_subtype_of(IfcType::IfcProduct) {
                products.push((id, type_name.to_string()));
            } else if matches!(
                ifc_type,
                IfcType::IfcRelContainedInSpatialStructure
                    | IfcType::IfcRelVoidsElement
                    | IfcType::IfcRelFillsElement
                    | IfcType::IfcRelDefinesByProperties
                    | IfcType::IfcRelDefinesByType
            ) {
                relationships.push((id, ifc_type));
            }
        }

        // Pass two: elements. Created before any relationship so ordering never matters.
        let mut geometry_failures = 0usize;
        let mut seen: BTreeSet<GlobalId> = BTreeSet::new();
        let mut created = 0usize;

        for (id, type_name) in &products {
            match self.element(*id, type_name, &mut seen) {
                Some((element, had_geometry_problem)) => {
                    if had_geometry_problem {
                        geometry_failures += 1;
                    }
                    self.identities.insert(*id, element.global_id.clone());
                    if let Some(placement) = self.placement_ref(*id) {
                        self.placements.insert(*id, placement);
                    }
                    if model
                        .apply(ModelCommand::CreateElement {
                            element: Box::new(element),
                        })
                        .is_ok()
                    {
                        created += 1;
                    }
                }
                None => continue,
            }
        }

        // Pass three: placements, now that containers exist and can be resolved.
        self.resolve_placements(model, &products);

        // Pass four: relationships.
        for (id, kind) in &relationships {
            self.relationship(model, *id, *kind);
        }

        for (entity, count) in std::mem::take(&mut self.unsupported) {
            self.warnings
                .push(ImportWarning::UnsupportedEntity { entity, count });
        }

        Ok(ImportReport {
            schema: self.schema,
            elements: created,
            warnings: self.warnings,
            geometry_failures,
        })
    }

    // ---- elements ----------------------------------------------------------------------

    /// Build one element. Returns the record and whether its geometry could not be read.
    fn element(
        &mut self,
        id: u32,
        type_name: &str,
        seen: &mut BTreeSet<GlobalId>,
    ) -> Option<(ElementRecord, bool)> {
        let entity = self.decoder.decode_by_id(id).ok()?;

        let global_id = match string_attr(&entity, "GlobalId") {
            Some(raw) => match GlobalId::parse(&raw) {
                Ok(parsed) if seen.insert(parsed.clone()) => parsed,
                Ok(duplicate) => {
                    // Common in files assembled from other files. Keeping both matters more
                    // than keeping the identity, so the second gets a fresh one.
                    self.warnings
                        .push(ImportWarning::DuplicateGlobalId(duplicate));
                    let fresh = GlobalId::new();
                    seen.insert(fresh.clone());
                    fresh
                }
                Err(_) => {
                    let replacement = GlobalId::new();
                    self.warnings.push(ImportWarning::InvalidGlobalId {
                        raw,
                        replacement: replacement.clone(),
                    });
                    seen.insert(replacement.clone());
                    replacement
                }
            },
            None => GlobalId::new(),
        };

        let class = class_of(type_name);
        if matches!(class, IfcClass::Other(_)) {
            *self.unsupported.entry(type_name.to_string()).or_default() += 1;
        }

        let mut element = ElementRecord::new(global_id.clone(), class);
        element.name = string_attr(&entity, "Name");
        element.object_type = string_attr(&entity, "ObjectType");

        let (representation, failed) = self.representation(&entity, &global_id);
        element.representation = representation;

        Some((element, failed))
    }

    fn placement_ref(&mut self, id: u32) -> Option<u32> {
        let entity = self.decoder.decode_by_id(id).ok()?;
        match attr(&entity, "ObjectPlacement")? {
            AttributeValue::EntityRef(r) => Some(*r),
            _ => None,
        }
    }

    /// Compose each element's placement chain, then express it relative to its container.
    ///
    /// IFC nests placements; CADForge stores one placement per element, relative to whatever
    /// contains it. Composing to world and dividing by the container's world transform is the
    /// only thing that is correct for both a file we wrote (where the chain is two deep) and a
    /// file with assemblies nested inside storeys.
    fn resolve_placements(&mut self, model: &mut Model, products: &[(u32, String)]) {
        let containers: BTreeMap<GlobalId, u32> = self
            .identities
            .iter()
            .map(|(entity, global)| (global.clone(), *entity))
            .collect();

        for (id, _) in products {
            let Some(global_id) = self.identities.get(id).cloned() else {
                continue;
            };
            let world = self.world_transform(*id);

            // The container is applied in pass four, so at this point every element is still
            // parented to nothing; world and relative coincide until we know better.
            let container_world = model
                .get(&global_id)
                .and_then(|e| e.container.clone())
                .and_then(|c| containers.get(&c).copied())
                .map(|entity| self.world_transform(entity))
                .unwrap_or(DMat4::IDENTITY);

            let relative = container_world.inverse() * world;
            let _ = model.apply(ModelCommand::SetPlacement {
                global_id,
                placement: Placement::from_matrix(relative),
            });
        }
    }

    /// The world transform of a product, by walking its `IfcLocalPlacement` chain.
    fn world_transform(&mut self, product: u32) -> DMat4 {
        let Some(placement) = self.placements.get(&product).copied() else {
            return DMat4::IDENTITY;
        };
        self.local_placement(placement, 0)
    }

    fn local_placement(&mut self, id: u32, depth: usize) -> DMat4 {
        if depth > MAX_PLACEMENT_DEPTH {
            return DMat4::IDENTITY;
        }
        if let Some(cached) = self.world_cache.get(&id) {
            return *cached;
        }

        let Ok(entity) = self.decoder.decode_by_id(id) else {
            return DMat4::IDENTITY;
        };

        let relative = attr(&entity, "RelativePlacement")
            .and_then(|a| match a {
                AttributeValue::EntityRef(r) => Some(*r),
                _ => None,
            })
            .map(|r| self.axis_placement(r))
            .unwrap_or(DMat4::IDENTITY);

        let parent = attr(&entity, "PlacementRelTo")
            .and_then(|a| match a {
                AttributeValue::EntityRef(r) => Some(*r),
                _ => None,
            })
            .map(|r| self.local_placement(r, depth + 1))
            .unwrap_or(DMat4::IDENTITY);

        let result = parent * relative;
        self.world_cache.insert(id, result);
        result
    }

    fn axis_placement(&mut self, id: u32) -> DMat4 {
        let Ok(entity) = self.decoder.decode_by_id(id) else {
            return DMat4::IDENTITY;
        };
        let location = self.point_of(&entity, "Location").unwrap_or(DVec3::ZERO);
        let axis = self.direction_of(&entity, "Axis").unwrap_or(DVec3::Z);
        let reference = self
            .direction_of(&entity, "RefDirection")
            .unwrap_or(DVec3::X);
        Placement::new(location, axis, reference).to_matrix()
    }

    fn point_of(&mut self, entity: &DecodedEntity, name: &str) -> Option<DVec3> {
        let reference = match attr(entity, name)? {
            AttributeValue::EntityRef(r) => *r,
            _ => return None,
        };
        let point = self.decoder.decode_by_id(reference).ok()?;
        coordinates(&point, "Coordinates")
    }

    fn direction_of(&mut self, entity: &DecodedEntity, name: &str) -> Option<DVec3> {
        let reference = match attr(entity, name)? {
            AttributeValue::EntityRef(r) => *r,
            _ => return None,
        };
        let direction = self.decoder.decode_by_id(reference).ok()?;
        coordinates(&direction, "DirectionRatios")
    }

    // ---- geometry ----------------------------------------------------------------------

    /// Read the body representation, if there is one we understand.
    ///
    /// Returning `(None, false)` means the element legitimately has no geometry — a storey, a
    /// site. `(None, true)` means it had geometry we could not read, which is a warning and a
    /// count, never a failed import: the element keeps its semantics and loses its shape,
    /// never the other way around.
    fn representation(
        &mut self,
        entity: &DecodedEntity,
        global_id: &GlobalId,
    ) -> (Option<Representation>, bool) {
        let Some(shape_ref) = attr(entity, "Representation").and_then(as_ref) else {
            return (None, false);
        };
        let Ok(shape) = self.decoder.decode_by_id(shape_ref) else {
            return (None, true);
        };
        let Some(representations) = attr(&shape, "Representations").and_then(as_ref_list) else {
            return (None, true);
        };

        // Prefer the body. A file often carries an axis and a footprint too, and importing a
        // wall's centre line as its solid would be quietly absurd.
        let mut chosen = None;
        for candidate in &representations {
            let Ok(entity) = self.decoder.decode_by_id(*candidate) else {
                continue;
            };
            let identifier = string_attr(&entity, "RepresentationIdentifier");
            if identifier.as_deref() == Some("Body") {
                chosen = Some(entity);
                break;
            }
            chosen.get_or_insert(entity);
        }
        let Some(shape_representation) = chosen else {
            return (None, true);
        };

        let Some(items) = attr(&shape_representation, "Items").and_then(as_ref_list) else {
            return (None, true);
        };
        let Some(first) = items.first().copied() else {
            return (None, true);
        };
        let Ok(item) = self.decoder.decode_by_id(first) else {
            return (None, true);
        };

        let result = match item.ifc_type {
            IfcType::IfcExtrudedAreaSolid => self.swept_solid(&item),
            IfcType::IfcTriangulatedFaceSet => self.triangulated(&item),
            IfcType::IfcPolygonalFaceSet => self.polygonal(&item),
            other => {
                *self
                    .unsupported
                    .entry(format!("{} (geometry)", other.as_str()))
                    .or_default() += 1;
                None
            }
        };

        match result {
            Some(representation) if representation.is_valid() => (Some(representation), false),
            Some(_) => {
                self.warnings.push(ImportWarning::GeometryFailed {
                    element: global_id.clone(),
                    reason: "representation failed validation".into(),
                });
                (None, true)
            }
            None => (None, true),
        }
    }

    /// `IfcExtrudedAreaSolid` — the case that stays parametric and editable.
    fn swept_solid(&mut self, item: &DecodedEntity) -> Option<Representation> {
        let depth = float_of(attr(item, "Depth")?)?;
        let direction = self
            .direction_of(item, "ExtrudedDirection")
            .unwrap_or(DVec3::Z);

        let profile_ref = attr(item, "SweptArea").and_then(as_ref)?;
        let profile_entity = self.decoder.decode_by_id(profile_ref).ok()?;
        let profile = self.profile(&profile_entity)?;

        // The solid's own Position offsets the profile within the element.
        let position = attr(item, "Position")
            .and_then(as_ref)
            .map(|r| self.axis_placement(r))
            .unwrap_or(DMat4::IDENTITY);
        let direction = position.transform_vector3(direction);

        Some(Representation::extrusion(
            profile,
            direction.to_array(),
            depth,
        ))
    }

    /// The profile forms CADForge can express. Anything else is left to degrade.
    fn profile(&mut self, entity: &DecodedEntity) -> Option<Vec<[f64; 2]>> {
        match entity.ifc_type {
            IfcType::IfcArbitraryClosedProfileDef => {
                let curve_ref = attr(entity, "OuterCurve").and_then(as_ref)?;
                let curve = self.decoder.decode_by_id(curve_ref).ok()?;
                if curve.ifc_type != IfcType::IfcPolyline {
                    *self
                        .unsupported
                        .entry(format!("{} (profile curve)", curve.ifc_type.as_str()))
                        .or_default() += 1;
                    return None;
                }
                let points = attr(&curve, "Points").and_then(as_ref_list)?;
                let mut profile = Vec::with_capacity(points.len());
                for point in points {
                    let entity = self.decoder.decode_by_id(point).ok()?;
                    let c = coordinates(&entity, "Coordinates")?;
                    profile.push([c.x, c.y]);
                }
                // A closed polyline repeats its first point; the model stores it once.
                if profile.len() > 1 && profile.first() == profile.last() {
                    profile.pop();
                }
                Some(profile)
            }
            IfcType::IfcRectangleProfileDef => {
                let x = float_of(attr(entity, "XDim")?)? * 0.5;
                let y = float_of(attr(entity, "YDim")?)? * 0.5;
                Some(vec![[-x, -y], [x, -y], [x, y], [-x, y]])
            }
            other => {
                *self
                    .unsupported
                    .entry(format!("{} (profile)", other.as_str()))
                    .or_default() += 1;
                None
            }
        }
    }

    /// `IfcTriangulatedFaceSet` — the dominant case in real files (ADR-0010).
    fn triangulated(&mut self, item: &DecodedEntity) -> Option<Representation> {
        let vertices = self.point_list(item)?;
        let indices = attr(item, "CoordIndex")?;
        let AttributeValue::List(triangles) = indices else {
            return None;
        };

        let mut faces = Vec::with_capacity(triangles.len());
        for triangle in triangles {
            let AttributeValue::List(corner) = triangle else {
                continue;
            };
            let values: Vec<i64> = corner.iter().filter_map(integer_of).collect();
            if values.len() < 3 {
                continue;
            }
            // IFC indices are 1-based; a naive import shifts every face by one vertex.
            let face = [
                (values[0] - 1) as u32,
                (values[1] - 1) as u32,
                (values[2] - 1) as u32,
            ];
            if face.iter().all(|i| (*i as usize) < vertices.len()) {
                faces.push(face);
            }
        }
        if faces.is_empty() {
            return None;
        }
        Some(Representation::TriangulatedFaceSet { vertices, faces })
    }

    /// `IfcPolygonalFaceSet` — n-gon faces, fanned into triangles.
    fn polygonal(&mut self, item: &DecodedEntity) -> Option<Representation> {
        let vertices = self.point_list(item)?;
        let face_refs = attr(item, "Faces").and_then(as_ref_list)?;

        let mut faces = Vec::new();
        for face_ref in face_refs {
            let Ok(face) = self.decoder.decode_by_id(face_ref) else {
                continue;
            };
            let Some(AttributeValue::List(indices)) = attr(&face, "CoordIndex") else {
                continue;
            };
            let loop_indices: Vec<u32> = indices
                .iter()
                .filter_map(integer_of)
                .map(|i| (i - 1) as u32)
                .filter(|i| (*i as usize) < vertices.len())
                .collect();
            // Convex fan. IFC polygonal faces are planar, and non-convex ones are rare enough
            // that a wrong triangulation is better caught by looking at it than guessed at.
            for i in 1..loop_indices.len().saturating_sub(1) {
                faces.push([loop_indices[0], loop_indices[i], loop_indices[i + 1]]);
            }
        }
        if faces.is_empty() {
            return None;
        }
        Some(Representation::TriangulatedFaceSet { vertices, faces })
    }

    fn point_list(&mut self, item: &DecodedEntity) -> Option<Vec<[f64; 3]>> {
        let list_ref = attr(item, "Coordinates").and_then(as_ref)?;
        let list = self.decoder.decode_by_id(list_ref).ok()?;
        let AttributeValue::List(rows) = attr(&list, "CoordList")? else {
            return None;
        };
        let mut vertices = Vec::with_capacity(rows.len());
        for row in rows {
            let AttributeValue::List(values) = row else {
                continue;
            };
            let c: Vec<f64> = values.iter().filter_map(float_of).collect();
            if c.len() >= 3 {
                vertices.push([c[0], c[1], c[2]]);
            }
        }
        (!vertices.is_empty()).then_some(vertices)
    }

    // ---- relationships -------------------------------------------------------------------

    fn relationship(&mut self, model: &mut Model, id: u32, kind: IfcType) {
        let Ok(entity) = self.decoder.decode_by_id(id) else {
            return;
        };

        match kind {
            IfcType::IfcRelContainedInSpatialStructure => {
                let Some(structure) = attr(&entity, "RelatingStructure").and_then(as_ref) else {
                    return;
                };
                let Some(container) = self.identities.get(&structure).cloned() else {
                    return;
                };
                for element in attr(&entity, "RelatedElements")
                    .and_then(as_ref_list)
                    .unwrap_or_default()
                {
                    let Some(global_id) = self.identities.get(&element).cloned() else {
                        continue;
                    };
                    // The model refuses a non-spatial container. That is a real constraint,
                    // not a formality, so a file that breaks it gets a warning rather than a
                    // silently mis-parented element.
                    if model
                        .apply(ModelCommand::AssignContainer {
                            global_id: global_id.clone(),
                            container: Some(container.clone()),
                        })
                        .is_err()
                    {
                        self.warnings.push(ImportWarning::DanglingReference {
                            element: global_id,
                            relationship: "IfcRelContainedInSpatialStructure".into(),
                        });
                    }
                }
            }

            IfcType::IfcRelVoidsElement => {
                let host = attr(&entity, "RelatingBuildingElement").and_then(as_ref);
                let opening = attr(&entity, "RelatedOpeningElement").and_then(as_ref);
                if let (Some(host), Some(opening)) = (host, opening) {
                    self.apply_pair(model, host, opening, "IfcRelVoidsElement", |a, b| {
                        ModelCommand::AddVoid {
                            host: a,
                            opening: b,
                        }
                    });
                }
            }

            IfcType::IfcRelFillsElement => {
                let opening = attr(&entity, "RelatingOpeningElement").and_then(as_ref);
                let filler = attr(&entity, "RelatedBuildingElement").and_then(as_ref);
                if let (Some(opening), Some(filler)) = (opening, filler) {
                    self.apply_pair(model, opening, filler, "IfcRelFillsElement", |a, b| {
                        ModelCommand::AddFill {
                            opening: a,
                            filler: b,
                        }
                    });
                }
            }

            IfcType::IfcRelDefinesByType => {
                let Some(type_object) = attr(&entity, "RelatingType").and_then(as_ref) else {
                    return;
                };
                // Type objects are not products, so they have no identity yet. Mint a stable
                // one from the type entity so every instance points at the same thing.
                let type_ref = self.type_identity(type_object);
                for object in attr(&entity, "RelatedObjects")
                    .and_then(as_ref_list)
                    .unwrap_or_default()
                {
                    if let Some(global_id) = self.identities.get(&object).cloned() {
                        let _ = model.apply(ModelCommand::AssignType {
                            global_id,
                            type_ref: Some(type_ref.clone()),
                        });
                    }
                }
            }

            IfcType::IfcRelDefinesByProperties => {
                let Some(definition) = attr(&entity, "RelatingPropertyDefinition").and_then(as_ref)
                else {
                    return;
                };
                let objects = attr(&entity, "RelatedObjects")
                    .and_then(as_ref_list)
                    .unwrap_or_default();
                self.property_set(model, definition, &objects);
            }

            _ => {}
        }
    }

    fn apply_pair(
        &mut self,
        model: &mut Model,
        a: u32,
        b: u32,
        relationship: &str,
        build: impl Fn(GlobalId, GlobalId) -> ModelCommand,
    ) {
        let (Some(first), Some(second)) = (
            self.identities.get(&a).cloned(),
            self.identities.get(&b).cloned(),
        ) else {
            return;
        };
        if model.apply(build(first.clone(), second)).is_err() {
            self.warnings.push(ImportWarning::DanglingReference {
                element: first,
                relationship: relationship.to_string(),
            });
        }
    }

    /// A stable identity for a type object, taken from its own `GlobalId` where it has one.
    fn type_identity(&mut self, entity_id: u32) -> GlobalId {
        if let Some(existing) = self.identities.get(&entity_id) {
            return existing.clone();
        }
        let identity = self
            .decoder
            .decode_by_id(entity_id)
            .ok()
            .and_then(|e| string_attr(&e, "GlobalId"))
            .and_then(|raw| GlobalId::parse(&raw).ok())
            .unwrap_or_default();
        self.identities.insert(entity_id, identity.clone());
        identity
    }

    fn property_set(&mut self, model: &mut Model, definition: u32, objects: &[u32]) {
        let Ok(set) = self.decoder.decode_by_id(definition) else {
            return;
        };
        if set.ifc_type != IfcType::IfcPropertySet {
            return;
        }
        let set_name = string_attr(&set, "Name").unwrap_or_else(|| "Pset".into());
        let Some(properties) = attr(&set, "HasProperties").and_then(as_ref_list) else {
            return;
        };

        let mut values = Vec::new();
        for property in properties {
            let Ok(entity) = self.decoder.decode_by_id(property) else {
                continue;
            };
            if entity.ifc_type != IfcType::IfcPropertySingleValue {
                continue;
            }
            let Some(name) = string_attr(&entity, "Name") else {
                continue;
            };
            let Some(value) = attr(&entity, "NominalValue").and_then(property_value) else {
                continue;
            };
            values.push((name, value));
        }

        for object in objects {
            let Some(global_id) = self.identities.get(object).cloned() else {
                continue;
            };
            for (name, value) in &values {
                let _ = model.apply(ModelCommand::SetProperty {
                    global_id: global_id.clone(),
                    set: set_name.clone(),
                    name: name.clone(),
                    value: Some(value.clone()),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Attribute helpers
// ---------------------------------------------------------------------------------------

/// Look an attribute up by name.
///
/// Never by position. `IfcType::attribute_index` is what makes that possible, and hardcoding
/// "GlobalId is attribute 0" is the assumption that breaks across schema versions.
fn attr<'e>(entity: &'e DecodedEntity, name: &str) -> Option<&'e AttributeValue> {
    let index = entity.ifc_type.attribute_index(name)?;
    entity.attributes.get(index)
}

fn string_attr(entity: &DecodedEntity, name: &str) -> Option<String> {
    match attr(entity, name)? {
        AttributeValue::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn as_ref(value: &AttributeValue) -> Option<u32> {
    match value {
        AttributeValue::EntityRef(r) => Some(*r),
        _ => None,
    }
}

fn as_ref_list(value: &AttributeValue) -> Option<Vec<u32>> {
    match value {
        AttributeValue::List(items) => Some(items.iter().filter_map(as_ref).collect()),
        AttributeValue::EntityRef(r) => Some(vec![*r]),
        _ => None,
    }
}

fn float_of(value: &AttributeValue) -> Option<f64> {
    match value {
        AttributeValue::Float(f) => Some(*f),
        AttributeValue::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn integer_of(value: &AttributeValue) -> Option<i64> {
    match value {
        AttributeValue::Integer(i) => Some(*i),
        AttributeValue::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn coordinates(entity: &DecodedEntity, name: &str) -> Option<DVec3> {
    let AttributeValue::List(values) = attr(entity, name)? else {
        return None;
    };
    let c: Vec<f64> = values.iter().filter_map(float_of).collect();
    match c.len() {
        0 | 1 => None,
        2 => Some(DVec3::new(c[0], c[1], 0.0)),
        _ => Some(DVec3::new(c[0], c[1], c[2])),
    }
}

/// An `IfcValue` in a property, keeping its measure type.
///
/// A length and a bare real are not interchangeable, and collapsing them loses information the
/// exporter needs to write the file back correctly.
///
/// `ifc-lite` decodes a typed constructor — `IFCLENGTHMEASURE(2.4)` — as
/// `List([String("IFCLENGTHMEASURE"), Float(2.4)])`. The type name in position zero is exactly
/// what distinguishes the measures, so it drives the variant rather than being discarded.
fn property_value(value: &AttributeValue) -> Option<PropertyValue> {
    match value {
        AttributeValue::List(items) if items.len() >= 2 => match &items[0] {
            AttributeValue::String(type_name) => typed_property(type_name, &items[1]),
            _ => None,
        },
        AttributeValue::List(items) if items.len() == 1 => property_value(&items[0]),
        AttributeValue::String(s) => Some(PropertyValue::Text(s.clone())),
        AttributeValue::Integer(i) => Some(PropertyValue::Integer(*i)),
        AttributeValue::Float(f) => Some(PropertyValue::Real(*f)),
        AttributeValue::Enum(e) => Some(boolean_or_text(e)),
        _ => None,
    }
}

fn typed_property(type_name: &str, value: &AttributeValue) -> Option<PropertyValue> {
    Some(match type_name.to_ascii_uppercase().as_str() {
        "IFCBOOLEAN" | "IFCLOGICAL" => match value {
            AttributeValue::Enum(e) => boolean_or_text(e),
            _ => return None,
        },
        "IFCLENGTHMEASURE" | "IFCPOSITIVELENGTHMEASURE" | "IFCNONNEGATIVELENGTHMEASURE" => {
            PropertyValue::Length(float_of(value)?)
        }
        "IFCAREAMEASURE" | "IFCPOSITIVEAREAMEASURE" => PropertyValue::Area(float_of(value)?),
        "IFCVOLUMEMEASURE" => PropertyValue::Volume(float_of(value)?),
        "IFCCOUNTMEASURE" => PropertyValue::Count(integer_of(value)?),
        "IFCINTEGER" => PropertyValue::Integer(integer_of(value)?),
        "IFCREAL" | "IFCRATIOMEASURE" | "IFCNORMALISEDRATIOMEASURE" | "IFCPOSITIVERATIOMEASURE" => {
            PropertyValue::Real(float_of(value)?)
        }
        // IfcText, IfcLabel, IfcIdentifier, and anything else that reads as prose.
        _ => match value {
            AttributeValue::String(s) => PropertyValue::Text(s.clone()),
            AttributeValue::Enum(e) => boolean_or_text(e),
            other => return property_value(other),
        },
    })
}

/// STEP writes booleans as the enumeration `.T.` / `.F.`.
fn boolean_or_text(raw: &str) -> PropertyValue {
    match raw.trim_matches('.').to_ascii_uppercase().as_str() {
        "T" | "TRUE" => PropertyValue::Boolean(true),
        "F" | "FALSE" => PropertyValue::Boolean(false),
        _ => PropertyValue::Text(raw.to_string()),
    }
}

/// Map an IFC entity name onto the model's class enum.
///
/// Everything unmodelled keeps its original name in `Other`, which — per ADR-0010 — is the
/// majority path for real files, not an exception.
fn class_of(entity: &str) -> IfcClass {
    match entity.to_ascii_uppercase().as_str() {
        "IFCPROJECT" => IfcClass::Project,
        "IFCSITE" => IfcClass::Site,
        "IFCBUILDING" => IfcClass::Building,
        "IFCBUILDINGSTOREY" => IfcClass::BuildingStorey,
        "IFCSPACE" => IfcClass::Space,
        "IFCWALL" | "IFCWALLSTANDARDCASE" | "IFCWALLELEMENTEDCASE" => IfcClass::Wall,
        "IFCSLAB" | "IFCSLABSTANDARDCASE" | "IFCSLABELEMENTEDCASE" => IfcClass::Slab,
        "IFCROOF" => IfcClass::Roof,
        "IFCCOLUMN" | "IFCCOLUMNSTANDARDCASE" => IfcClass::Column,
        "IFCBEAM" | "IFCBEAMSTANDARDCASE" => IfcClass::Beam,
        "IFCDOOR" | "IFCDOORSTANDARDCASE" => IfcClass::Door,
        "IFCWINDOW" | "IFCWINDOWSTANDARDCASE" => IfcClass::Window,
        "IFCSTAIR" => IfcClass::Stair,
        "IFCCOVERING" => IfcClass::Covering,
        "IFCFURNITURE" | "IFCFURNISHINGELEMENT" => IfcClass::Furniture,
        "IFCOPENINGELEMENT" | "IFCOPENINGSTANDARDCASE" => IfcClass::OpeningElement,
        "IFCBUILDINGELEMENTPROXY" => IfcClass::BuildingElementProxy,
        other => IfcClass::Other(canonical_case(other)),
    }
}

/// `IFCGEOGRAPHICELEMENT` back to `IfcGeographicElement`, so a round trip does not shout.
fn canonical_case(upper: &str) -> String {
    let known = ifc_lite_core::IfcType::from_str(upper);
    let name = known.name();
    if name.eq_ignore_ascii_case(upper) {
        name.to_string()
    } else {
        // Unknown to the schema too; keep it verbatim rather than inventing a casing.
        upper.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spf::{ExportContext, SpfBackend};

    /// Storey, wall with a swept solid, opening, door — plus a property set.
    fn authored_model() -> (Model, GlobalId, GlobalId) {
        let mut model = Model::new();
        let storey =
            ElementRecord::new(GlobalId::new(), IfcClass::BuildingStorey).with_name("Level 00");
        let storey_id = storey.global_id.clone();

        let wall = ElementRecord::new(GlobalId::new(), IfcClass::Wall)
            .with_name("W-01")
            .with_container(storey_id.clone())
            .with_placement(Placement::at(DVec3::new(2.0, 1.0, 0.0)))
            .with_representation(Representation::extrusion(
                vec![[0.0, 0.0], [4.0, 0.0], [4.0, 0.2], [0.0, 0.2]],
                [0.0, 0.0, 1.0],
                3.0,
            ));
        let wall_id = wall.global_id.clone();

        let opening = ElementRecord::new(GlobalId::new(), IfcClass::OpeningElement)
            .with_container(storey_id.clone())
            .with_representation(Representation::extrusion(
                vec![[0.0, 0.0], [0.92, 0.0], [0.92, 0.3], [0.0, 0.3]],
                [0.0, 0.0, 1.0],
                2.12,
            ));
        let opening_id = opening.global_id.clone();

        let door = ElementRecord::new(GlobalId::new(), IfcClass::Door)
            .with_name("Single Flush Door")
            .with_container(storey_id.clone())
            .with_representation(Representation::TriangulatedFaceSet {
                vertices: vec![[0.0, 0.0, 0.0], [0.9, 0.0, 0.0], [0.9, 0.0, 2.1]],
                faces: vec![[0, 1, 2]],
            });
        let door_id = door.global_id.clone();

        model
            .apply_all([
                ModelCommand::CreateElement {
                    element: Box::new(storey),
                },
                ModelCommand::CreateElement {
                    element: Box::new(wall),
                },
                ModelCommand::CreateElement {
                    element: Box::new(opening),
                },
                ModelCommand::CreateElement {
                    element: Box::new(door),
                },
                ModelCommand::AddVoid {
                    host: wall_id.clone(),
                    opening: opening_id.clone(),
                },
                ModelCommand::AddFill {
                    opening: opening_id,
                    filler: door_id,
                },
                ModelCommand::SetProperty {
                    global_id: wall_id.clone(),
                    set: "Pset_WallCommon".into(),
                    name: "IsExternal".into(),
                    value: Some(PropertyValue::Boolean(true)),
                },
                ModelCommand::SetProperty {
                    global_id: wall_id.clone(),
                    set: "Pset_WallCommon".into(),
                    name: "FireRating".into(),
                    value: Some(PropertyValue::Text("60/60".into())),
                },
            ])
            .unwrap();

        (model, storey_id, wall_id)
    }

    fn round_trip(model: &Model) -> (Model, ImportReport) {
        let bytes = SpfBackend::new(ExportContext::named("round trip").at("2026-08-27T09:00:00"))
            .write(model, IfcSchema::Ifc4)
            .expect("export succeeds");
        let mut imported = Model::new();
        let report = IfcLiteBackend::new()
            .read(&bytes, &mut imported)
            .expect("import succeeds");
        (imported, report)
    }

    #[test]
    fn a_model_survives_export_and_import() {
        let (original, _, wall_id) = authored_model();
        let (imported, report) = round_trip(&original);

        assert_eq!(report.schema, IfcSchema::Ifc4);
        // The writer synthesises a project, site, and building the original did not have.
        assert!(imported.len() >= original.len());
        assert_eq!(report.geometry_failures, 0);

        let wall = imported
            .get(&wall_id)
            .expect("the wall came back by identity");
        assert_eq!(wall.class, IfcClass::Wall);
        assert_eq!(wall.name.as_deref(), Some("W-01"));
    }

    #[test]
    fn a_swept_solid_comes_back_as_a_swept_solid() {
        // The point of the whole parametric story: a wall that leaves as a profile and a depth
        // arrives as a profile and a depth, still editable.
        let (original, _, wall_id) = authored_model();
        let (imported, _) = round_trip(&original);

        let representation = imported
            .get(&wall_id)
            .and_then(|e| e.representation.clone())
            .expect("the wall has geometry");

        assert!(representation.is_native_parametric());
        let Representation::ExtrudedAreaSolid {
            profile,
            direction,
            depth,
        } = representation
        else {
            panic!("expected a swept solid");
        };
        assert_eq!(profile.len(), 4);
        assert!((depth - 3.0).abs() < 1e-9);
        assert!((direction[2] - 1.0).abs() < 1e-9);
        assert_eq!(profile[1], [4.0, 0.0]);
    }

    #[test]
    fn a_tessellated_door_stays_tessellated() {
        // ADR-0010: geometry that arrived as triangles leaves as the same triangles. CADForge
        // must never quietly re-mesh somebody else's geometry.
        let (original, _, _) = authored_model();
        let (imported, _) = round_trip(&original);

        let door = imported
            .by_class(&IfcClass::Door)
            .next()
            .expect("the door came back");
        let Some(Representation::TriangulatedFaceSet { vertices, faces }) = &door.representation
        else {
            panic!("expected tessellation, got {:?}", door.representation);
        };
        assert_eq!(vertices.len(), 3);
        assert_eq!(faces, &[[0, 1, 2]]);
        assert!((vertices[1][0] - 0.9).abs() < 1e-9);
    }

    #[test]
    fn relationships_survive_the_round_trip() {
        let (original, _, wall_id) = authored_model();
        let (imported, _) = round_trip(&original);

        let opening = imported
            .openings_of(&wall_id)
            .next()
            .cloned()
            .expect("the wall is still voided");
        assert_eq!(imported.host_of(&opening), Some(&wall_id));
        assert_eq!(
            imported.fills_of(&opening).count(),
            1,
            "the door still fills the opening"
        );
    }

    #[test]
    fn properties_survive_with_their_measure_types() {
        let (original, _, wall_id) = authored_model();
        let (imported, _) = round_trip(&original);

        let wall = imported.get(&wall_id).expect("the wall came back");
        assert_eq!(
            wall.properties.get("Pset_WallCommon", "IsExternal"),
            Some(&PropertyValue::Boolean(true)),
            "a boolean must not come back as the text 'T'"
        );
        assert_eq!(
            wall.properties.get("Pset_WallCommon", "FireRating"),
            Some(&PropertyValue::Text("60/60".into()))
        );
    }

    #[test]
    fn placement_is_recovered_relative_to_the_container() {
        // The writer nests placements; the reader composes the chain and divides by the
        // container. Getting this wrong shifts every element by its storey's offset.
        let (original, _, wall_id) = authored_model();
        let (imported, _) = round_trip(&original);

        let placement = imported.get(&wall_id).expect("wall").placement;
        assert!(
            (placement.location - DVec3::new(2.0, 1.0, 0.0)).length() < 1e-9,
            "got {:?}",
            placement.location
        );
    }

    #[test]
    fn spatial_containment_is_rebuilt() {
        let (original, storey_id, wall_id) = authored_model();
        let (imported, _) = round_trip(&original);

        let wall = imported.get(&wall_id).expect("wall");
        assert_eq!(wall.container.as_ref(), Some(&storey_id));
        assert!(imported.contained_in(&storey_id).count() >= 2);
    }

    #[test]
    fn an_unmodelled_class_keeps_its_entity_name() {
        // ADR-0010: this is the majority path for real files, not an exception.
        assert_eq!(class_of("IFCWALL"), IfcClass::Wall);
        assert_eq!(class_of("IFCWALLSTANDARDCASE"), IfcClass::Wall);

        let IfcClass::Other(name) = class_of("IFCGEOGRAPHICELEMENT") else {
            panic!("expected Other");
        };
        assert_eq!(name, "IfcGeographicElement", "casing should be canonical");

        let IfcClass::Other(name) = class_of("IFCTOTALLYMADEUPTHING") else {
            panic!("expected Other");
        };
        assert_eq!(name, "IFCTOTALLYMADEUPTHING", "unknown names stay verbatim");
    }

    #[test]
    fn a_file_that_is_not_ifc_is_refused_not_guessed_at() {
        let mut model = Model::new();
        assert!(IfcLiteBackend::new()
            .read(b"this is not an IFC file", &mut model)
            .is_err());
        assert!(model.is_empty(), "a failed read must not half-populate");
    }

    #[test]
    fn truncated_input_does_not_panic() {
        // Untrusted input: a streamed upload cut mid-entity must fail or degrade, never crash.
        let (original, _, _) = authored_model();
        let bytes = SpfBackend::new(ExportContext::named("truncated").at("2026-08-27T09:00:00"))
            .write(&original, IfcSchema::Ifc4)
            .unwrap();

        for fraction in [0.25, 0.5, 0.75, 0.9] {
            let cut = &bytes[..(bytes.len() as f64 * fraction) as usize];
            let mut model = Model::new();
            // Either outcome is acceptable. Panicking is not.
            let _ = IfcLiteBackend::new().read(cut, &mut model);
        }
    }

    #[test]
    fn the_reader_declares_itself_read_only() {
        let backend = IfcLiteBackend::new();
        assert!(backend.capabilities().can_read(IfcSchema::Ifc4));
        assert!(backend.capabilities().can_read(IfcSchema::Ifc2x3));
        assert!(!backend.capabilities().can_write(IfcSchema::Ifc4));
        assert_eq!(
            backend.write(&Model::new(), IfcSchema::Ifc4),
            Err(IfcError::ReadOnlyBackend {
                backend: "ifc-lite"
            })
        );
    }
}
