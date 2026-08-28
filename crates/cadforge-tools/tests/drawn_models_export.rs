//! Drawing a room by hand and getting a valid IFC file out of it.
//!
//! Everything in `draft.rs` is tested against the model. This is tested against the file,
//! which is a different question: a tool can produce an element the command model accepts and
//! the exporter still refuses, or one that exports and comes back as something else. The unit
//! bug in Phase 3b is the standing reminder that a system agreeing with itself proves nothing.

use cadforge_core::{ElementRecord, GlobalId, IfcClass, Model, ModelCommand, Placement};
use cadforge_ifc::{ExportContext, IfcBackend, IfcLiteBackend, IfcSchema, SpfBackend};
use cadforge_tools::{snap_candidates, ContainerRef, Draft, DraftOutcome, Tool};
use glam::DVec3;

/// A project, a site, a building, and a storey at `elevation`. Returns the storey.
fn spatial_structure(model: &mut Model, elevation: f64) -> GlobalId {
    let mut parent: Option<GlobalId> = None;
    let mut storey = None;
    for (class, height) in [
        (IfcClass::Project, 0.0),
        (IfcClass::Site, 0.0),
        (IfcClass::Building, 0.0),
        (IfcClass::BuildingStorey, elevation),
    ] {
        let id = GlobalId::new();
        let mut element = ElementRecord::new(id.clone(), class.clone());
        element.name = Some(class.ifc_name().to_string());
        element.placement = Placement::at(DVec3::new(0.0, 0.0, height));
        element.container = parent.clone();
        model
            .apply(ModelCommand::CreateElement {
                element: Box::new(element),
            })
            .unwrap();
        if class == IfcClass::BuildingStorey {
            storey = Some(id.clone());
        }
        parent = Some(id);
    }
    storey.expect("the storey was just created")
}

/// Draw a closed rectangle of walls, snapping each one to the last, plus a floor slab.
fn draw_a_room(model: &mut Model, storey: GlobalId, elevation: f64) -> usize {
    let mut draft = Draft::default();
    draft.settings.elevation = elevation;
    draft.container = Some(ContainerRef::new(storey, DVec3::new(0.0, 0.0, elevation)));

    let corners = [
        DVec3::new(0.0, 0.0, elevation),
        DVec3::new(6.0, 0.0, elevation),
        DVec3::new(6.0, 4.0, elevation),
        DVec3::new(0.0, 4.0, elevation),
    ];

    draft.set_tool(Tool::Wall);
    let mut drawn = 0;
    for pair in 0..corners.len() {
        let (from, to) = (corners[pair], corners[(pair + 1) % corners.len()]);
        for target in [from, to] {
            // Aim a couple of centimetres off every time, exactly as a hand does. If snapping
            // is not doing its job the walls will not meet and the test will say so.
            let sloppy = target + DVec3::new(0.02, -0.015, 0.0);
            let candidates = snap_candidates(model, sloppy, 0.2);
            if let DraftOutcome::Commit { commands, .. } = draft.click(sloppy, &candidates).unwrap()
            {
                model.apply_all(commands).unwrap();
                drawn += 1;
            }
        }
    }
    assert_eq!(drawn, 4, "four clicks-pairs, four walls");

    draft.set_tool(Tool::Slab);
    for corner in corners {
        draft.click(corner, &[]).unwrap();
    }
    let DraftOutcome::Commit { commands, .. } = draft.finish().unwrap() else {
        panic!("the slab should close");
    };
    model.apply_all(commands).unwrap();
    drawn + 1
}

#[test]
fn a_hand_drawn_room_exports_as_native_parametric_ifc() {
    let elevation = 3.0;
    let mut model = Model::new();
    let storey = spatial_structure(&mut model, elevation);
    let drawn = draw_a_room(&mut model, storey, elevation);
    assert_eq!(drawn, 5);

    let bytes = SpfBackend::new(ExportContext::named("Drawn Room"))
        .write(&model, IfcSchema::Ifc4)
        .expect("export");
    let text = String::from_utf8(bytes.clone()).expect("IFC is ASCII text");

    // Swept solids, not meshes. A tool that emitted triangles would look identical on screen
    // and arrive in Revit or Archicad as something nobody can edit.
    assert_eq!(text.matches("IFCEXTRUDEDAREASOLID").count(), 5);
    assert!(
        !text.contains("IFCTRIANGULATEDFACESET"),
        "nothing drawn should degrade to tessellation"
    );
    assert_eq!(text.matches("IFCWALL(").count(), 4);
    assert_eq!(text.matches("IFCSLAB(").count(), 1);

    // Every drawn element has to be filed in the storey, or it exports into nothing.
    assert_eq!(text.matches("IFCRELCONTAINEDINSPATIALSTRUCTURE").count(), 1);
}

#[test]
fn drawn_walls_actually_meet() {
    // The point of snapping, stated as a measurement rather than a feeling. Every click was
    // aimed 25 mm off; if the corners are not exact, the room has gaps that survive into
    // quantities and cost someone money.
    let mut model = Model::new();
    let storey = spatial_structure(&mut model, 0.0);
    draw_a_room(&mut model, storey, 0.0);

    let mut ends: Vec<DVec3> = Vec::new();
    for wall in model.by_class(&IfcClass::Wall) {
        let transform = cadforge_tools::world_transform(&model, wall.global_id.clone());
        let Some(cadforge_core::Representation::ExtrudedAreaSolid { profile, .. }) =
            &wall.representation
        else {
            panic!("walls are swept solids");
        };
        let length = profile.iter().map(|p| p[0]).fold(f64::MIN, f64::max);
        ends.push(transform.transform_point3(DVec3::ZERO));
        ends.push(transform.transform_point3(DVec3::new(length, 0.0, 0.0)));
    }

    // Eight endpoints over four corners: each corner must be shared exactly, not nearly.
    for corner in [
        DVec3::new(0.0, 0.0, 0.0),
        DVec3::new(6.0, 0.0, 0.0),
        DVec3::new(6.0, 4.0, 0.0),
        DVec3::new(0.0, 4.0, 0.0),
    ] {
        let meeting = ends.iter().filter(|e| e.distance(corner) < 1e-9).count();
        assert_eq!(
            meeting, 2,
            "two wall ends should land exactly on {corner:?}, found {meeting}"
        );
    }
}

#[test]
fn a_drawn_room_survives_a_round_trip() {
    let mut model = Model::new();
    let storey = spatial_structure(&mut model, 0.0);
    draw_a_room(&mut model, storey, 0.0);

    let first = SpfBackend::new(ExportContext::named("Drawn Room"))
        .write(&model, IfcSchema::Ifc4)
        .expect("export");
    // Read back with `ifc-lite`, not with our own writer — the writer cannot read, and one
    // that parsed its own output would only prove it agrees with itself.
    let mut reimported = Model::new();
    IfcLiteBackend::new()
        .read(&first, &mut reimported)
        .expect("reimport");

    assert_eq!(reimported.by_class(&IfcClass::Wall).count(), 4);
    assert_eq!(reimported.by_class(&IfcClass::Slab).count(), 1);

    let second = SpfBackend::new(ExportContext::named("Drawn Room"))
        .write(&reimported, IfcSchema::Ifc4)
        .expect("re-export");
    assert_eq!(
        String::from_utf8(first)
            .unwrap()
            .matches("IFCEXTRUDEDAREASOLID")
            .count(),
        String::from_utf8(second)
            .unwrap()
            .matches("IFCEXTRUDEDAREASOLID")
            .count(),
        "geometry must not be lost or duplicated on the way through"
    );
}

#[test]
fn drawing_is_fully_undoable() {
    // Not a property of the tools — a property of routing every edit through the command
    // model. Stated as a test so a future tool that reaches past `ModelCommand` breaks it.
    let mut model = Model::new();
    let storey = spatial_structure(&mut model, 0.0);
    let before = model.len();
    let drawn = draw_a_room(&mut model, storey, 0.0);
    assert_eq!(model.len(), before + drawn);

    for _ in 0..drawn {
        model.undo().expect("every drawn element undoes");
    }
    assert_eq!(model.len(), before);

    for _ in 0..drawn {
        model.redo().expect("and redoes");
    }
    assert_eq!(model.len(), before + drawn);
    assert!(SpfBackend::new(ExportContext::default())
        .write(&model, IfcSchema::Ifc4)
        .is_ok());
}
