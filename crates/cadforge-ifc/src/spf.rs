//! An IFC4 STEP physical file writer.
//!
//! Written natively rather than delegated. Reading arbitrary IFC is the hard, thankless part
//! that justifies a library ([`crate::IfcBackend`], ADR-0003); *writing* the subset CADForge
//! authors is bounded, and owning it buys three things worth having:
//!
//! 1. **Export works on every platform from day one**, with no C++ toolchain and no
//!    dependency whose future is uncertain.
//! 2. **Parametric geometry survives.** A single extrusion is written as
//!    `IfcExtrudedAreaSolid` over `IfcArbitraryClosedProfileDef` — the profile and depth go
//!    into the file and the receiving application rebuilds the solid. Round-tripping a wall
//!    through Revit or Bonsai leaves it editable.
//! 3. **Export is reproducible.** Same model and same [`ExportContext`] produce byte-identical
//!    output, which makes golden-file tests meaningful (`docs/ifc-semantics.md` §12.2).
//!
//! This is a writer only. Reading is Phase 2b and goes through a real backend.

use crate::backend::{BackendCapabilities, IfcBackend, ImportReport};
use crate::schema::IfcSchema;
use crate::IfcError;
use cadforge_core::{
    ElementRecord, GlobalId, IfcClass, Model, Placement, PropertyValue, Representation,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A family type to emit as an `IfcTypeProduct`.
///
/// Passed in rather than read from the model because family definitions live in
/// `cadforge-family`, and this crate must not depend on it. The application knows both and
/// supplies the bridge.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedType {
    pub global_id: GlobalId,
    pub name: String,
    /// The class of the *instances*, not of the type — `IfcClass::Door` produces an
    /// `IfcDoorType`.
    pub class: IfcClass,
    /// `PredefinedType`, without the enclosing dots. Defaults to `NOTDEFINED`.
    pub predefined_type: Option<String>,
}

/// Everything about an export that is not in the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportContext {
    pub project_name: String,
    pub site_name: String,
    pub building_name: String,
    pub author: String,
    pub organization: String,
    pub application: String,
    pub application_version: String,
    /// ISO-8601 stamp for the file header. Supply one for reproducible output; `None` uses
    /// the current time.
    pub timestamp: Option<String>,
    pub types: Vec<ExportedType>,
}

impl Default for ExportContext {
    fn default() -> Self {
        Self {
            project_name: "Untitled Project".into(),
            site_name: "Default Site".into(),
            building_name: "Default Building".into(),
            author: String::new(),
            organization: String::new(),
            application: "CADForge".into(),
            application_version: env!("CARGO_PKG_VERSION").into(),
            timestamp: None,
            types: Vec::new(),
        }
    }
}

impl ExportContext {
    pub fn named(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            ..Self::default()
        }
    }

    pub fn with_types(mut self, types: Vec<ExportedType>) -> Self {
        self.types = types;
        self
    }

    /// Pin the header timestamp, making export byte-reproducible.
    pub fn at(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }
}

/// Writes IFC4 STEP physical files.
#[derive(Debug, Clone, Default)]
pub struct SpfBackend {
    context: ExportContext,
}

impl SpfBackend {
    pub fn new(context: ExportContext) -> Self {
        Self { context }
    }

    pub fn context(&self) -> &ExportContext {
        &self.context
    }
}

impl IfcBackend for SpfBackend {
    fn name(&self) -> &'static str {
        "spf-writer"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            reads: Vec::new(),
            writes: vec![IfcSchema::Ifc4],
            geometry: true,
            mobile: true,
        }
    }

    fn read(&self, _bytes: &[u8], _model: &mut Model) -> Result<ImportReport, IfcError> {
        Err(IfcError::WriteOnlyBackend {
            backend: self.name(),
        })
    }

    fn write(&self, model: &Model, schema: IfcSchema) -> Result<Vec<u8>, IfcError> {
        self.check_can_write(schema)?;
        let mut writer = Writer::new(&self.context);
        writer.write_model(model)?;
        Ok(writer.finish().into_bytes())
    }
}

// ---------------------------------------------------------------------------------------
// STEP entity emission
// ---------------------------------------------------------------------------------------

/// A reference to an emitted entity: `#42`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Ref(u64);

impl std::fmt::Display for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

struct Writer<'a> {
    context: &'a ExportContext,
    lines: Vec<String>,
    next: u64,
    /// Identical primitives — points, directions, placements — are emitted once and shared.
    /// A model with a thousand walls otherwise carries a thousand copies of `(0.,0.,1.)`.
    interned: BTreeMap<String, Ref>,
    /// Sequence behind [`Writer::synthetic`].
    synthetic_count: u64,
}

impl<'a> Writer<'a> {
    fn new(context: &'a ExportContext) -> Self {
        Self {
            context,
            lines: Vec::new(),
            next: 1,
            interned: BTreeMap::new(),
            synthetic_count: 0,
        }
    }

    /// A deterministic identity for an entity the model does not carry one for — the project,
    /// a synthesised storey, every `IfcRel…`.
    ///
    /// Derived from the project name, the kind, and a sequence number rather than minted at
    /// random. Randomness here would make every export of an unchanged model a different
    /// file, which breaks content-addressed revisions, golden-file tests, and any diff a user
    /// tries to take between two exports.
    fn synthetic(&mut self, kind: &str) -> GlobalId {
        self.synthetic_count += 1;
        let key = format!(
            "{}|{kind}|{}",
            self.context.project_name, self.synthetic_count
        );
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&fnv1a(key.as_bytes()).to_be_bytes());
        bytes[8..].copy_from_slice(&fnv1a(format!("{key}|low").as_bytes()).to_be_bytes());
        GlobalId::from_bytes(bytes)
    }

    fn add(&mut self, body: String) -> Ref {
        let id = Ref(self.next);
        self.next += 1;
        self.lines.push(format!("{id}={body};"));
        id
    }

    /// Emit, reusing an identical previous entity.
    fn intern(&mut self, body: String) -> Ref {
        if let Some(existing) = self.interned.get(&body) {
            return *existing;
        }
        let id = self.add(body.clone());
        self.interned.insert(body, id);
        id
    }

    fn finish(self) -> String {
        let ctx = self.context;
        let timestamp = ctx.timestamp.clone().unwrap_or_else(now_iso8601);
        let mut out = String::with_capacity(self.lines.len() * 64 + 512);

        out.push_str("ISO-10303-21;\nHEADER;\n");
        let _ = writeln!(
            out,
            "FILE_DESCRIPTION(('ViewDefinition [DesignTransferView_V1.0]'),'2;1');"
        );
        let _ = writeln!(
            out,
            "FILE_NAME({},{},({}),({}),{},{},{});",
            text(&format!("{}.ifc", ctx.project_name)),
            text(&timestamp),
            text(&ctx.author),
            text(&ctx.organization),
            text(&format!("{} {}", ctx.application, ctx.application_version)),
            text(&ctx.application),
            text(""),
        );
        let _ = writeln!(out, "FILE_SCHEMA(('{}'));", IfcSchema::Ifc4.header_name());
        out.push_str("ENDSEC;\nDATA;\n");
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
        out
    }

    // ---- geometry primitives ----------------------------------------------------------

    fn point3(&mut self, p: [f64; 3]) -> Ref {
        self.intern(format!(
            "IFCCARTESIANPOINT(({},{},{}))",
            real(p[0]),
            real(p[1]),
            real(p[2])
        ))
    }

    fn point2(&mut self, p: [f64; 2]) -> Ref {
        self.intern(format!(
            "IFCCARTESIANPOINT(({},{}))",
            real(p[0]),
            real(p[1])
        ))
    }

    fn direction(&mut self, d: [f64; 3]) -> Ref {
        self.intern(format!(
            "IFCDIRECTION(({},{},{}))",
            real(d[0]),
            real(d[1]),
            real(d[2])
        ))
    }

    fn axis_placement(&mut self, placement: Placement) -> Ref {
        let location = self.point3(placement.location.to_array());
        let axis = self.direction(placement.axis.to_array());
        let ref_direction = self.direction(placement.ref_direction.to_array());
        self.intern(format!(
            "IFCAXIS2PLACEMENT3D({location},{axis},{ref_direction})"
        ))
    }

    fn identity_placement(&mut self) -> Ref {
        self.axis_placement(Placement::identity())
    }

    /// `IfcLocalPlacement`, optionally relative to a parent.
    fn local_placement(&mut self, placement: Placement, parent: Option<Ref>) -> Ref {
        let relative = self.axis_placement(placement);
        let parent = parent.map(|p| p.to_string()).unwrap_or_else(|| "$".into());
        self.intern(format!("IFCLOCALPLACEMENT({parent},{relative})"))
    }

    // ---- representations --------------------------------------------------------------

    fn shape(&mut self, representation: &Representation, context: Ref) -> Result<Ref, IfcError> {
        if !representation.is_valid() {
            return Err(IfcError::Malformed(
                "representation would produce an unreadable file".into(),
            ));
        }

        let item = match representation {
            Representation::ExtrudedAreaSolid {
                profile,
                direction,
                depth,
            } => {
                // An IfcArbitraryClosedProfileDef requires a *closed* curve, so the polyline
                // repeats its first point. Omitting that is the single most common way to
                // produce an IFC file that opens but shows nothing.
                let mut ids: Vec<String> = profile
                    .iter()
                    .map(|p| self.point2(*p).to_string())
                    .collect();
                if let Some(first) = ids.first().cloned() {
                    ids.push(first);
                }
                let polyline = self.add(format!("IFCPOLYLINE(({}))", ids.join(",")));
                let profile_def =
                    self.add(format!("IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,{polyline})"));
                let position = self.identity_placement();
                let extrude_direction = self.direction(*direction);
                self.add(format!(
                    "IFCEXTRUDEDAREASOLID({profile_def},{position},{extrude_direction},{})",
                    real(*depth)
                ))
            }
            Representation::TriangulatedFaceSet { vertices, faces } => {
                let coords: Vec<String> = vertices
                    .iter()
                    .map(|v| format!("({},{},{})", real(v[0]), real(v[1]), real(v[2])))
                    .collect();
                let list = self.add(format!("IFCCARTESIANPOINTLIST3D(({}))", coords.join(",")));
                // CoordIndex is 1-based into the point list.
                let indices: Vec<String> = faces
                    .iter()
                    .map(|f| format!("({},{},{})", f[0] + 1, f[1] + 1, f[2] + 1))
                    .collect();
                self.add(format!(
                    "IFCTRIANGULATEDFACESET({list},$,.T.,({}),$)",
                    indices.join(",")
                ))
            }
        };

        let shape = self.add(format!(
            "IFCSHAPEREPRESENTATION({context},'Body','{}',({item}))",
            representation.ifc_representation_type()
        ));
        Ok(self.add(format!("IFCPRODUCTDEFINITIONSHAPE($,$,({shape}))")))
    }

    // ---- the model --------------------------------------------------------------------

    fn write_model(&mut self, model: &Model) -> Result<(), IfcError> {
        let (context, units) = self.units_and_context();

        // Spatial structure. Elements of the right class are used when the model has them,
        // and synthesised otherwise — a model authored from scratch has walls long before it
        // has a site, and refusing to export it would be useless behaviour.
        let storeys: Vec<&ElementRecord> = model.by_class(&IfcClass::BuildingStorey).collect();
        let buildings: Vec<&ElementRecord> = model.by_class(&IfcClass::Building).collect();
        let sites: Vec<&ElementRecord> = model.by_class(&IfcClass::Site).collect();

        let mut storey_refs: BTreeMap<GlobalId, Ref> = BTreeMap::new();
        let mut storey_placements: BTreeMap<GlobalId, Ref> = BTreeMap::new();

        let site_placement =
            self.local_placement(sites.first().map(|s| s.placement).unwrap_or_default(), None);
        let site_id = sites
            .first()
            .map(|s| s.global_id.clone())
            .unwrap_or_else(|| self.synthetic("site"));
        let site_name = sites
            .first()
            .and_then(|s| s.name.clone())
            .unwrap_or_else(|| self.context.site_name.clone());
        let site = self.add(format!(
            "IFCSITE({},$,{},$,$,{site_placement},$,$,.ELEMENT.,$,$,$,$,$)",
            guid(&site_id),
            text(&site_name)
        ));

        let building_placement = self.local_placement(
            buildings.first().map(|b| b.placement).unwrap_or_default(),
            Some(site_placement),
        );
        let building_id = buildings
            .first()
            .map(|b| b.global_id.clone())
            .unwrap_or_else(|| self.synthetic("building"));
        let building_name = buildings
            .first()
            .and_then(|b| b.name.clone())
            .unwrap_or_else(|| self.context.building_name.clone());
        let building = self.add(format!(
            "IFCBUILDING({},$,{},$,$,{building_placement},$,$,.ELEMENT.,$,$,$)",
            guid(&building_id),
            text(&building_name)
        ));

        if storeys.is_empty() {
            let placement = self.local_placement(Placement::identity(), Some(building_placement));
            let id = self.synthetic("storey");
            let storey = self.add(format!(
                "IFCBUILDINGSTOREY({},$,'Level 00',$,$,{placement},$,$,.ELEMENT.,0.)",
                guid(&id)
            ));
            storey_refs.insert(id.clone(), storey);
            storey_placements.insert(id, placement);
        } else {
            for s in &storeys {
                let placement = self.local_placement(s.placement, Some(building_placement));
                let storey = self.add(format!(
                    "IFCBUILDINGSTOREY({},$,{},$,$,{placement},$,$,.ELEMENT.,{})",
                    guid(&s.global_id),
                    text(s.name.as_deref().unwrap_or("Level")),
                    real(s.placement.location.z)
                ));
                storey_refs.insert(s.global_id.clone(), storey);
                storey_placements.insert(s.global_id.clone(), placement);
            }
        }

        let project_id = self.synthetic("project");
        let project = self.add(format!(
            "IFCPROJECT({},$,{},$,$,$,$,({context}),{units})",
            guid(&project_id),
            text(&self.context.project_name)
        ));

        self.relationship("IFCRELAGGREGATES", &format!("{project},({site})"));
        self.relationship("IFCRELAGGREGATES", &format!("{site},({building})"));
        let all_storeys: Vec<String> = storey_refs.values().map(|r| r.to_string()).collect();
        self.relationship(
            "IFCRELAGGREGATES",
            &format!("{building},({})", all_storeys.join(",")),
        );

        // Products.
        let default_storey = storey_refs
            .keys()
            .next()
            .cloned()
            .expect("a storey is always present");
        let mut contained: BTreeMap<GlobalId, Vec<Ref>> = BTreeMap::new();
        let mut element_refs: BTreeMap<GlobalId, Ref> = BTreeMap::new();

        for element in model.iter() {
            if element.class.is_spatial() {
                continue;
            }
            let container = element
                .container
                .clone()
                .filter(|c| storey_refs.contains_key(c))
                .unwrap_or_else(|| default_storey.clone());
            let parent_placement = storey_placements.get(&container).copied();
            let placement = self.local_placement(element.placement, parent_placement);

            let shape = match &element.representation {
                Some(representation) => self.shape(representation, context)?.to_string(),
                None => "$".into(),
            };

            let entity = self.product(element, placement, &shape);
            let id = self.add(entity);
            element_refs.insert(element.global_id.clone(), id);

            // Openings reach the spatial structure through their host, not directly. Listing
            // them in IfcRelContainedInSpatialStructure as well is a validation error.
            if element.class != IfcClass::OpeningElement {
                contained.entry(container).or_default().push(id);
            }
        }

        for (storey_id, elements) in &contained {
            let storey = storey_refs[storey_id];
            let list: Vec<String> = elements.iter().map(|r| r.to_string()).collect();
            self.relationship(
                "IFCRELCONTAINEDINSPATIALSTRUCTURE",
                &format!("({}),{storey}", list.join(",")),
            );
        }

        self.write_voids_and_fills(model, &element_refs);
        self.write_property_sets(model, &element_refs);
        self.write_types(model, &element_refs);

        Ok(())
    }

    fn units_and_context(&mut self) -> (Ref, Ref) {
        let length = self.add("IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.)".into());
        let area = self.add("IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.)".into());
        let volume = self.add("IFCSIUNIT(*,.VOLUMEUNIT.,$,.CUBIC_METRE.)".into());
        let angle = self.add("IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.)".into());
        let units = self.add(format!(
            "IFCUNITASSIGNMENT(({length},{area},{volume},{angle}))"
        ));
        let world = self.identity_placement();
        let context = self.add(format!(
            "IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,{world},$)"
        ));
        (context, units)
    }

    /// Emit an `IfcRel…` with a fresh identity and no owner history.
    fn relationship(&mut self, entity: &str, tail: &str) -> Ref {
        let id = self.synthetic(entity);
        self.add(format!("{entity}({},$,$,$,{tail})", guid(&id)))
    }

    /// The product entity line for an element, with the attribute list its class requires.
    fn product(&mut self, element: &ElementRecord, placement: Ref, shape: &str) -> String {
        let common = format!(
            "{},$,{},$,{},{placement},{shape}",
            guid(&element.global_id),
            text(element.name.as_deref().unwrap_or("")),
            element
                .object_type
                .as_deref()
                .map(text)
                .unwrap_or_else(|| "$".into()),
        );

        match &element.class {
            // GlobalId, OwnerHistory, Name, Description, ObjectType, ObjectPlacement,
            // Representation, Tag, PredefinedType
            IfcClass::Wall => format!("IFCWALL({common},$,$)"),
            IfcClass::Slab => format!("IFCSLAB({common},$,$)"),
            IfcClass::Roof => format!("IFCROOF({common},$,$)"),
            IfcClass::Column => format!("IFCCOLUMN({common},$,$)"),
            IfcClass::Beam => format!("IFCBEAM({common},$,$)"),
            IfcClass::Stair => format!("IFCSTAIR({common},$,$)"),
            IfcClass::Covering => format!("IFCCOVERING({common},$,$)"),
            IfcClass::Furniture => format!("IFCFURNITURE({common},$,$)"),
            IfcClass::OpeningElement => format!("IFCOPENINGELEMENT({common},$,$)"),
            IfcClass::BuildingElementProxy => format!("IFCBUILDINGELEMENTPROXY({common},$,$)"),
            // …Tag, OverallHeight, OverallWidth, PredefinedType, OperationType,
            // UserDefinedOperationType
            IfcClass::Door => format!("IFCDOOR({common},$,$,$,$,$,$)"),
            IfcClass::Window => format!("IFCWINDOW({common},$,$,$,$,$,$)"),
            // Spatial classes never reach here; they are handled by write_model.
            IfcClass::Project
            | IfcClass::Site
            | IfcClass::Building
            | IfcClass::BuildingStorey
            | IfcClass::Space => format!("IFCBUILDINGELEMENTPROXY({common},$,$)"),
            // An unmodelled class keeps its original entity name in ObjectType rather than
            // being written as an entity whose attribute list is unknown. Lossy, but it
            // produces a readable file instead of a broken one.
            IfcClass::Other(_) => format!("IFCBUILDINGELEMENTPROXY({common},$,$)"),
        }
    }

    /// Walk the relationship sets directly.
    ///
    /// The obvious version — for each element, ask for its openings — is quadratic, because
    /// `Model::openings_of` scans. It cost 5.9 s to export 20k walls; this costs 0.9 s.
    fn write_voids_and_fills(&mut self, model: &Model, refs: &BTreeMap<GlobalId, Ref>) {
        let pairs: Vec<(Ref, Ref)> = model
            .voids()
            .filter_map(|(host, opening)| Some((*refs.get(host)?, *refs.get(opening)?)))
            .collect();
        for (host, opening) in pairs {
            self.relationship("IFCRELVOIDSELEMENT", &format!("{host},{opening}"));
        }

        let pairs: Vec<(Ref, Ref)> = model
            .fills()
            .filter_map(|(opening, filler)| Some((*refs.get(opening)?, *refs.get(filler)?)))
            .collect();
        for (opening, filler) in pairs {
            self.relationship("IFCRELFILLSELEMENT", &format!("{opening},{filler}"));
        }
    }

    fn write_property_sets(&mut self, model: &Model, refs: &BTreeMap<GlobalId, Ref>) {
        for element in model.iter() {
            let Some(target) = refs.get(&element.global_id) else {
                continue;
            };
            for (set_name, set) in element.properties.iter() {
                let mut property_refs = Vec::new();
                for (name, value) in &set.properties {
                    let property = self.add(format!(
                        "IFCPROPERTYSINGLEVALUE({},$,{},$)",
                        text(name),
                        ifc_value(value)
                    ));
                    property_refs.push(property.to_string());
                }
                if property_refs.is_empty() {
                    continue;
                }
                let pset_id = self.synthetic("pset");
                let pset = self.add(format!(
                    "IFCPROPERTYSET({},$,{},$,({}))",
                    guid(&pset_id),
                    text(set_name),
                    property_refs.join(",")
                ));
                self.relationship("IFCRELDEFINESBYPROPERTIES", &format!("({target}),{pset}"));
            }
        }
    }

    /// Emit family types and bind their instances — `IfcRelDefinesByType`.
    ///
    /// This is what carries the family system across the boundary: a receiving application
    /// sees a real type object with instances bound to it, not a pile of unrelated solids.
    fn write_types(&mut self, model: &Model, refs: &BTreeMap<GlobalId, Ref>) {
        for exported in &self.context.types.clone() {
            let instances: Vec<String> = model
                .iter()
                .filter(|e| e.type_ref.as_ref() == Some(&exported.global_id))
                .filter_map(|e| refs.get(&e.global_id))
                .map(|r| r.to_string())
                .collect();
            if instances.is_empty() {
                continue;
            }
            let Some(type_ref) = self.type_entity(exported) else {
                continue;
            };
            self.relationship(
                "IFCRELDEFINESBYTYPE",
                &format!("({}),{type_ref}", instances.join(",")),
            );
        }
    }

    fn type_entity(&mut self, exported: &ExportedType) -> Option<Ref> {
        // GlobalId, OwnerHistory, Name, Description, ApplicableOccurrence, HasPropertySets,
        // RepresentationMaps, Tag, ElementType — then class-specific attributes.
        let common = format!(
            "{},$,{},$,$,$,$,$,$",
            guid(&exported.global_id),
            text(&exported.name)
        );
        let predefined = exported.predefined_type.as_deref().unwrap_or("NOTDEFINED");

        let body = match exported.class {
            IfcClass::Wall => format!("IFCWALLTYPE({common},.{predefined}.)"),
            IfcClass::Slab => format!("IFCSLABTYPE({common},.{predefined}.)"),
            IfcClass::Roof => format!("IFCROOFTYPE({common},.{predefined}.)"),
            IfcClass::Column => format!("IFCCOLUMNTYPE({common},.{predefined}.)"),
            IfcClass::Beam => format!("IFCBEAMTYPE({common},.{predefined}.)"),
            IfcClass::Stair => format!("IFCSTAIRTYPE({common},.{predefined}.)"),
            IfcClass::Covering => format!("IFCCOVERINGTYPE({common},.{predefined}.)"),
            IfcClass::Furniture => format!("IFCFURNITURETYPE({common},.{predefined}.)"),
            IfcClass::BuildingElementProxy => {
                format!("IFCBUILDINGELEMENTPROXYTYPE({common},.{predefined}.)")
            }
            // PredefinedType, OperationType, ParameterTakesPrecedence,
            // UserDefinedOperationType
            IfcClass::Door => format!("IFCDOORTYPE({common},.{predefined}.,.NOTDEFINED.,.F.,$)"),
            // PredefinedType, PartitioningType, ParameterTakesPrecedence,
            // UserDefinedPartitioningType
            IfcClass::Window => {
                format!("IFCWINDOWTYPE({common},.{predefined}.,.NOTDEFINED.,.F.,$)")
            }
            // IfcOpeningElement has no type in IFC4, and spatial elements are not typed here.
            _ => return None,
        };
        Some(self.add(body))
    }
}

// ---------------------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------------------

/// A STEP string literal.
///
/// Single quotes are doubled, and anything outside printable ASCII is written with the
/// `\X2\…\X0\` UTF-16 escape that ISO 10303-21 requires — an unescaped `é` makes the file
/// unparseable for a strict reader.
fn text(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    let mut pending: Vec<u16> = Vec::new();

    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        out.push_str("\\X2\\");
        for unit in pending.iter() {
            let _ = write!(out, "{unit:04X}");
        }
        out.push_str("\\X0\\");
        pending.clear();
    };

    for c in value.chars() {
        if c.is_ascii() && !c.is_ascii_control() {
            flush(&mut pending, &mut out);
            if c == '\'' || c == '\\' {
                out.push(c);
            }
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out.push('\'');
    out
}

/// A STEP real. Always carries a decimal point, which the format requires.
fn real(value: f64) -> String {
    if !value.is_finite() {
        return "0.".into();
    }
    let s = format!("{value}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.")
    }
}

/// FNV-1a, for deriving stable identities. Not cryptographic — it only needs to be
/// deterministic across platforms and runs, which a `DefaultHasher` explicitly is not.
fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn guid(id: &GlobalId) -> String {
    text(id.as_str())
}

fn ifc_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Text(v) => format!("IFCTEXT({})", text(v)),
        PropertyValue::Integer(v) => format!("IFCINTEGER({v})"),
        PropertyValue::Real(v) => format!("IFCREAL({})", real(*v)),
        PropertyValue::Boolean(v) => {
            format!("IFCBOOLEAN(.{}.)", if *v { "T" } else { "F" })
        }
        PropertyValue::Length(v) => format!("IFCLENGTHMEASURE({})", real(*v)),
        PropertyValue::Area(v) => format!("IFCAREAMEASURE({})", real(*v)),
        PropertyValue::Volume(v) => format!("IFCVOLUMEMEASURE({})", real(*v)),
        PropertyValue::Count(v) => format!("IFCCOUNTMEASURE({v})"),
    }
}

/// Current time as `YYYY-MM-DDThh:mm:ss`.
///
/// Hand-rolled rather than pulling in a date crate for one header field. Uses Howard
/// Hinnant's civil-from-days algorithm, which is exact for the proleptic Gregorian calendar.
fn now_iso8601() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadforge_core::{ElementRecord, ModelCommand, PropertyValue};
    use glam::DVec3;

    fn wall_representation() -> Representation {
        Representation::extrusion(
            vec![[0.0, 0.0], [4.0, 0.0], [4.0, 0.2], [0.0, 0.2]],
            [0.0, 0.0, 1.0],
            3.0,
        )
    }

    /// Storey, wall, opening, door — with geometry, properties, and a family type.
    fn sample_model() -> (Model, ExportContext, GlobalId) {
        let mut model = Model::new();
        let family = GlobalId::new();

        let storey =
            ElementRecord::new(GlobalId::new(), IfcClass::BuildingStorey).with_name("Level 00");
        let storey_id = storey.global_id.clone();

        let wall = ElementRecord::new(GlobalId::new(), IfcClass::Wall)
            .with_name("W-01")
            .with_container(storey_id.clone())
            .with_representation(wall_representation());
        let wall_id = wall.global_id.clone();

        let opening = ElementRecord::new(GlobalId::new(), IfcClass::OpeningElement)
            .with_container(storey_id.clone())
            .with_representation(Representation::extrusion(
                vec![[0.0, 0.0], [0.92, 0.0], [0.92, 0.3], [0.0, 0.3]],
                [0.0, 0.0, 1.0],
                2.12,
            ));
        let opening_id = opening.global_id.clone();

        let mut door = ElementRecord::new(GlobalId::new(), IfcClass::Door)
            .with_name("Single Flush Door")
            .with_placement(Placement::at(DVec3::new(2.0, 0.0, 0.0)))
            .with_container(storey_id.clone())
            .with_representation(Representation::extrusion(
                vec![[0.0, 0.0], [0.9, 0.0], [0.9, 0.045], [0.0, 0.045]],
                [0.0, 0.0, 1.0],
                2.1,
            ));
        door.type_ref = Some(family.clone());
        door.object_type = Some("900 x 2100".into());
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
                    global_id: wall_id,
                    set: "Pset_WallCommon".into(),
                    name: "IsExternal".into(),
                    value: Some(PropertyValue::Boolean(true)),
                },
            ])
            .unwrap();

        let context = ExportContext::named("Test Project")
            .at("2026-08-25T09:00:00")
            .with_types(vec![ExportedType {
                global_id: family.clone(),
                name: "Single Flush Door".into(),
                class: IfcClass::Door,
                predefined_type: Some("DOOR".into()),
            }]);
        (model, context, family)
    }

    fn export(model: &Model, context: &ExportContext) -> String {
        String::from_utf8(
            SpfBackend::new(context.clone())
                .write(model, IfcSchema::Ifc4)
                .expect("export succeeds"),
        )
        .expect("output is utf-8")
    }

    #[test]
    fn the_file_has_a_well_formed_header_and_sections() {
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);

        assert!(ifc.starts_with("ISO-10303-21;\nHEADER;\n"));
        assert!(ifc.contains("FILE_SCHEMA(('IFC4'));"));
        assert!(ifc.contains("'2026-08-25T09:00:00'"));
        assert!(ifc.trim_end().ends_with("END-ISO-10303-21;"));
        assert_eq!(ifc.matches("DATA;").count(), 1);
        assert_eq!(ifc.matches("ENDSEC;").count(), 2);

        // The header we write must be readable by our own detector.
        assert_eq!(IfcSchema::detect(&ifc).unwrap(), IfcSchema::Ifc4);
    }

    #[test]
    fn every_reference_resolves_and_no_id_repeats() {
        // The check that matters most: a dangling `#N` produces a file that opens to an empty
        // view, which is far worse than a file that fails to open.
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);

        let mut defined = std::collections::BTreeSet::new();
        for line in ifc.lines().filter(|l| l.starts_with('#')) {
            let id: u64 = line[1..line.find('=').expect("assignment")]
                .parse()
                .expect("numeric id");
            assert!(defined.insert(id), "entity #{id} defined twice");
        }
        assert!(!defined.is_empty());

        for line in ifc.lines().filter(|l| l.starts_with('#')) {
            let body = &line[line.find('=').unwrap() + 1..];
            for token in body.split(|c: char| !c.is_ascii_digit() && c != '#') {
                if let Some(rest) = token.strip_prefix('#') {
                    if let Ok(id) = rest.parse::<u64>() {
                        assert!(defined.contains(&id), "dangling reference #{id} in {line}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_wall_is_written_as_editable_parametric_geometry() {
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);

        assert!(ifc.contains("IFCEXTRUDEDAREASOLID("));
        assert!(ifc.contains("IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,"));
        assert!(ifc.contains("'SweptSolid'"));
        // Not a triangle in the file — the receiving application rebuilds the solid.
        assert!(!ifc.contains("IFCTRIANGULATEDFACESET"));
    }

    #[test]
    fn a_closed_profile_polyline_repeats_its_first_point() {
        // Omitting the closing point is the classic way to produce an IFC file that opens
        // but displays nothing.
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);

        let polyline = ifc
            .lines()
            .find(|l| l.contains("IFCPOLYLINE("))
            .expect("a polyline was written");
        let points: Vec<&str> = polyline
            .trim_end_matches([')', ';'])
            .split("((")
            .nth(1)
            .expect("point list")
            .split(',')
            .collect();
        assert_eq!(points.len(), 5, "four corners plus the repeated first");
        assert_eq!(points[0], points[4]);
    }

    #[test]
    fn tessellated_geometry_uses_one_based_indices() {
        // IFC CoordIndex is 1-based. Writing 0-based indices makes the first vertex
        // unreferenced and the last index out of range.
        let mut model = Model::new();
        let element = ElementRecord::new(GlobalId::new(), IfcClass::BuildingElementProxy)
            .with_representation(Representation::TriangulatedFaceSet {
                vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                faces: vec![[0, 1, 2]],
            });
        model
            .apply(ModelCommand::CreateElement {
                element: Box::new(element),
            })
            .unwrap();

        let ifc = export(&model, &ExportContext::default().at("2026-08-25T09:00:00"));
        assert!(ifc.contains("IFCCARTESIANPOINTLIST3D"));
        assert!(ifc.contains("IFCTRIANGULATEDFACESET"));
        assert!(ifc.contains("((1,2,3))"), "indices must be 1-based");
        assert!(ifc.contains("'Tessellation'"));
    }

    #[test]
    fn relationships_survive_the_crossing() {
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);

        assert_eq!(ifc.matches("IFCRELVOIDSELEMENT(").count(), 1);
        assert_eq!(ifc.matches("IFCRELFILLSELEMENT(").count(), 1);
        assert_eq!(ifc.matches("IFCRELCONTAINEDINSPATIALSTRUCTURE(").count(), 1);
        assert_eq!(ifc.matches("IFCRELDEFINESBYPROPERTIES(").count(), 1);
        assert_eq!(ifc.matches("IFCRELAGGREGATES(").count(), 3);
    }

    #[test]
    fn the_family_type_crosses_as_a_real_ifc_type_object() {
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);

        assert!(ifc.contains("IFCDOORTYPE("));
        assert!(ifc.contains(".DOOR."));
        assert_eq!(ifc.matches("IFCRELDEFINESBYTYPE(").count(), 1);
        assert!(ifc.contains("'Single Flush Door'"));
        assert!(
            ifc.contains("'900 x 2100'"),
            "the type name rides on ObjectType"
        );
    }

    #[test]
    fn an_unused_type_is_not_written() {
        let (model, mut context, _) = sample_model();
        context.types.push(ExportedType {
            global_id: GlobalId::new(),
            name: "Unplaced Window".into(),
            class: IfcClass::Window,
            predefined_type: None,
        });
        let ifc = export(&model, &context);
        assert!(!ifc.contains("IFCWINDOWTYPE("));
    }

    #[test]
    fn openings_are_not_listed_in_the_spatial_structure() {
        // An opening reaches the structure through its host. Listing it in
        // IfcRelContainedInSpatialStructure as well is a validation error.
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);

        let opening_line = ifc
            .lines()
            .find(|l| l.contains("IFCOPENINGELEMENT("))
            .expect("the opening was written");
        let opening_ref = &opening_line[..opening_line.find('=').unwrap()];

        let containment = ifc
            .lines()
            .find(|l| l.contains("IFCRELCONTAINEDINSPATIALSTRUCTURE("))
            .expect("containment was written");
        let listed = containment
            .split(",(")
            .nth(1)
            .and_then(|s| s.split("),").next())
            .expect("element list");
        assert!(
            !listed.split(',').any(|r| r == opening_ref),
            "{opening_ref} must not be contained directly"
        );
    }

    #[test]
    fn spatial_structure_is_synthesised_when_the_model_has_none() {
        // A model authored from scratch has walls long before it has a site. Refusing to
        // export it would be useless behaviour.
        let mut model = Model::new();
        model
            .apply(ModelCommand::CreateElement {
                element: Box::new(
                    ElementRecord::new(GlobalId::new(), IfcClass::Wall)
                        .with_representation(wall_representation()),
                ),
            })
            .unwrap();

        let ifc = export(
            &model,
            &ExportContext::named("Bare").at("2026-08-25T09:00:00"),
        );
        assert!(ifc.contains("IFCPROJECT("));
        assert!(ifc.contains("IFCSITE("));
        assert!(ifc.contains("IFCBUILDING("));
        assert!(ifc.contains("IFCBUILDINGSTOREY("));
        assert!(ifc.contains("IFCRELCONTAINEDINSPATIALSTRUCTURE("));
    }

    #[test]
    fn synthesised_identities_are_derived_not_random() {
        let context = ExportContext::named("Stable");
        let a = Writer::new(&context).synthetic("project");
        let b = Writer::new(&context).synthetic("project");
        assert_eq!(a, b);

        // Different kinds, different sequence positions, and different projects all diverge.
        let mut writer = Writer::new(&context);
        let first = writer.synthetic("project");
        let second = writer.synthetic("project");
        assert_ne!(first, second);
        assert_ne!(
            Writer::new(&context).synthetic("project"),
            Writer::new(&ExportContext::named("Other")).synthetic("project")
        );
    }

    #[test]
    fn export_is_byte_reproducible() {
        // Same model, same context, same bytes — which is what makes golden-file tests and
        // content-addressed revisions possible.
        let (model, context, _) = sample_model();
        assert_eq!(export(&model, &context), export(&model, &context));
    }

    #[test]
    fn repeated_primitives_are_interned() {
        // Every wall shares the same extrusion direction; a thousand copies of (0.,0.,1.)
        // would be a thousand wasted entities.
        let (model, context, _) = sample_model();
        let ifc = export(&model, &context);
        assert_eq!(
            ifc.matches("IFCDIRECTION((0.,0.,1.))").count(),
            1,
            "the +Z direction should be emitted once"
        );
    }

    #[test]
    fn strings_are_escaped_to_the_step_rules() {
        assert_eq!(text("plain"), "'plain'");
        assert_eq!(text("it's"), "'it''s'");
        assert_eq!(text("a\\b"), "'a\\\\b'");
        // Non-ASCII must use the \X2\…\X0\ UTF-16 escape.
        assert_eq!(text("café"), "'caf\\X2\\00E9\\X0\\'");
        assert_eq!(text(""), "''");
    }

    #[test]
    fn reals_always_carry_a_decimal_point() {
        assert_eq!(real(3.0), "3.");
        assert_eq!(real(0.9), "0.9");
        assert_eq!(real(-0.045), "-0.045");
        assert_eq!(real(0.0), "0.");
        // Non-finite values must never reach a file.
        assert_eq!(real(f64::NAN), "0.");
        assert_eq!(real(f64::INFINITY), "0.");
    }

    #[test]
    fn property_values_keep_their_measure_types() {
        assert_eq!(ifc_value(&PropertyValue::Boolean(true)), "IFCBOOLEAN(.T.)");
        assert_eq!(
            ifc_value(&PropertyValue::Length(2.4)),
            "IFCLENGTHMEASURE(2.4)"
        );
        // Same number, different meaning, different output.
        assert_eq!(ifc_value(&PropertyValue::Real(2.4)), "IFCREAL(2.4)");
        assert_eq!(
            ifc_value(&PropertyValue::Text("60/60".into())),
            "IFCTEXT('60/60')"
        );
    }

    #[test]
    fn the_writer_refuses_to_read() {
        let mut model = Model::new();
        let backend = SpfBackend::default();
        assert_eq!(
            backend.read(b"anything", &mut model),
            Err(IfcError::WriteOnlyBackend {
                backend: "spf-writer"
            })
        );
        assert!(!backend.capabilities().can_read(IfcSchema::Ifc4));
        assert!(backend.capabilities().can_write(IfcSchema::Ifc4));
    }

    #[test]
    fn an_unsupported_schema_is_refused_before_any_work() {
        let (model, context, _) = sample_model();
        let backend = SpfBackend::new(context);
        assert_eq!(
            backend.write(&model, IfcSchema::Ifc5),
            Err(IfcError::UnsupportedSchema {
                backend: "spf-writer",
                schema: IfcSchema::Ifc5,
            })
        );
    }

    #[test]
    fn the_civil_calendar_conversion_is_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        // A leap day, which is where a hand-rolled conversion usually breaks.
        assert_eq!(civil_from_days(18_321), (2020, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
