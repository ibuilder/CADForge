//! The demo room, as a model.
//!
//! Four walls and a hosted door — the same scene the headless demo authors, built here so the
//! viewport has something to show when it is not given a file. Everything goes through
//! `ModelCommand`, because nothing else may touch the store.

use anyhow::Result;
use cadforge_core::{
    ElementRecord, GlobalId, IfcClass, Model, ModelCommand, Placement, Representation,
};
use cadforge_family::{
    Expr, FamilyDefinition, FamilyType, GeometryRecipe, HostBehavior, IfcTypeMapping, ParamBag,
    ParamDef, ParamValue, PlacementRequest, ProfileSpec,
};
use cadforge_geom::{BspCsg, TessellationSettings};
use glam::DVec3;

const WALL_HEIGHT: f64 = 3.0;
const WALL_THICKNESS: f64 = 0.2;

pub fn build(model: &mut Model) -> Result<()> {
    let storey =
        ElementRecord::new(GlobalId::new(), IfcClass::BuildingStorey).with_name("Level 00");
    let storey_id = storey.global_id.clone();
    model.apply(ModelCommand::CreateElement {
        element: Box::new(storey),
    })?;

    // An 8 × 5 m room.
    let corners = [
        DVec3::new(0.0, 0.0, 0.0),
        DVec3::new(8.0, 0.0, 0.0),
        DVec3::new(8.0, 5.0, 0.0),
        DVec3::new(0.0, 5.0, 0.0),
    ];

    let mut first_wall = None;
    for i in 0..corners.len() {
        let (start, end) = (corners[i], corners[(i + 1) % corners.len()]);
        let axis = end - start;
        let length = axis.length();

        let id = GlobalId::new();
        let element = ElementRecord::new(id.clone(), IfcClass::Wall)
            .with_name(format!("W-{:02}", i + 1))
            .with_placement(Placement::new(start + axis * 0.5, DVec3::Z, axis))
            .with_container(storey_id.clone())
            .with_representation(Representation::extrusion(
                rectangle(length, WALL_THICKNESS),
                [0.0, 0.0, 1.0],
                WALL_HEIGHT,
            ));
        model.apply(ModelCommand::CreateElement {
            element: Box::new(element),
        })?;
        first_wall.get_or_insert(id);
    }

    // A door in the first wall, placed through the family system so the opening, the void,
    // and the fill are all real relationships rather than implied by geometry.
    let host = first_wall.expect("four walls were just created");
    let door = door_family()?;
    let plan = door.place(
        &PlacementRequest::new(Placement::new(
            DVec3::new(2.0, 0.0, 0.0),
            DVec3::Z,
            DVec3::X,
        ))
        .of_type("900 x 2100")
        .in_container(storey_id)
        .hosted_by(host),
    )?;
    model.apply_all(plan.commands)?;

    let tess = TessellationSettings::standard();
    let csg = BspCsg::new();
    for (id, representation) in [
        (
            plan.instance.clone(),
            door.representation(Some("900 x 2100"), &ParamBag::new(), &tess, &csg)?,
        ),
        (
            plan.opening.clone().expect("a door cuts an opening"),
            door.void_representation(Some("900 x 2100"), &ParamBag::new(), &tess, &csg)?
                .expect("this family declares a void"),
        ),
    ] {
        model.apply(ModelCommand::SetRepresentation {
            global_id: id,
            representation: Some(Box::new(representation)),
        })?;
    }

    Ok(())
}

fn rectangle(width: f64, depth: f64) -> Vec<[f64; 2]> {
    let (w, d) = (width * 0.5, depth * 0.5);
    vec![[-w, -d], [w, -d], [w, d], [-w, d]]
}

fn door_family() -> Result<FamilyDefinition> {
    let leaf = GeometryRecipe::single_extrusion(
        ProfileSpec::Rectangle {
            width: Expr::param("Width"),
            depth: Expr::param("Thickness"),
        },
        [0.0, 0.0, 1.0],
        Expr::param("Height"),
    )?;
    let void = GeometryRecipe::single_extrusion(
        ProfileSpec::Rectangle {
            width: Expr::param("Width") + Expr::param("FrameClearance"),
            depth: Expr::Const(WALL_THICKNESS * 1.5),
        },
        [0.0, 0.0, 1.0],
        Expr::param("Height") + Expr::param("FrameClearance"),
    )?;

    Ok(FamilyDefinition::new(
        "Single Flush Door",
        IfcTypeMapping::new(IfcClass::Door).with_predefined_type("DOOR"),
        vec![
            ParamDef::length("Width", 0.9).with_range(0.4, 2.4),
            ParamDef::length("Height", 2.1).with_range(1.2, 3.6),
            ParamDef::length("Thickness", 0.045),
            ParamDef::length("FrameClearance", 0.02),
        ],
        leaf,
    )?
    .with_host(HostBehavior::WallHosted { cuts_host: true })
    .with_void_recipe(void)
    .with_type(
        FamilyType::new("900 x 2100")
            .set("Width", ParamValue::Length(0.9))
            .set("Height", ParamValue::Length(2.1)),
    ))
}
