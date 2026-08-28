//! A window you can draw in.
//!
//! Phase 3b opened it; Phase 5 made it author. The render pipeline was proven headless on real
//! hardware in Phase 3a; what was unproven is everything around it — the event loop, surface
//! configuration, resize, and present. That is precisely the part that differs per platform,
//! which is why it lives here in the shell and not in `cadforge-render` (ADR-0002).
//!
//! ```text
//! cargo run -p cadforge-shell --features viewport --bin cadforge-viewport
//! cargo run -p cadforge-shell --features viewport --bin cadforge-viewport -- model.ifc
//! cargo run -p cadforge-shell --features viewport --bin cadforge-viewport -- --frames 3
//! ```
//!
//! With no argument it builds the demo room. Given an `.ifc` path it imports the file and
//! shows it, which makes this the first place the reader, the geometry pipeline, and the
//! renderer are all load-bearing at once.
//!
//! `--frames N` renders N frames and exits. A window that closes itself is testable; one that
//! waits for a human is not.
//!
//! The drawing tools themselves are in `cadforge-tools` and know nothing about winit. What is
//! here is the part that genuinely needs a window: turning a cursor into a world point, and
//! turning a keystroke into a tool.

use anyhow::{Context, Result};
use cadforge_core::{BoundingBox, GlobalId, IfcClass, Model, Representation};
use cadforge_geom::{extrude_along, IndexedMesh, Profile};
use cadforge_ifc::{IfcBackend, IfcLiteBackend};
use cadforge_render::{Camera, FragmentId, MeshData, Renderer, SectionPlane};
use cadforge_tools::{snap_candidates, world_transform, ContainerRef, Draft, DraftOutcome, Tool};
use glam::{DMat4, DVec2, DVec3};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

/// How far the camera orbits per pixel of drag.
const ORBIT_PER_PIXEL: f64 = 0.008;
/// Fraction of the orbit distance panned per pixel, so panning feels the same at any zoom.
const PAN_PER_PIXEL: f64 = 0.0018;
/// How far a click reaches for something to snap to, in pixels. World tolerance is derived
/// from this and the zoom, because "near where I clicked" is a screen-space idea.
const SNAP_PIXELS: f64 = 12.0;
/// The ghost element under the cursor. Zero is the pick buffer's miss sentinel, so clicking
/// a preview selects nothing — which is right, because it is not there yet.
const PREVIEW_ID: FragmentId = FragmentId(0);

fn main() -> Result<()> {
    let mut frames = None;
    let mut png = None;
    let mut section = None;
    let mut path = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().and_then(|n| n.parse::<u32>().ok()),
            "--png" => png = args.next(),
            "--section" => section = args.next().and_then(|a| axis_of(&a)),
            other => path = Some(other.to_string()),
        }
    }

    let scene = match &path {
        Some(file) => Scene::from_ifc(file)?,
        None => Scene::demo()?,
    };
    println!(
        "scene               {} drawable elements, {} triangles, {:.1} × {:.1} × {:.1} m",
        scene.meshes.len(),
        scene
            .meshes
            .iter()
            .map(|m| m.triangle_count())
            .sum::<usize>(),
        scene.bounds.size().x,
        scene.bounds.size().y,
        scene.bounds.size().z
    );

    // Headless: render one frame to a file and never open a window. Useful for thumbnails,
    // for CI, and for looking at a model on a machine with no display.
    if let Some(png) = png {
        let mut renderer = Renderer::new_headless(1600, 1000)?;
        if let Some(axis) = section {
            renderer.set_sections(&[SectionPlane::halving(&scene.bounds, axis)]);
            println!("section             cutting at the model centre along {axis}");
        }
        let mut camera = Camera::default();
        camera.set_viewport(1600, 1000);
        camera.frame(&scene.bounds);
        renderer.render_to_png(&scene.mesh_data(None), &camera, std::path::Path::new(&png))?;
        println!("gpu                 {}", renderer.adapter_description());
        println!("wrote               {png}");
        return Ok(());
    }

    let event_loop = EventLoop::new().context("creating the event loop")?;
    // Redraw only when something changes. A CAD viewport that spins the GPU at 60 fps over a
    // static model is just a heater.
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop
        .run_app(&mut App::new(scene, frames))
        .context("running the event loop")?;
    Ok(())
}

/// `x`, `-y`, `z` and so on.
fn axis_of(name: &str) -> Option<DVec3> {
    let (sign, letter) = match name.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, name),
    };
    let axis = match letter.to_ascii_lowercase().as_str() {
        "x" => DVec3::X,
        "y" => DVec3::Y,
        "z" => DVec3::Z,
        _ => return None,
    };
    Some(axis * sign)
}

/// The model, plus everything needed to draw it, already in world space.
///
/// The model is the authority and the meshes are a cache of it — delete every mesh and
/// nothing is lost. That is why editing rebuilds rather than patching: correct by
/// construction, and fast enough at the scale a person draws at.
struct Scene {
    model: Model,
    meshes: Vec<IndexedMesh>,
    colors: Vec<[f32; 3]>,
    /// Parallel to `meshes`. Selection is by identity, not by index, so it survives a rebuild.
    ids: Vec<GlobalId>,
    /// What each mesh is, so a pick can report something a person recognises.
    labels: Vec<String>,
    bounds: BoundingBox,
    selected: Option<GlobalId>,
}

impl Scene {
    fn from_ifc(path: &str) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
        let mut model = Model::new();
        let report = IfcLiteBackend::new()
            .read(&bytes, &mut model)
            .with_context(|| format!("importing {path}"))?;
        println!(
            "imported            {path}: {} schema, {} elements, {} warnings",
            report.schema.header_name(),
            report.elements,
            report.warnings.len()
        );
        Self::from_model(model)
    }

    fn demo() -> Result<Self> {
        let mut model = Model::new();
        crate::demo::build(&mut model)?;
        Self::from_model(model)
    }

    fn from_model(model: Model) -> Result<Self> {
        let mut scene = Self {
            model,
            meshes: Vec::new(),
            colors: Vec::new(),
            ids: Vec::new(),
            labels: Vec::new(),
            bounds: BoundingBox::empty(),
            selected: None,
        };
        scene.rebuild();
        anyhow::ensure!(
            !scene.meshes.is_empty(),
            "nothing in this model has geometry"
        );
        Ok(scene)
    }

    /// Regenerate the mesh cache from the model.
    ///
    /// The bounds only ever grow. Shrinking them on every edit would re-frame the camera under
    /// the user's hands the moment they deleted something.
    fn rebuild(&mut self) {
        self.meshes.clear();
        self.colors.clear();
        self.ids.clear();
        self.labels.clear();

        for element in self.model.iter() {
            // Openings are voids. Drawing one puts a box in a doorway.
            if element.class == IfcClass::OpeningElement {
                continue;
            }
            let Some(representation) = &element.representation else {
                continue;
            };
            let Some(local) = mesh_of(representation) else {
                continue;
            };
            let world = local.transformed(world_transform(&self.model, element.global_id.clone()));
            self.bounds = self.bounds.union(world.bounds());
            self.colors.push(color_for(&element.class));
            self.labels.push(format!(
                "{} {}",
                element.class.ifc_name(),
                element
                    .name
                    .as_deref()
                    .unwrap_or(element.global_id.as_str())
            ));
            self.ids.push(element.global_id.clone());
            self.meshes.push(world);
        }

        // An element that no longer exists cannot stay selected.
        if let Some(selected) = &self.selected {
            if !self.model.contains(selected) {
                self.selected = None;
            }
        }
    }

    /// The lowest storey, and where it sits, so drawn elements are filed somewhere real.
    ///
    /// Elements with no container export, but they belong to no level, which is the kind of
    /// thing that looks fine in a viewer and falls apart in a schedule.
    fn ground_storey(&self) -> Option<ContainerRef> {
        let mut storeys: Vec<(&GlobalId, f64)> = self
            .model
            .by_class(&IfcClass::BuildingStorey)
            .map(|s| {
                let origin =
                    world_transform(&self.model, s.global_id.clone()).transform_point3(DVec3::ZERO);
                (&s.global_id, origin.z)
            })
            .collect();
        storeys.sort_by(|a, b| a.1.total_cmp(&b.1));
        let (id, elevation) = storeys.first()?;
        Some(ContainerRef::new(
            (*id).clone(),
            DVec3::new(0.0, 0.0, *elevation),
        ))
    }

    /// Identities start at 1, because zero is the miss sentinel a cleared pick buffer reads
    /// as. Index `i` is therefore `FragmentId(i + 1)`, and [`PREVIEW_ID`] takes the zero.
    fn mesh_data<'a>(&'a self, preview: Option<&'a IndexedMesh>) -> Vec<MeshData<'a>> {
        let mut data: Vec<MeshData<'a>> = self
            .meshes
            .iter()
            .enumerate()
            .map(|(i, mesh)| MeshData {
                positions: &mesh.positions,
                normals: &mesh.normals,
                indices: &mesh.indices,
                color: if self.selected.as_ref() == Some(&self.ids[i]) {
                    [0.95, 0.62, 0.20]
                } else {
                    self.colors[i]
                },
                id: FragmentId(i as u32 + 1),
            })
            .collect();

        if let Some(mesh) = preview {
            data.push(MeshData {
                positions: &mesh.positions,
                normals: &mesh.normals,
                indices: &mesh.indices,
                color: [0.30, 0.70, 0.95],
                id: PREVIEW_ID,
            });
        }
        data
    }

    fn id_at(&self, id: FragmentId) -> Option<&GlobalId> {
        (id.0 as usize).checked_sub(1).and_then(|i| self.ids.get(i))
    }

    fn label_of(&self, id: &GlobalId) -> &str {
        self.ids
            .iter()
            .position(|candidate| candidate == id)
            .map(|i| self.labels[i].as_str())
            .unwrap_or("(no geometry)")
    }
}

/// Turn a stored representation back into triangles.
fn mesh_of(representation: &Representation) -> Option<IndexedMesh> {
    match representation {
        Representation::ExtrudedAreaSolid {
            profile,
            direction,
            depth,
        } => {
            let profile = Profile::new(profile.iter().map(|p| DVec2::new(p[0], p[1]))).ok()?;
            extrude_along(&profile, DVec3::from_array(*direction), *depth).ok()
        }
        Representation::TriangulatedFaceSet { vertices, faces } => {
            let mut mesh = IndexedMesh::with_capacity(faces.len());
            for face in faces {
                let corner = |i: u32| -> Option<DVec3> {
                    vertices.get(i as usize).map(|v| DVec3::from_array(*v))
                };
                mesh.push_triangle(corner(face[0])?, corner(face[1])?, corner(face[2])?);
            }
            (!mesh.is_empty()).then_some(mesh)
        }
    }
}

fn color_for(class: &IfcClass) -> [f32; 3] {
    match class {
        IfcClass::Wall => [0.78, 0.76, 0.72],
        IfcClass::Slab | IfcClass::Roof => [0.62, 0.62, 0.64],
        IfcClass::Door | IfcClass::Window => [0.72, 0.45, 0.20],
        IfcClass::Column | IfcClass::Beam => [0.55, 0.58, 0.62],
        IfcClass::Space => [0.35, 0.55, 0.70],
        _ => [0.58, 0.62, 0.68],
    }
}

/// GPU state that only exists once there is a window to present to.
struct Surface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    depth: wgpu::TextureView,
}

struct App {
    scene: Scene,
    window: Option<Arc<Window>>,
    gpu: Option<Surface>,
    camera: Camera,
    /// The active tool and whatever is half-drawn. Platform-free, and tested without a window.
    draft: Draft,
    /// The ghost element under the cursor, rebuilt on move.
    preview: Option<IndexedMesh>,
    /// Left-drag orbits, right- or middle-drag pans.
    dragging: Option<MouseButton>,
    cursor: Option<DVec2>,
    /// Where the current press started, to tell a click from the end of a drag.
    press_at: Option<DVec2>,
    modifiers: ModifiersState,
    /// Render this many frames then exit, so the viewport is testable without a human.
    frames_left: Option<u32>,
    /// The active section, if any. View state — the model never learns about it.
    section: Option<SectionPlane>,
}

impl App {
    fn new(scene: Scene, frames: Option<u32>) -> Self {
        Self {
            scene,
            window: None,
            gpu: None,
            camera: Camera::default(),
            draft: Draft::default(),
            preview: None,
            dragging: None,
            cursor: None,
            press_at: None,
            modifiers: ModifiersState::empty(),
            frames_left: frames,
            section: None,
        }
    }

    fn configure(&mut self, width: u32, height: u32) {
        let Some(gpu) = &mut self.gpu else { return };
        if width == 0 || height == 0 {
            return;
        }
        gpu.config.width = width;
        gpu.config.height = height;
        gpu.surface.configure(gpu.renderer.device(), &gpu.config);
        gpu.renderer.resize(width, height);
        // The depth buffer must match the colour target exactly, or the pass is invalid.
        gpu.depth = gpu.renderer.create_depth_view();
        self.camera.set_viewport(width, height);
    }

    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn viewport(&self) -> DVec2 {
        match &self.gpu {
            Some(gpu) => DVec2::new(gpu.config.width as f64, gpu.config.height as f64),
            None => DVec2::new(1280.0, 800.0),
        }
    }

    /// Where the cursor is on the level being drawn on.
    ///
    /// `None` when the ray never meets the plane, which happens the moment the camera drops to
    /// eye level and looks along it.
    fn ground_point(&self, cursor: DVec2) -> Option<DVec3> {
        self.camera
            .ray_at(cursor, self.viewport())
            .intersect_ground(self.draft.settings.elevation)
    }

    /// Snap tolerance in metres, from a fixed reach in pixels.
    ///
    /// Fixing it in metres instead would make snapping grabby when zoomed out and unreachable
    /// when zoomed in — the same gesture has to mean the same thing at every scale.
    fn snap_tolerance(&self) -> f64 {
        let metres_per_pixel = self.camera.distance / self.viewport().y.max(1.0);
        (metres_per_pixel * SNAP_PIXELS).max(1e-4)
    }

    /// The points a click near `at` could latch onto.
    fn candidates(&self, at: DVec3) -> Vec<DVec3> {
        snap_candidates(&self.scene.model, at, self.snap_tolerance())
    }

    /// Rebuild the ghost under the cursor. Cheap enough to do on every mouse move.
    fn update_preview(&mut self) {
        self.preview = None;
        if self.draft.tool() == Tool::Select {
            return;
        }
        let Some(cursor) = self.cursor else { return };
        let Some(point) = self.ground_point(cursor) else {
            return;
        };
        self.preview = self
            .draft
            .preview(point, &self.candidates(point))
            .and_then(|element| {
                let representation = element.representation.as_ref()?;
                Some(mesh_of(representation)?.transformed(
                    // Local to world: the ghost is not in the model, so there is no chain to
                    // walk — the container's own transform is the whole of it.
                    self.container_transform() * element.placement.to_matrix(),
                ))
            });
    }

    fn container_transform(&self) -> DMat4 {
        match &self.draft.container {
            Some(container) => world_transform(&self.scene.model, container.id.clone()),
            None => DMat4::IDENTITY,
        }
    }

    /// Draw at the cursor, or select under it.
    fn click(&mut self, cursor: DVec2) {
        if self.draft.tool() == Tool::Select {
            self.select(cursor);
            return;
        }
        let Some(point) = self.ground_point(cursor) else {
            println!("draw                the view is edge-on to the level; orbit up a little");
            return;
        };
        let candidates = self.candidates(point);
        match self.draft.click(point, &candidates) {
            Ok(DraftOutcome::Commit {
                commands,
                global_id,
                ..
            }) => match self.scene.model.apply_all(commands) {
                Ok(_) => {
                    self.scene.rebuild();
                    self.scene.selected = Some(global_id.clone());
                    println!("drew                {}", self.scene.label_of(&global_id));
                }
                // The tool built it and the model refused it. Worth saying out loud rather
                // than swallowing, because it means the two disagree about what is valid.
                Err(e) => eprintln!("the model refused that element: {e}"),
            },
            Ok(DraftOutcome::Pending(snap)) => {
                println!(
                    "point               {:.3}, {:.3}, {:.3} ({:?})",
                    snap.point.x, snap.point.y, snap.point.z, snap.kind
                );
            }
            Ok(DraftOutcome::Ignored) => {}
            Err(e) => eprintln!("{e}"),
        }
        self.update_preview();
        self.redraw();
    }

    fn select(&mut self, cursor: DVec2) {
        let Some(gpu) = &self.gpu else { return };
        match gpu.renderer.pick(
            &self.scene.mesh_data(None),
            &self.camera,
            cursor.x.max(0.0) as u32,
            cursor.y.max(0.0) as u32,
        ) {
            Ok(Some(id)) => {
                self.scene.selected = self.scene.id_at(id).cloned();
                match &self.scene.selected {
                    Some(id) => println!("selected            {}", self.scene.label_of(id)),
                    None => println!("selected            nothing"),
                }
            }
            Ok(None) => {
                println!("selected            nothing");
                self.scene.selected = None;
            }
            Err(e) => eprintln!("pick failed: {e}"),
        }
        self.redraw();
    }

    /// Undo or redo, then resynchronise everything derived from the model.
    fn history(&mut self, redo: bool) {
        let result = if redo {
            self.scene.model.redo()
        } else {
            self.scene.model.undo()
        };
        match result {
            Ok(_) => {
                // Half-drawn points refer to a model state that no longer exists.
                self.draft.cancel();
                self.scene.rebuild();
                println!(
                    "{:<19} revision {}",
                    if redo { "redo" } else { "undo" },
                    self.scene.model.revision()
                );
            }
            Err(e) => println!("{:<19} {e}", if redo { "redo" } else { "undo" }),
        }
        self.update_preview();
        self.redraw();
    }

    fn set_tool(&mut self, tool: Tool) {
        self.draft.set_tool(tool);
        println!("tool                {}", tool.label());
        self.update_preview();
        self.redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("CADForge")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 800));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                eprintln!("could not create a window: {e}");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        // Cloning the Arc into the surface is what makes it `Surface<'static>`; borrowing the
        // window instead ties the surface to a local and will not compile.
        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(e) => {
                eprintln!("could not create a surface: {e}");
                event_loop.exit();
                return;
            }
        };

        let renderer = match Renderer::for_surface(&instance, &surface, size.width, size.height) {
            Ok(renderer) => renderer,
            Err(e) => {
                eprintln!("no usable GPU: {e}");
                event_loop.exit();
                return;
            }
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: renderer.format(),
            color_space: wgpu::SurfaceColorSpace::Srgb,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(renderer.device(), &config);
        let depth = renderer.create_depth_view();

        // Draw onto the lowest storey by default, and file new elements there.
        self.draft.container = self.scene.ground_storey();
        self.draft.settings.elevation = self
            .draft
            .container
            .as_ref()
            .map(|c| c.origin.z)
            .unwrap_or(0.0);

        println!("gpu                 {}", renderer.adapter_description());
        println!("window              {}×{}", size.width, size.height);
        println!(
            "level               drawing at {:.3} m{}",
            self.draft.settings.elevation,
            match &self.draft.container {
                Some(_) => "",
                None => " (no storey found — elements will be uncontained)",
            }
        );
        println!("tools               1 select · 2 wall · 3 slab · 4 column");
        println!("draw                click to place · Enter closes a slab · Backspace takes a point back · Esc cancels");
        println!("edit                Ctrl+Z undo · Ctrl+Y redo");
        println!("view                X/Y/Z section · [ ] slide · C clear · F frame");

        self.gpu = Some(Surface {
            surface,
            config,
            renderer,
            depth,
        });
        self.window = Some(window);

        self.camera.set_viewport(size.width, size.height);
        self.camera.frame(&self.scene.bounds);
        self.redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.modifiers.control_key() {
                    match code {
                        KeyCode::KeyZ if self.modifiers.shift_key() => self.history(true),
                        KeyCode::KeyZ => self.history(false),
                        KeyCode::KeyY => self.history(true),
                        _ => {}
                    }
                    return;
                }

                match code {
                    KeyCode::Digit1 => return self.set_tool(Tool::Select),
                    KeyCode::Digit2 => return self.set_tool(Tool::Wall),
                    KeyCode::Digit3 => return self.set_tool(Tool::Slab),
                    KeyCode::Digit4 => return self.set_tool(Tool::Column),

                    KeyCode::Enter | KeyCode::NumpadEnter => {
                        match self.draft.finish() {
                            Ok(DraftOutcome::Commit {
                                commands,
                                global_id,
                                ..
                            }) => {
                                if let Err(e) = self.scene.model.apply_all(commands) {
                                    eprintln!("the model refused that element: {e}");
                                } else {
                                    self.scene.rebuild();
                                    self.scene.selected = Some(global_id.clone());
                                    println!(
                                        "drew                {}",
                                        self.scene.label_of(&global_id)
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(e) => eprintln!("{e}"),
                        }
                        self.update_preview();
                        self.redraw();
                        return;
                    }

                    KeyCode::Backspace => {
                        self.draft.undo_point();
                        self.update_preview();
                        self.redraw();
                        return;
                    }

                    // Escape cancels the drawing first and clears the section second, so it
                    // never throws away a half-drawn outline and a view at the same time.
                    KeyCode::Escape if self.draft.is_drawing() => {
                        self.draft.cancel();
                        self.update_preview();
                        self.redraw();
                        return;
                    }
                    _ => {}
                }

                // X/Y/Z section through the model centre, [ and ] slide the cut, C clears it.
                let step = self.scene.bounds.size().max_element() * 0.05;
                self.section = match code {
                    KeyCode::KeyX => Some(SectionPlane::halving(&self.scene.bounds, DVec3::X)),
                    KeyCode::KeyY => Some(SectionPlane::halving(&self.scene.bounds, DVec3::Y)),
                    KeyCode::KeyZ => Some(SectionPlane::halving(&self.scene.bounds, DVec3::Z)),
                    KeyCode::BracketLeft => self.section.map(|p| p.offset_by(-step)),
                    KeyCode::BracketRight => self.section.map(|p| p.offset_by(step)),
                    KeyCode::KeyC | KeyCode::Escape => None,
                    KeyCode::KeyF => {
                        self.camera.frame(&self.scene.bounds);
                        self.section
                    }
                    _ => return,
                };

                if let Some(gpu) = &mut self.gpu {
                    match self.section {
                        Some(plane) => gpu.renderer.set_sections(&[plane]),
                        None => gpu.renderer.set_sections(&[]),
                    }
                }
                self.redraw();
            }

            WindowEvent::Resized(size) => {
                self.configure(size.width, size.height);
                self.redraw();
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed {
                    self.dragging = Some(button);
                    self.press_at = self.cursor;
                    return;
                }
                self.dragging = None;

                // A click is a release close to where the press landed. Anything further is
                // the end of a drag, and orbiting should not also draw.
                let moved = match (self.press_at, self.cursor) {
                    (Some(from), Some(to)) => (to - from).length(),
                    _ => f64::INFINITY,
                };
                if button != MouseButton::Left || moved > 4.0 {
                    return;
                }
                if let Some(cursor) = self.cursor {
                    self.click(cursor);
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let now = DVec2::new(position.x, position.y);
                let orbiting = self.dragging.is_some();
                if let (Some(button), Some(last)) = (self.dragging, self.cursor) {
                    let delta = now - last;
                    match button {
                        MouseButton::Left => self
                            .camera
                            .orbit(-delta.x * ORBIT_PER_PIXEL, -delta.y * ORBIT_PER_PIXEL),
                        // Pan scales with distance, so it tracks the cursor at any zoom.
                        _ => {
                            let scale = self.camera.distance * PAN_PER_PIXEL;
                            self.camera.pan(-delta.x * scale, delta.y * scale);
                        }
                    }
                    self.redraw();
                }
                self.cursor = Some(now);

                // Only when a tool is armed. Tracing a ghost through every mouse move of an
                // orbit would be work nobody asked for.
                if !orbiting && self.draft.tool() != Tool::Select {
                    self.update_preview();
                    self.redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                };
                self.camera.dolly(0.9f64.powf(notches));
                self.redraw();
            }

            WindowEvent::RedrawRequested => {
                let Some(gpu) = &mut self.gpu else { return };
                // wgpu 30 reports acquisition as an enum rather than a Result, which makes
                // the recoverable cases explicit: suboptimal is still drawable, outdated and
                // lost want a reconfigure, and occluded means there is nothing to draw.
                let frame = match gpu.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame) => frame,
                    wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                        gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                        frame
                    }
                    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                        gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                        return;
                    }
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => return,
                    other => {
                        eprintln!("could not acquire a frame: {other:?}");
                        event_loop.exit();
                        return;
                    }
                };

                let view = frame.texture.create_view(&Default::default());
                let commands = gpu.renderer.render_to_view(
                    &self.scene.mesh_data(self.preview.as_ref()),
                    &self.camera,
                    &view,
                    &gpu.depth,
                );
                gpu.renderer.queue().submit([commands]);
                gpu.renderer.queue().present(frame);

                if let Some(left) = &mut self.frames_left {
                    *left = left.saturating_sub(1);
                    if *left == 0 {
                        println!("rendered            the requested frames, exiting");
                        event_loop.exit();
                    } else if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }

            _ => {}
        }
    }
}

#[path = "demo_scene.rs"]
mod demo;
