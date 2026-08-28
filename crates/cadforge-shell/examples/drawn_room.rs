//! Draw a small building with the authoring tools, then photograph and export it.
//!
//! Phase 5's claim, run end to end without a window: every element here is produced by the
//! same `Draft` the viewport drives, from clicks that are all deliberately a couple of
//! centimetres off. Snapping is what turns those into a room whose corners meet exactly, and
//! the export is what proves the result is real IFC rather than something that only looks
//! right on screen.
//!
//! The drawing and the export need no GPU, so this runs in CI and its output goes through
//! IfcOpenShell like the demo model does. The picture is the only part that needs hardware.
//!
//!     cargo run --release -p cadforge-shell --example drawn_room
//!     cargo run --release -p cadforge-shell --features gpu --example drawn_room

use anyhow::{Context, Result};
use cadforge_core::{ElementRecord, GlobalId, IfcClass, Model, ModelCommand, Representation};
use cadforge_ifc::{ExportContext, IfcBackend, IfcSchema, SpfBackend};
use cadforge_tools::{snap_candidates, world_transform, ContainerRef, Draft, DraftOutcome, Tool};
use glam::DVec3;

/// How far off every click is aimed. A hand does not hit a coordinate.
const SLOP: DVec3 = DVec3::new(0.023, -0.017, 0.0);

fn main() -> Result<()> {
    println!("A room, drawn with the tools\n");

    let mut model = Model::new();
    let storey = spatial_structure(&mut model)?;
    let mut draft = Draft::default();
    draft.container = Some(ContainerRef::new(storey, DVec3::ZERO));

    // The outline. Every wall starts and ends with a sloppy click; the second click of each
    // wall and the first of the next are aimed at the same corner from different directions.
    let plan = [
        DVec3::new(0.0, 0.0, 0.0),
        DVec3::new(7.0, 0.0, 0.0),
        DVec3::new(7.0, 5.0, 0.0),
        DVec3::new(0.0, 5.0, 0.0),
    ];

    draft.set_tool(Tool::Slab);
    for corner in plan {
        draft.click(corner + SLOP, &[])?;
    }
    commit(&mut model, draft.finish()?)?;

    draft.set_tool(Tool::Wall);
    for i in 0..plan.len() {
        for corner in [plan[i], plan[(i + 1) % plan.len()]] {
            let target = corner + SLOP;
            let candidates = snap_candidates(&model, target, 0.3);
            commit(&mut model, draft.click(target, &candidates)?)?;
        }
    }

    // Two columns, placed on the grid rather than against anything.
    draft.set_tool(Tool::Column);
    for x in [2.5, 4.5] {
        commit(
            &mut model,
            draft.click(DVec3::new(x, 2.5, 0.0) + SLOP, &[])?,
        )?;
    }

    println!(
        "drew                {} walls, {} slab, {} columns — every click {:.0} mm off",
        model.by_class(&IfcClass::Wall).count(),
        model.by_class(&IfcClass::Slab).count(),
        model.by_class(&IfcClass::Column).count(),
        SLOP.length() * 1000.0
    );

    // The measurement, not the impression. Eight wall ends over four corners: if snapping did
    // its job each corner is shared exactly, and if it did not the room has gaps that survive
    // all the way into a quantity take-off.
    let mut exact = 0;
    for corner in plan {
        let meeting = wall_ends(&model)
            .iter()
            .filter(|end| end.distance(corner) < 1e-9)
            .count();
        anyhow::ensure!(
            meeting == 2,
            "two wall ends should land exactly on {corner:?}, found {meeting}"
        );
        exact += meeting;
    }
    println!("corners             {exact} wall ends meet exactly, to the nanometre");

    let ifc = std::path::Path::new("out/drawn.ifc");
    std::fs::create_dir_all("out")?;
    let bytes = SpfBackend::new(ExportContext::named("Drawn Room"))
        .write(&model, IfcSchema::Ifc4)
        .context("exporting")?;
    std::fs::write(ifc, &bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    println!(
        "exported            {} — {} swept solids, {} tessellated",
        ifc.display(),
        text.matches("IFCEXTRUDEDAREASOLID").count(),
        text.matches("IFCTRIANGULATEDFACESET").count()
    );

    render(&model)?;

    // Undo everything, because a drawn edit is an ordinary command and nothing about the
    // tools is allowed to be special.
    let drawn = model.len();
    while model.can_undo() {
        model.undo()?;
    }
    println!(
        "undo                {drawn} elements down to {}, then back up",
        model.len()
    );
    while model.can_redo() {
        model.redo()?;
    }
    anyhow::ensure!(model.len() == drawn, "redo did not restore the room");
    Ok(())
}

/// Photograph the model, when there is hardware to do it with.
#[cfg(feature = "gpu")]
fn render(model: &Model) -> Result<()> {
    use cadforge_core::BoundingBox;
    use cadforge_render::{Camera, FragmentId, MeshData, Renderer};

    let (width, height) = (1400u32, 900u32);
    let renderer = Renderer::new_headless(width, height)?;
    let meshes = meshes_of(model);

    // From the meshes, not from `Model::bounds()`. That reads a per-element cache which is
    // populated by whoever evaluates geometry, and nothing here does — framing it gives an
    // empty box and a camera pointing at nothing.
    let bounds = meshes.iter().fold(BoundingBox::empty(), |all, (mesh, _)| {
        all.union(mesh.bounds())
    });

    let mut camera = Camera::default();
    camera.set_viewport(width, height);
    camera.yaw = -2.1;
    camera.pitch = 0.92;
    camera.frame(&bounds);

    let data: Vec<MeshData<'_>> = meshes
        .iter()
        .map(|(mesh, color)| MeshData {
            positions: &mesh.positions,
            normals: &mesh.normals,
            indices: &mesh.indices,
            color: *color,
            id: FragmentId::NONE,
        })
        .collect();

    let png = std::path::Path::new("site/assets/drawn.png");
    renderer.render_to_png(&data, &camera, png)?;
    println!("gpu                 {}", renderer.adapter_description());
    println!("wrote               {} at {width}×{height}", png.display());
    Ok(())
}

#[cfg(not(feature = "gpu"))]
fn render(_model: &Model) -> Result<()> {
    println!("render              skipped; re-run with --features gpu for the picture");
    Ok(())
}

fn commit(model: &mut Model, outcome: DraftOutcome) -> Result<()> {
    if let DraftOutcome::Commit { commands, .. } = outcome {
        model.apply_all(commands)?;
    }
    Ok(())
}

/// Project → site → building → storey. Returns the storey.
fn spatial_structure(model: &mut Model) -> Result<GlobalId> {
    let mut parent: Option<GlobalId> = None;
    let mut storey = None;
    for class in [
        IfcClass::Project,
        IfcClass::Site,
        IfcClass::Building,
        IfcClass::BuildingStorey,
    ] {
        let id = GlobalId::new();
        let mut element = ElementRecord::new(id.clone(), class.clone());
        element.name = Some(class.ifc_name().to_string());
        element.container = parent.clone();
        model.apply(ModelCommand::CreateElement {
            element: Box::new(element),
        })?;
        if class == IfcClass::BuildingStorey {
            storey = Some(id.clone());
        }
        parent = Some(id);
    }
    storey.context("the storey was just created")
}

/// The world-space endpoints of every wall centreline.
fn wall_ends(model: &Model) -> Vec<DVec3> {
    let mut ends = Vec::new();
    for wall in model.by_class(&IfcClass::Wall) {
        let Some(Representation::ExtrudedAreaSolid { profile, .. }) = &wall.representation else {
            continue;
        };
        let transform = world_transform(model, wall.global_id.clone());
        let length = profile.iter().map(|p| p[0]).fold(f64::MIN, f64::max);
        ends.push(transform.transform_point3(DVec3::ZERO));
        ends.push(transform.transform_point3(DVec3::new(length, 0.0, 0.0)));
    }
    ends
}

#[cfg(feature = "gpu")]
fn meshes_of(model: &Model) -> Vec<(cadforge_geom::IndexedMesh, [f32; 3])> {
    use cadforge_geom::{extrude_along, Profile};
    use glam::DVec2;

    let mut meshes = Vec::new();
    for element in model.iter() {
        let Some(Representation::ExtrudedAreaSolid {
            profile,
            direction,
            depth,
        }) = &element.representation
        else {
            continue;
        };
        let Ok(profile) = Profile::new(profile.iter().map(|p| DVec2::new(p[0], p[1]))) else {
            continue;
        };
        let Ok(local) = extrude_along(&profile, DVec3::from_array(*direction), *depth) else {
            continue;
        };
        let color = match element.class {
            IfcClass::Wall => [0.78, 0.76, 0.72],
            IfcClass::Slab => [0.55, 0.56, 0.58],
            _ => [0.45, 0.52, 0.62],
        };
        meshes.push((
            local.transformed(world_transform(model, element.global_id.clone())),
            color,
        ));
    }
    meshes
}
