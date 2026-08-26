//! Family definitions, types, and placement.

use crate::param::{ParamBag, ParamDef, ParamScope, ParamValue};
use crate::recipe::GeometryRecipe;
use crate::FamilyError;
use cadforge_core::{ElementRecord, GlobalId, IfcClass, ModelCommand, Placement, Representation};
use cadforge_geom::{CsgBackend, IndexedMesh, TessellationSettings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Whether geometry survives export as native parametric IFC, or has to be tessellated.
///
/// Surfaced to the user rather than hidden: a degraded element is still valid IFC, but it has
/// lost its editability downstream and the model author deserves to know
/// (`docs/ifc-semantics.md` §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepresentationKind {
    /// Exports as `IfcExtrudedAreaSolid` over `IfcArbitraryClosedProfileDef`, still parametric
    /// and still editable in Revit, Archicad, or Bonsai.
    NativeParametric,
    /// Exports as `IfcTriangulatedFaceSet`. Geometrically correct, semantically poorer.
    Tessellated,
}

impl RepresentationKind {
    pub fn is_degraded(self) -> bool {
        matches!(self, Self::Tessellated)
    }

    /// The IFC representation type this maps to.
    pub fn ifc_representation(self) -> &'static str {
        match self {
            Self::NativeParametric => "IfcExtrudedAreaSolid",
            Self::Tessellated => "IfcTriangulatedFaceSet",
        }
    }
}

/// How a family relates to the elements around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostBehavior {
    /// Placed anywhere. Furniture, equipment.
    Free,
    /// Placed on a storey. Columns, freestanding walls.
    LevelHosted,
    /// Placed in a host element. Doors, windows.
    WallHosted {
        /// Whether placement cuts an opening in the host (`IfcRelVoidsElement`). True for
        /// doors and windows; false for something surface-mounted like a wall light.
        cuts_host: bool,
    },
    /// Placed on a face without cutting it. Wall-mounted fixtures.
    FaceHosted,
}

impl HostBehavior {
    pub fn name(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::LevelHosted => "level-hosted",
            Self::WallHosted { .. } => "wall-hosted",
            Self::FaceHosted => "face-hosted",
        }
    }

    pub fn requires_host(self) -> bool {
        matches!(self, Self::WallHosted { .. } | Self::FaceHosted)
    }

    pub fn cuts_host(self) -> bool {
        matches!(self, Self::WallHosted { cuts_host: true })
    }
}

/// A named set of parameter overrides — Revit's "type", Archicad's parameter preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FamilyType {
    pub name: String,
    pub overrides: BTreeMap<String, ParamValue>,
}

impl FamilyType {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            overrides: BTreeMap::new(),
        }
    }

    pub fn set(mut self, name: impl Into<String>, value: ParamValue) -> Self {
        self.overrides.insert(name.into(), value);
        self
    }
}

/// How a family lands in IFC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IfcTypeMapping {
    pub entity: IfcClass,
    /// `PredefinedType`, e.g. `DOOR` / `SLIDING_TO_LEFT`.
    pub predefined_type: Option<String>,
    /// Property set family parameters are written into, e.g. `Pset_DoorCommon`.
    pub property_set: Option<String>,
}

impl IfcTypeMapping {
    pub fn new(entity: IfcClass) -> Self {
        Self {
            entity,
            predefined_type: None,
            property_set: None,
        }
    }

    pub fn with_predefined_type(mut self, value: impl Into<String>) -> Self {
        self.predefined_type = Some(value.into());
        self
    }

    pub fn with_property_set(mut self, value: impl Into<String>) -> Self {
        self.property_set = Some(value.into());
        self
    }
}

/// A reusable parametric component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FamilyDefinition {
    pub id: GlobalId,
    pub name: String,
    /// Bumped on any edit to parameters or recipe. Placed instances record the version they
    /// were flexed against, so a library update can report what needs re-evaluating.
    pub version: u32,
    pub parameters: Vec<ParamDef>,
    pub types: Vec<FamilyType>,
    pub recipe: GeometryRecipe,
    /// Geometry of the void this family cuts in its host. Required in practice for doors and
    /// windows: the opening is a real `IfcOpeningElement` with its own representation, not a
    /// hole subtracted from a wall mesh (`docs/ifc-semantics.md` §6.1).
    pub void_recipe: Option<GeometryRecipe>,
    pub host: HostBehavior,
    pub ifc_mapping: IfcTypeMapping,
}

/// What to place, where.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementRequest {
    /// Family type to place. `None` uses bare defaults.
    pub type_name: Option<String>,
    pub placement: Placement,
    /// Spatial container — usually the storey.
    pub container: Option<GlobalId>,
    /// Host element, for hosted families.
    pub host: Option<GlobalId>,
    /// Instance-scoped overrides.
    pub overrides: ParamBag,
}

impl PlacementRequest {
    pub fn new(placement: Placement) -> Self {
        Self {
            type_name: None,
            placement,
            container: None,
            host: None,
            overrides: ParamBag::new(),
        }
    }

    pub fn of_type(mut self, name: impl Into<String>) -> Self {
        self.type_name = Some(name.into());
        self
    }

    pub fn in_container(mut self, container: GlobalId) -> Self {
        self.container = Some(container);
        self
    }

    pub fn hosted_by(mut self, host: GlobalId) -> Self {
        self.host = Some(host);
        self
    }

    pub fn with_override(mut self, name: impl Into<String>, value: ParamValue) -> Self {
        self.overrides.insert(name, value);
        self
    }
}

/// The result of planning a placement: identities, resolved parameters, and the commands that
/// realise it.
///
/// Returning commands rather than mutating a model keeps the family system free of any
/// dependency on model state, and means a placement can be validated, previewed, permission-
/// checked, or replayed before anything changes.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementPlan {
    pub instance: GlobalId,
    /// The opening created for a host-cutting family.
    pub opening: Option<GlobalId>,
    pub params: ParamBag,
    pub representation: RepresentationKind,
    pub commands: Vec<ModelCommand>,
}

impl FamilyDefinition {
    pub fn new(
        name: impl Into<String>,
        ifc_mapping: IfcTypeMapping,
        parameters: Vec<ParamDef>,
        recipe: GeometryRecipe,
    ) -> Result<Self, FamilyError> {
        let mut seen = std::collections::BTreeSet::new();
        for p in &parameters {
            if !seen.insert(p.name.clone()) {
                return Err(FamilyError::DuplicateParameter(p.name.clone()));
            }
        }
        Ok(Self {
            id: GlobalId::new(),
            name: name.into(),
            version: 1,
            parameters,
            types: Vec::new(),
            recipe,
            void_recipe: None,
            host: HostBehavior::Free,
            ifc_mapping,
        })
    }

    pub fn with_type(mut self, family_type: FamilyType) -> Self {
        self.types.push(family_type);
        self
    }

    pub fn with_host(mut self, host: HostBehavior) -> Self {
        self.host = host;
        self
    }

    pub fn with_void_recipe(mut self, recipe: GeometryRecipe) -> Self {
        self.void_recipe = Some(recipe);
        self
    }

    pub fn parameter(&self, name: &str) -> Option<&ParamDef> {
        self.parameters.iter().find(|p| p.name == name)
    }

    pub fn family_type(&self, name: &str) -> Option<&FamilyType> {
        self.types.iter().find(|t| t.name == name)
    }

    pub fn type_names(&self) -> impl Iterator<Item = &str> {
        self.types.iter().map(|t| t.name.as_str())
    }

    /// How this family's geometry will export.
    pub fn representation_kind(&self) -> RepresentationKind {
        self.recipe.representation_kind()
    }

    /// Resolve parameters: defaults, then type overrides, then instance overrides.
    ///
    /// Every override is validated against its declaration, and instance overrides are
    /// refused for type-scoped parameters — which is the family-authoring mistake that
    /// otherwise shows up as "why did every door change?".
    pub fn resolve(
        &self,
        type_name: Option<&str>,
        instance_overrides: &ParamBag,
    ) -> Result<ParamBag, FamilyError> {
        let mut bag = ParamBag::new();
        for p in &self.parameters {
            bag.insert(p.name.clone(), p.default.clone());
        }

        if let Some(name) = type_name {
            let family_type = self
                .family_type(name)
                .ok_or_else(|| FamilyError::UnknownType(name.to_owned()))?;
            for (key, value) in &family_type.overrides {
                let def = self
                    .parameter(key)
                    .ok_or_else(|| FamilyError::UnknownParameter(key.clone()))?;
                def.validate(value)?;
                bag.insert(key.clone(), value.clone());
            }
        }

        for (key, value) in instance_overrides.iter() {
            let def = self
                .parameter(key)
                .ok_or_else(|| FamilyError::UnknownParameter(key.to_owned()))?;
            if def.scope != ParamScope::Instance {
                return Err(FamilyError::NotInstanceScoped(key.to_owned()));
            }
            def.validate(value)?;
            bag.insert(key.to_owned(), value.clone());
        }

        Ok(bag)
    }

    /// Flex the family and produce its mesh.
    pub fn evaluate(
        &self,
        type_name: Option<&str>,
        instance_overrides: &ParamBag,
        tess: &TessellationSettings,
        csg: &dyn CsgBackend,
    ) -> Result<IndexedMesh, FamilyError> {
        let params = self.resolve(type_name, instance_overrides)?;
        self.recipe.evaluate(&params, tess, csg)
    }

    /// Flex the family and produce its IFC-ready representation.
    ///
    /// Prefer this over [`FamilyDefinition::evaluate`] when the destination is a file: a
    /// single extrusion never gets tessellated on the way out.
    pub fn representation(
        &self,
        type_name: Option<&str>,
        instance_overrides: &ParamBag,
        tess: &TessellationSettings,
        csg: &dyn CsgBackend,
    ) -> Result<Representation, FamilyError> {
        let params = self.resolve(type_name, instance_overrides)?;
        self.recipe.to_representation(&params, tess, csg)
    }

    /// The representation of the void this family cuts, if it cuts one.
    pub fn void_representation(
        &self,
        type_name: Option<&str>,
        instance_overrides: &ParamBag,
        tess: &TessellationSettings,
        csg: &dyn CsgBackend,
    ) -> Result<Option<Representation>, FamilyError> {
        let Some(recipe) = &self.void_recipe else {
            return Ok(None);
        };
        let params = self.resolve(type_name, instance_overrides)?;
        Ok(Some(recipe.to_representation(&params, tess, csg)?))
    }

    /// Produce the geometry of the void this family cuts, if it cuts one.
    pub fn evaluate_void(
        &self,
        type_name: Option<&str>,
        instance_overrides: &ParamBag,
        tess: &TessellationSettings,
        csg: &dyn CsgBackend,
    ) -> Result<Option<IndexedMesh>, FamilyError> {
        let Some(recipe) = &self.void_recipe else {
            return Ok(None);
        };
        let params = self.resolve(type_name, instance_overrides)?;
        Ok(Some(recipe.evaluate(&params, tess, csg)?))
    }

    /// Plan a placement.
    ///
    /// For a host-cutting family this expands into the full IFC relationship set — an
    /// `IfcOpeningElement`, an `IfcRelVoidsElement` to the host, and an `IfcRelFillsElement`
    /// to the instance. That expansion is the difference between authoring a door and drawing
    /// a box in a hole.
    pub fn place(&self, request: &PlacementRequest) -> Result<PlacementPlan, FamilyError> {
        let params = self.resolve(request.type_name.as_deref(), &request.overrides)?;

        match (self.host.requires_host(), &request.host) {
            (true, None) => {
                return Err(FamilyError::HostRequired {
                    behavior: self.host.name(),
                })
            }
            (false, Some(_)) => {
                return Err(FamilyError::HostNotAllowed {
                    behavior: self.host.name(),
                })
            }
            _ => {}
        }

        let mut commands = Vec::new();

        // The opening is created first: the instance fills it, so it must already exist.
        let opening = if self.host.cuts_host() {
            let id = GlobalId::new();
            let mut element = ElementRecord::new(id.clone(), IfcClass::OpeningElement)
                .with_placement(request.placement);
            element.name = Some(format!("{} opening", self.name));
            element.object_type = request.type_name.clone();
            if let Some(container) = &request.container {
                element.container = Some(container.clone());
            }
            commands.push(ModelCommand::CreateElement {
                element: Box::new(element),
            });
            Some(id)
        } else {
            None
        };

        let instance_id = GlobalId::new();
        let mut instance = ElementRecord::new(instance_id.clone(), self.ifc_mapping.entity.clone())
            .with_placement(request.placement);
        instance.name = Some(self.name.clone());
        instance.object_type = request
            .type_name
            .clone()
            .or_else(|| Some(self.name.clone()));
        instance.type_ref = Some(self.id.clone());
        instance.container = request.container.clone();
        commands.push(ModelCommand::CreateElement {
            element: Box::new(instance),
        });

        if let (Some(opening), Some(host)) = (&opening, &request.host) {
            commands.push(ModelCommand::AddVoid {
                host: host.clone(),
                opening: opening.clone(),
            });
            commands.push(ModelCommand::AddFill {
                opening: opening.clone(),
                filler: instance_id.clone(),
            });
        }

        Ok(PlacementPlan {
            instance: instance_id,
            opening,
            params,
            representation: self.representation_kind(),
            commands,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::ParamType;
    use crate::recipe::{Expr, ProfileSpec};
    use cadforge_core::Model;
    use cadforge_geom::UnavailableCsg;
    use glam::DVec3;

    /// A door family, the way one would actually be authored: type-scoped leaf dimensions,
    /// an instance-scoped sill height, a named type, and a declared void.
    fn door_family() -> FamilyDefinition {
        let leaf = GeometryRecipe::single_extrusion(
            ProfileSpec::Rectangle {
                width: Expr::param("Width"),
                depth: Expr::param("Thickness"),
            },
            [0.0, 0.0, 1.0],
            Expr::param("Height"),
        )
        .unwrap();

        // The void is deliberately wider and taller than the leaf — a door needs frame
        // clearance, and this is exactly the detail a mesh-subtraction approach loses.
        let void = GeometryRecipe::single_extrusion(
            ProfileSpec::Rectangle {
                width: Expr::param("Width") + Expr::param("FrameClearance"),
                depth: Expr::Const(1.0),
            },
            [0.0, 0.0, 1.0],
            Expr::param("Height") + Expr::param("FrameClearance"),
        )
        .unwrap();

        FamilyDefinition::new(
            "Single Flush Door",
            IfcTypeMapping::new(IfcClass::Door)
                .with_predefined_type("DOOR")
                .with_property_set("Pset_DoorCommon"),
            vec![
                ParamDef::length("Width", 0.9).with_range(0.4, 2.4),
                ParamDef::length("Height", 2.1).with_range(1.2, 3.6),
                ParamDef::length("Thickness", 0.045),
                ParamDef::length("FrameClearance", 0.02),
                ParamDef::instance_length("SillHeight", 0.0),
                ParamDef::new(
                    "Finish",
                    ParamValue::Text("Painted".into()),
                    ParamScope::Type,
                ),
            ],
            leaf,
        )
        .unwrap()
        .with_host(HostBehavior::WallHosted { cuts_host: true })
        .with_void_recipe(void)
        .with_type(
            FamilyType::new("900 x 2100")
                .set("Width", ParamValue::Length(0.9))
                .set("Height", ParamValue::Length(2.1)),
        )
        .with_type(
            FamilyType::new("1200 x 2400")
                .set("Width", ParamValue::Length(1.2))
                .set("Height", ParamValue::Length(2.4)),
        )
    }

    #[test]
    fn defaults_resolve_when_no_type_is_named() {
        let family = door_family();
        let params = family.resolve(None, &ParamBag::new()).unwrap();
        assert_eq!(params.number("Width").unwrap(), 0.9);
        assert_eq!(params.len(), 6);
    }

    #[test]
    fn a_named_type_overrides_defaults() {
        let family = door_family();
        let params = family
            .resolve(Some("1200 x 2400"), &ParamBag::new())
            .unwrap();
        assert_eq!(params.number("Width").unwrap(), 1.2);
        assert_eq!(params.number("Height").unwrap(), 2.4);
        // Untouched by the type, so still the default.
        assert_eq!(params.number("Thickness").unwrap(), 0.045);
    }

    #[test]
    fn an_unknown_type_is_named_in_the_error() {
        let family = door_family();
        assert_eq!(
            family.resolve(Some("800 x 2000"), &ParamBag::new()),
            Err(FamilyError::UnknownType("800 x 2000".into()))
        );
    }

    #[test]
    fn instance_overrides_apply_only_to_instance_scoped_parameters() {
        let family = door_family();

        let mut ok = ParamBag::new();
        ok.insert("SillHeight", ParamValue::Length(0.15));
        let params = family.resolve(Some("900 x 2100"), &ok).unwrap();
        assert_eq!(params.number("SillHeight").unwrap(), 0.15);

        // Width is type-scoped: overriding per instance is the classic family bug.
        let mut bad = ParamBag::new();
        bad.insert("Width", ParamValue::Length(1.5));
        assert_eq!(
            family.resolve(Some("900 x 2100"), &bad),
            Err(FamilyError::NotInstanceScoped("Width".into()))
        );
    }

    #[test]
    fn overrides_are_validated_against_their_declaration() {
        let family = door_family();
        let mut too_wide = ParamBag::new();
        too_wide.insert("SillHeight", ParamValue::Text("high".into()));
        assert!(matches!(
            family.resolve(None, &too_wide),
            Err(FamilyError::TypeMismatch { .. })
        ));

        let bad_type = FamilyType::new("Oversized").set("Width", ParamValue::Length(9.0));
        let family = door_family().with_type(bad_type);
        assert!(matches!(
            family.resolve(Some("Oversized"), &ParamBag::new()),
            Err(FamilyError::OutOfRange { .. })
        ));
    }

    #[test]
    fn flexing_a_type_changes_the_geometry() {
        let family = door_family();
        let tess = TessellationSettings::standard();

        let small = family
            .evaluate(Some("900 x 2100"), &ParamBag::new(), &tess, &UnavailableCsg)
            .unwrap();
        let large = family
            .evaluate(
                Some("1200 x 2400"),
                &ParamBag::new(),
                &tess,
                &UnavailableCsg,
            )
            .unwrap();

        assert!((small.signed_volume() - 0.9 * 0.045 * 2.1).abs() < 1e-9);
        assert!((large.signed_volume() - 1.2 * 0.045 * 2.4).abs() < 1e-9);
        assert!(large.signed_volume() > small.signed_volume());
    }

    #[test]
    fn the_void_is_larger_than_the_leaf_it_frames() {
        let family = door_family();
        let tess = TessellationSettings::standard();
        let leaf = family
            .evaluate(Some("900 x 2100"), &ParamBag::new(), &tess, &UnavailableCsg)
            .unwrap();
        let void = family
            .evaluate_void(Some("900 x 2100"), &ParamBag::new(), &tess, &UnavailableCsg)
            .unwrap()
            .expect("a door declares its void");

        assert!(void.bounds().size().x > leaf.bounds().size().x);
        assert!(void.bounds().size().z > leaf.bounds().size().z);
    }

    #[test]
    fn a_door_exports_as_native_parametric_ifc() {
        let family = door_family();
        assert_eq!(
            family.representation_kind(),
            RepresentationKind::NativeParametric
        );
        assert!(!family.representation_kind().is_degraded());
        assert_eq!(
            family.representation_kind().ifc_representation(),
            "IfcExtrudedAreaSolid"
        );
    }

    #[test]
    fn placing_a_door_produces_the_full_ifc_relationship_set() {
        // The real proof: the plan applies cleanly to a live model and every relationship
        // lands where IFC expects it.
        let mut model = Model::new();
        let storey = ElementRecord::new(GlobalId::new(), IfcClass::BuildingStorey);
        let storey_id = storey.global_id.clone();
        let wall =
            ElementRecord::new(GlobalId::new(), IfcClass::Wall).with_container(storey_id.clone());
        let wall_id = wall.global_id.clone();
        model
            .apply_all([
                ModelCommand::CreateElement {
                    element: Box::new(storey.clone()),
                },
                ModelCommand::CreateElement {
                    element: Box::new(wall),
                },
            ])
            .unwrap();

        let family = door_family();
        let request = PlacementRequest::new(Placement::at(DVec3::new(2.0, 0.0, 0.0)))
            .of_type("900 x 2100")
            .in_container(storey_id.clone())
            .hosted_by(wall_id.clone())
            .with_override("SillHeight", ParamValue::Length(0.0));

        let plan = family.place(&request).unwrap();
        assert_eq!(plan.commands.len(), 4, "opening, door, void, fill");
        model.apply_all(plan.commands.clone()).unwrap();

        let opening = plan.opening.expect("a cutting family creates an opening");
        assert_eq!(model.host_of(&opening), Some(&wall_id));
        assert_eq!(
            model.fills_of(&opening).collect::<Vec<_>>(),
            vec![&plan.instance]
        );
        let door = model.get(&plan.instance).unwrap();
        assert_eq!(door.class, IfcClass::Door);
        assert_eq!(door.type_ref, Some(family.id.clone()));
        assert_eq!(door.container, Some(storey_id));
        assert_eq!(door.object_type.as_deref(), Some("900 x 2100"));
        assert_eq!(door.placement.location, DVec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn placing_a_door_is_fully_undoable() {
        let mut model = Model::new();
        let storey = ElementRecord::new(GlobalId::new(), IfcClass::BuildingStorey);
        let storey_id = storey.global_id.clone();
        let wall =
            ElementRecord::new(GlobalId::new(), IfcClass::Wall).with_container(storey_id.clone());
        let wall_id = wall.global_id.clone();
        model
            .apply_all([
                ModelCommand::CreateElement {
                    element: Box::new(storey),
                },
                ModelCommand::CreateElement {
                    element: Box::new(wall),
                },
            ])
            .unwrap();
        let before = model.len();

        let family = door_family();
        let plan = family
            .place(
                &PlacementRequest::new(Placement::identity())
                    .of_type("900 x 2100")
                    .in_container(storey_id)
                    .hosted_by(wall_id.clone()),
            )
            .unwrap();
        let count = plan.commands.len();
        model.apply_all(plan.commands).unwrap();
        assert_eq!(model.len(), before + 2);

        for _ in 0..count {
            model.undo().unwrap();
        }
        assert_eq!(model.len(), before);
        assert_eq!(model.openings_of(&wall_id).count(), 0);
    }

    #[test]
    fn hosting_rules_are_enforced_at_placement_time() {
        let family = door_family();
        // Wall-hosted with no host.
        assert_eq!(
            family.place(&PlacementRequest::new(Placement::identity()).of_type("900 x 2100")),
            Err(FamilyError::HostRequired {
                behavior: "wall-hosted"
            })
        );

        // Free-standing with a host.
        let chair = FamilyDefinition::new(
            "Chair",
            IfcTypeMapping::new(IfcClass::Furniture),
            vec![ParamDef::length("Width", 0.5)],
            GeometryRecipe::single_extrusion(
                ProfileSpec::Rectangle {
                    width: Expr::param("Width"),
                    depth: Expr::param("Width"),
                },
                [0.0, 0.0, 1.0],
                Expr::Const(0.45),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            chair.place(&PlacementRequest::new(Placement::identity()).hosted_by(GlobalId::new())),
            Err(FamilyError::HostNotAllowed { behavior: "free" })
        );
    }

    #[test]
    fn a_non_cutting_hosted_family_creates_no_opening() {
        let light = FamilyDefinition::new(
            "Wall Light",
            IfcTypeMapping::new(IfcClass::BuildingElementProxy),
            vec![ParamDef::length("Radius", 0.08)],
            GeometryRecipe::single_extrusion(
                ProfileSpec::Circle {
                    radius: Expr::param("Radius"),
                },
                [0.0, 1.0, 0.0],
                Expr::Const(0.1),
            )
            .unwrap(),
        )
        .unwrap()
        .with_host(HostBehavior::FaceHosted);

        let plan = light
            .place(&PlacementRequest::new(Placement::identity()).hosted_by(GlobalId::new()))
            .unwrap();
        assert!(plan.opening.is_none());
        assert_eq!(plan.commands.len(), 1, "just the instance");
    }

    #[test]
    fn duplicate_parameters_are_rejected_at_definition_time() {
        let recipe = GeometryRecipe::single_extrusion(
            ProfileSpec::Rectangle {
                width: Expr::Const(1.0),
                depth: Expr::Const(1.0),
            },
            [0.0, 0.0, 1.0],
            Expr::Const(1.0),
        )
        .unwrap();
        assert_eq!(
            FamilyDefinition::new(
                "Broken",
                IfcTypeMapping::new(IfcClass::Furniture),
                vec![
                    ParamDef::length("Width", 1.0),
                    ParamDef::length("Width", 2.0)
                ],
                recipe,
            )
            .err(),
            Some(FamilyError::DuplicateParameter("Width".into()))
        );
    }

    #[test]
    fn types_are_enumerable_for_a_type_picker() {
        let family = door_family();
        assert_eq!(
            family.type_names().collect::<Vec<_>>(),
            ["900 x 2100", "1200 x 2400"]
        );
        assert_eq!(
            family.parameter("Width").unwrap().param_type,
            ParamType::Length
        );
    }
}
