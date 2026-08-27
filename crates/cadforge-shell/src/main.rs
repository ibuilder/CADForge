//! CADForge demo pipeline.
//!
//! Phase 3 replaces this with a `winit` window and a `wgpu` renderer (PLAN.md §7). Until
//! then it exists to prove the crate boundaries actually hold, by driving the whole stack
//! end to end:
//!
//! 1. Author a spatial structure and four walls through commands.
//! 2. Define a parametric door family and place it in a wall — which expands into a real
//!    `IfcOpeningElement`, an `IfcRelVoidsElement`, and an `IfcRelFillsElement`.
//! 3. Evaluate every recipe to a mesh and build render fragments.
//! 4. Frame a camera, cull against the frustum, and resolve a simulated GPU pick back to a
//!    `GlobalId`.
//! 5. Undo the entire session and confirm the model returns to empty.
//! 6. Write the result as OBJ.
//!
//! Nothing here is a mock. Every step calls the same code the real application will.

use anyhow::{Context, Result};
use cadforge_core::{
    BoundingBox, ElementRecord, GlobalId, IfcClass, Model, ModelCommand, Placement, Representation,
};
use cadforge_family::{
    Expr, FamilyDefinition, FamilyType, GeometryRecipe, HostBehavior, IfcTypeMapping, ParamBag,
    ParamDef, ParamValue, PlacementRequest, ProfileSpec,
};
use cadforge_geom::{extrude, BspCsg, CsgBackend, IndexedMesh, Profile, TessellationSettings};
use cadforge_ifc::{ExportContext, ExportedType, IfcBackend, IfcSchema, SpfBackend};
use cadforge_render::{Camera, FragmentId, FragmentSet, Frustum, GeometrySource};
use glam::DVec3;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const WALL_HEIGHT: f64 = 3.0;
const WALL_THICKNESS: f64 = 0.2;

/// One evaluated element: its identity, its geometry in family-local space, and the same
/// geometry placed in the world.
///
/// Both are kept deliberately. Bounds and drawing need world space, but the **geometry hash
/// must be taken in local space** — otherwise two identical walls in different positions hash
/// differently and the renderer uploads the same mesh twice. That distinction is the whole
/// basis of instancing.
struct Piece {
    id: GlobalId,
    local: IndexedMesh,
    world: IndexedMesh,
}

fn main() -> Result<()> {
    println!("CADForge {} — demo pipeline\n", env!("CARGO_PKG_VERSION"));

    let tess = TessellationSettings::standard();
    let csg = BspCsg::new();
    let mut model = Model::new();

    // ---- 1. spatial structure -------------------------------------------------------
    let storey = create(&mut model, IfcClass::BuildingStorey, "Level 00", None)?;
    println!("spatial structure   IfcBuildingStorey \"Level 00\"  {storey}");

    // ---- 2. walls -------------------------------------------------------------------
    // A 8 × 5 m room. Each wall is a rectangle profile swept vertically — which is exactly
    // IfcExtrudedAreaSolid, so this exports as native parametric IFC.
    let corners = [
        DVec3::new(0.0, 0.0, 0.0),
        DVec3::new(8.0, 0.0, 0.0),
        DVec3::new(8.0, 5.0, 0.0),
        DVec3::new(0.0, 5.0, 0.0),
    ];

    let mut pieces: Vec<Piece> = Vec::new();
    let mut walls = Vec::new();
    for i in 0..corners.len() {
        let (start, end) = (corners[i], corners[(i + 1) % corners.len()]);
        let piece = build_wall(&mut model, &storey, start, end, i)?;
        walls.push(piece.id.clone());
        pieces.push(piece);
    }
    let total_wall_volume: f64 = pieces.iter().map(|p| p.world.signed_volume()).sum();
    println!(
        "walls               {} authored, {:.2} m³ uncut, {} triangles",
        walls.len(),
        total_wall_volume,
        pieces
            .iter()
            .map(|p| p.world.triangle_count())
            .sum::<usize>()
    );

    // ---- 3. a parametric family -----------------------------------------------------
    let door = door_family()?;
    println!(
        "\nfamily              \"{}\"  {}  types: {}",
        door.name,
        door.representation_kind().ifc_representation(),
        door.type_names().collect::<Vec<_>>().join(", ")
    );

    let host = walls[0].clone();
    let plan = door.place(
        &PlacementRequest::new(Placement::new(
            DVec3::new(2.0, 0.0, 0.0),
            DVec3::Z,
            DVec3::X,
        ))
        .of_type("900 x 2100")
        .in_container(storey.clone())
        .hosted_by(host.clone())
        .with_override("SillHeight", ParamValue::Length(0.0)),
    )?;

    println!(
        "placement           {} commands: opening, instance, IfcRelVoidsElement, IfcRelFillsElement",
        plan.commands.len()
    );
    model.apply_all(plan.commands)?;

    // The relationships are real, not implied by geometry.
    let opening = plan.opening.clone().context("a door must cut an opening")?;
    println!(
        "  host              {host} ← voided by {opening}\n  opening           {opening} ← filled by {}",
        plan.instance
    );

    let leaf = door.evaluate(Some("900 x 2100"), &ParamBag::new(), &tess, &csg)?;
    let void = door
        .evaluate_void(Some("900 x 2100"), &ParamBag::new(), &tess, &csg)?
        .context("this family declares a void")?;
    println!(
        "  leaf              {:.3} m³, void {:.3} m³ (frame clearance is real, not implied)",
        leaf.signed_volume(),
        void.signed_volume()
    );

    // Native parametric representations, taken from the recipe without tessellating.
    for (id, representation) in [
        (
            plan.instance.clone(),
            door.representation(Some("900 x 2100"), &ParamBag::new(), &tess, &csg)?,
        ),
        (
            opening.clone(),
            door.void_representation(Some("900 x 2100"), &ParamBag::new(), &tess, &csg)?
                .context("this family declares a void")?,
        ),
    ] {
        model.apply(ModelCommand::SetRepresentation {
            global_id: id,
            representation: Some(Box::new(representation)),
        })?;
    }

    for (id, local) in [(plan.instance.clone(), leaf), (opening.clone(), void)] {
        let world = local.transformed(
            model
                .get(&id)
                .context("element was just created")?
                .placement
                .to_matrix(),
        );
        model.set_bounds(&id, Some(world.bounds()));
        pieces.push(Piece { id, local, world });
    }

    // ---- 3b. cut the host --------------------------------------------------------------
    // Until a CSG backend existed, the exported IFC carried a real IfcRelVoidsElement while
    // our own meshes stayed uncut — the file was right and the viewer was behind it. Now the
    // opening is subtracted here too, so both agree.
    let void_world = pieces
        .iter()
        .find(|p| p.id == opening)
        .map(|p| p.world.clone())
        .context("the opening has geometry")?;

    if let Some(index) = pieces.iter().position(|p| p.id == host) {
        let before = pieces[index].world.signed_volume();
        let cut = csg.difference(&pieces[index].world, &void_world)?;
        let placement = model.get(&host).context("host exists")?.placement;

        println!(
            "csg                 {} backend: W-01 {:.3} → {:.3} m³ (−{:.3})",
            csg.name(),
            before,
            cut.signed_volume(),
            before - cut.signed_volume()
        );

        model.set_bounds(&host, Some(cut.bounds()));
        // Re-derive local geometry so the hash reflects the cut: this wall is no longer
        // identical to the one opposite it, and instancing must stop pretending it is.
        pieces[index].local = cut.transformed(placement.to_matrix().inverse());
        pieces[index].world = cut;
    }

    // Flexing the type changes the geometry, and nothing else has to know.
    let large = door.evaluate(Some("1200 x 2400"), &ParamBag::new(), &tess, &csg)?;
    println!(
        "  flex to 1200x2400 {:.3} m³ — same recipe, different type",
        large.signed_volume()
    );

    // ---- 4. render fragments --------------------------------------------------------
    let mut fragments = FragmentSet::new();
    for piece in &pieces {
        let element = model.get(&piece.id).context("element exists")?;
        fragments.insert(
            piece.id.clone(),
            GeometrySource {
                // Local space, so identical elements share a hash and one GPU buffer.
                hash: geometry_hash(&piece.local),
                vertex_count: piece.world.vertex_count() as u32,
                index_count: piece.world.indices.len() as u32,
            },
            material_for(&element.class),
            piece.world.bounds(),
            element.representation_revision,
        );
    }
    println!(
        "\nfragments           {} fragments, {} distinct geometries ({} uploads saved), {} triangles",
        fragments.len(),
        fragments.distinct_geometries(),
        fragments.len() - fragments.distinct_geometries(),
        fragments.total_triangles()
    );

    // ---- 5. camera, culling, picking ------------------------------------------------
    let mut camera = Camera::default();
    camera.set_viewport(1920, 1080);
    camera.frame(&fragments.bounds());
    let frustum = Frustum::from_view_projection(camera.view_projection());
    let visible = fragments.visible(|b| frustum.intersects(b));
    println!(
        "camera              framed at {:.1} m, {} of {} fragments visible",
        camera.distance,
        visible.len(),
        fragments.len()
    );

    // Simulate the GPU pick round trip: fragment id → RGBA8 → back → GlobalId.
    let target = fragments
        .fragments_of(&plan.instance)
        .first()
        .copied()
        .context("the door has a fragment")?;
    let picked = FragmentId::from_pick_color(target.to_pick_color());
    let picked_element = fragments.element_at(picked).context("pick resolves")?;
    let picked_record = model.get(picked_element).context("picked element exists")?;
    println!(
        "pick                pixel → fragment {} → {} ({})",
        picked.0,
        picked_element,
        picked_record.class.ifc_name()
    );
    assert_eq!(picked_element, &plan.instance);

    // ---- 6. spatial index -----------------------------------------------------------
    let index = model.spatial_index();
    let near_door = index.query_ids(&BoundingBox::new(
        DVec3::new(1.0, -1.0, 0.0),
        DVec3::new(3.0, 1.0, 3.0),
    ));
    println!(
        "spatial index       {} indexed, {} elements in the doorway region",
        index.len(),
        near_door.len()
    );

    // ---- 7. IFC boundary ------------------------------------------------------------
    let backend = SpfBackend::new(
        ExportContext::named("CADForge Demo")
            .at("2026-08-25T09:00:00")
            .with_types(vec![ExportedType {
                global_id: door.id.clone(),
                name: door.name.clone(),
                class: IfcClass::Door,
                predefined_type: Some("DOOR".into()),
            }]),
    );
    let ifc = backend.write(&model, IfcSchema::Ifc4)?;
    let parametric = model
        .iter()
        .filter_map(|e| e.representation.as_ref())
        .filter(|r| r.is_native_parametric())
        .count();
    println!(
        "ifc export          {} bytes, {} entities, {parametric} native parametric solids ({})",
        ifc.len(),
        // Count entity lines without an escape headache: 10 is the newline byte.
        ifc.iter().filter(|b| **b == 10).count(),
        backend.name()
    );

    std::fs::create_dir_all("out").context("creating ./out")?;
    std::fs::write("out/demo.ifc", &ifc).context("writing out/demo.ifc")?;

    // Re-read our own header. A file we cannot identify is a file nobody else can either.
    let header = String::from_utf8_lossy(&ifc[..ifc.len().min(1024)]);
    println!(
        "  schema            {} — round-tripped through our own detector",
        IfcSchema::detect(&header).context("our own header must be detectable")?
    );

    // The number that matters: our own cut meshes and an IFC consumer applying the void
    // should now agree. tools/validate_ifc.py checks the other side of this against
    // IfcOpenShell.
    let cut_wall_volume: f64 = walls
        .iter()
        .filter_map(|id| pieces.iter().find(|p| &p.id == id))
        .map(|p| p.world.signed_volume())
        .sum();
    println!(
        "  agreement         our meshes {:.2} m³ vs consumers applying the void {:.2} m³",
        cut_wall_volume,
        total_wall_volume - 0.92 * WALL_THICKNESS * 2.12
    );

    // ---- 8. undo the entire session -------------------------------------------------
    let revisions = model.revision();
    let elements = model.len();
    let mut undone = 0;
    while model.can_undo() {
        model.undo()?;
        undone += 1;
    }
    println!(
        "\nundo                {undone} commands reversed, {elements} elements → {}, revision {revisions} → {}",
        model.len(),
        model.revision()
    );
    assert!(model.is_empty(), "undo must return the model to empty");

    while model.can_redo() {
        model.redo()?;
    }
    println!("redo                replayed to {} elements", model.len());
    assert_eq!(model.len(), elements);

    // ---- 8b. render ------------------------------------------------------------------
    // The ADR-0001 payoff, on real hardware: one native wgpu path, no webview, no WebGL2
    // ceiling. Headless because a window belongs to the shell, not to the renderer, and
    // because a texture can be checked in CI.
    #[cfg(feature = "gpu")]
    {
        use cadforge_render::{MeshData, Renderer};

        let (width, height) = (1600u32, 900u32);
        let renderer = Renderer::new_headless(width, height)?;
        camera.set_viewport(width, height);
        camera.frame(&fragments.bounds());

        // Openings are voids, not things you look at. They cut their host and then get out
        // of the way — drawing them would show a box floating in a doorway.
        let drawable: Vec<&Piece> = pieces
            .iter()
            .filter(|p| {
                model
                    .get(&p.id)
                    .is_some_and(|e| e.class != IfcClass::OpeningElement)
            })
            .collect();

        let meshes: Vec<MeshData<'_>> = drawable
            .iter()
            .map(|piece| {
                let class = model
                    .get(&piece.id)
                    .map(|e| e.class.clone())
                    .unwrap_or(IfcClass::BuildingElementProxy);
                MeshData {
                    positions: &piece.world.positions,
                    normals: &piece.world.normals,
                    indices: &piece.world.indices,
                    color: color_for(&class),
                    id: FragmentId::NONE,
                }
            })
            .collect();

        renderer.render_to_png(&meshes, &camera, std::path::Path::new("out/demo.png"))?;

        // A second frame with the door hidden. The leaf fills its own opening to within the
        // 10 mm frame clearance, so in the first image the cut is invisible — correct, but it
        // proves nothing. Take the door away and the doorway has to be there.
        let without_door: Vec<MeshData<'_>> = drawable
            .iter()
            .zip(&meshes)
            .filter(|(piece, _)| {
                model
                    .get(&piece.id)
                    .is_some_and(|e| e.class != IfcClass::Door)
            })
            .map(|(_, mesh)| *mesh)
            .collect();
        renderer.render_to_png(
            &without_door,
            &camera,
            std::path::Path::new("out/demo-doorway.png"),
        )?;
        println!(
            "
gpu                 {}",
            renderer.adapter_description()
        );
        println!(
            "  rendered          {width}x{height} → out/demo.png (+ demo-doorway.png), {} meshes, {} triangles",
            meshes.len(),
            drawable
                .iter()
                .map(|p| p.world.triangle_count())
                .sum::<usize>()
        );
    }

    // ---- 9. write it out ------------------------------------------------------------
    let mut combined = IndexedMesh::new();
    for piece in &pieces {
        combined.append(&piece.world);
    }
    let path = "out/demo.obj";
    std::fs::write(path, combined.to_obj("cadforge_demo"))
        .with_context(|| format!("writing {path}"))?;
    println!(
        "\nwrote               out/demo.ifc and {path} — {} triangles, bounds {:.1} × {:.1} × {:.1} m",
        combined.triangle_count(),
        combined.bounds().size().x,
        combined.bounds().size().y,
        combined.bounds().size().z
    );

    Ok(())
}

/// Create an element through the command pipeline, returning its identity.
fn create(
    model: &mut Model,
    class: IfcClass,
    name: &str,
    container: Option<&GlobalId>,
) -> Result<GlobalId> {
    let id = GlobalId::new();
    let mut element = ElementRecord::new(id.clone(), class).with_name(name);
    element.container = container.cloned();
    model.apply(ModelCommand::CreateElement {
        element: Box::new(element),
    })?;
    Ok(id)
}

/// Author one wall: a rectangular profile swept vertically, placed along its axis.
///
/// The recipe is canonical; the mesh is derived. Local geometry is evaluated once and then
/// transformed by the element placement, which is how IFC composes an
/// `IfcExtrudedAreaSolid` with its `IfcLocalPlacement`.
fn build_wall(
    model: &mut Model,
    storey: &GlobalId,
    start: DVec3,
    end: DVec3,
    index: usize,
) -> Result<Piece> {
    let axis = end - start;
    let length = axis.length();

    let profile = Profile::rectangle(length, WALL_THICKNESS)?;
    let local = extrude(&profile, WALL_HEIGHT)?;

    let placement = Placement::new(start + axis * 0.5, DVec3::Z, axis);
    let world = local.transformed(placement.to_matrix());

    // The recipe is canonical; this is what gets written to IFC as an IfcExtrudedAreaSolid.
    // The mesh above is only a cache.
    let representation = Representation::extrusion(
        profile.outer().iter().map(|p| [p.x, p.y]).collect(),
        [0.0, 0.0, 1.0],
        WALL_HEIGHT,
    );

    let id = GlobalId::new();
    let element = ElementRecord::new(id.clone(), IfcClass::Wall)
        .with_name(format!("W-{:02}", index + 1))
        .with_placement(placement)
        .with_container(storey.clone());
    model.apply(ModelCommand::CreateElement {
        element: Box::new(element),
    })?;
    model.apply(ModelCommand::SetRepresentation {
        global_id: id.clone(),
        representation: Some(Box::new(representation)),
    })?;
    model.apply(ModelCommand::SetProperty {
        global_id: id.clone(),
        set: "Pset_WallCommon".into(),
        name: "IsExternal".into(),
        value: Some(cadforge_core::PropertyValue::Boolean(true)),
    })?;
    model.set_bounds(&id, Some(world.bounds()));

    Ok(Piece { id, local, world })
}

/// A door family as one would actually be authored.
fn door_family() -> Result<FamilyDefinition> {
    let leaf = GeometryRecipe::single_extrusion(
        ProfileSpec::Rectangle {
            width: Expr::param("Width"),
            depth: Expr::param("Thickness"),
        },
        [0.0, 0.0, 1.0],
        Expr::param("Height"),
    )?;

    // The void is wider and taller than the leaf: a door needs frame clearance, and that
    // detail is exactly what subtracting a box from a wall mesh throws away.
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
        IfcTypeMapping::new(IfcClass::Door)
            .with_predefined_type("DOOR")
            .with_property_set("Pset_DoorCommon"),
        vec![
            ParamDef::length("Width", 0.9)
                .with_range(0.4, 2.4)
                .described("Nominal leaf width"),
            ParamDef::length("Height", 2.1).with_range(1.2, 3.6),
            ParamDef::length("Thickness", 0.045),
            ParamDef::length("FrameClearance", 0.02),
            ParamDef::instance_length("SillHeight", 0.0).with_range(0.0, 1.2),
        ],
        leaf,
    )?
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
    ))
}

/// Hash the geometry so identical meshes can share a GPU buffer.
///
/// Hashes bit patterns, so it is exact and platform-stable — two meshes with the same hash
/// really are the same mesh, which is what makes instancing safe.
fn geometry_hash(mesh: &IndexedMesh) -> u64 {
    let mut hasher = DefaultHasher::new();
    for p in &mesh.positions {
        p.x.to_bits().hash(&mut hasher);
        p.y.to_bits().hash(&mut hasher);
        p.z.to_bits().hash(&mut hasher);
    }
    mesh.indices.hash(&mut hasher);
    hasher.finish()
}

/// Linear RGB per class, so the render reads as a building rather than a pile of shapes.
#[cfg(feature = "gpu")]
fn color_for(class: &IfcClass) -> [f32; 3] {
    match class {
        IfcClass::Wall => [0.78, 0.76, 0.72],
        IfcClass::Slab | IfcClass::Roof => [0.62, 0.62, 0.64],
        IfcClass::Door | IfcClass::Window => [0.72, 0.45, 0.20],
        IfcClass::OpeningElement => [0.90, 0.25, 0.25],
        _ => [0.60, 0.62, 0.66],
    }
}

fn material_for(class: &IfcClass) -> &'static str {
    match class {
        IfcClass::Wall => "concrete",
        IfcClass::Door | IfcClass::Window => "timber",
        IfcClass::Slab | IfcClass::Roof => "concrete",
        IfcClass::OpeningElement => "void",
        _ => "default",
    }
}
