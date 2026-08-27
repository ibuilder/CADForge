//! A window.
//!
//! Phase 3b. The render pipeline was proven headless on real hardware in Phase 3a; what was
//! unproven is everything around it — the event loop, surface configuration, resize, and
//! present. That is precisely the part that differs per platform, which is why it lives here
//! in the shell and not in `cadforge-render` (ADR-0002).
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

use anyhow::{Context, Result};
use cadforge_core::{BoundingBox, GlobalId, IfcClass, Model, Representation};
use cadforge_geom::{extrude_along, IndexedMesh, Profile};
use cadforge_ifc::{IfcBackend, IfcLiteBackend};
use cadforge_render::{Camera, FragmentId, MeshData, Renderer};
use glam::{DMat4, DVec2, DVec3};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// How far the camera orbits per pixel of drag.
const ORBIT_PER_PIXEL: f64 = 0.008;
/// Fraction of the orbit distance panned per pixel, so panning feels the same at any zoom.
const PAN_PER_PIXEL: f64 = 0.0018;

fn main() -> Result<()> {
    let mut frames = None;
    let mut png = None;
    let mut path = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().and_then(|n| n.parse::<u32>().ok()),
            "--png" => png = args.next(),
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
        let renderer = Renderer::new_headless(1600, 1000)?;
        let mut camera = Camera::default();
        camera.set_viewport(1600, 1000);
        camera.frame(&scene.bounds);
        renderer.render_to_png(&scene.mesh_data(), &camera, std::path::Path::new(&png))?;
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

/// Everything to draw, already in world space.
struct Scene {
    meshes: Vec<IndexedMesh>,
    colors: Vec<[f32; 3]>,
    /// What each mesh is, so a pick can report something a person recognises.
    labels: Vec<String>,
    bounds: BoundingBox,
    /// Highlighted by the last click.
    selected: Option<usize>,
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
        Self::from_model(&model)
    }

    fn demo() -> Result<Self> {
        let mut model = Model::new();
        crate::demo::build(&mut model)?;
        Self::from_model(&model)
    }

    fn from_model(model: &Model) -> Result<Self> {
        let mut meshes = Vec::new();
        let mut colors = Vec::new();
        let mut labels = Vec::new();
        let mut bounds = BoundingBox::empty();

        for element in model.iter() {
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
            let world = local.transformed(world_transform(model, element.global_id.clone()));
            bounds = bounds.union(world.bounds());
            colors.push(color_for(&element.class));
            labels.push(format!(
                "{} {}",
                element.class.ifc_name(),
                element
                    .name
                    .as_deref()
                    .unwrap_or(element.global_id.as_str())
            ));
            meshes.push(world);
        }

        anyhow::ensure!(!meshes.is_empty(), "nothing in this model has geometry");
        Ok(Self {
            meshes,
            colors,
            labels,
            bounds,
            selected: None,
        })
    }

    /// Identities start at 1, because zero is the miss sentinel a cleared pick buffer reads
    /// as. Index `i` is therefore `FragmentId(i + 1)`.
    fn mesh_data(&self) -> Vec<MeshData<'_>> {
        self.meshes
            .iter()
            .enumerate()
            .map(|(i, mesh)| MeshData {
                positions: &mesh.positions,
                normals: &mesh.normals,
                indices: &mesh.indices,
                color: if self.selected == Some(i) {
                    [0.95, 0.62, 0.20]
                } else {
                    self.colors[i]
                },
                id: FragmentId(i as u32 + 1),
            })
            .collect()
    }

    fn index_of(&self, id: FragmentId) -> Option<usize> {
        (id.0 as usize)
            .checked_sub(1)
            .filter(|i| *i < self.meshes.len())
    }
}

/// Compose an element's placement with its containers, up to the spatial root.
///
/// The model stores each placement relative to whatever contains it, so drawing needs the
/// chain. Bounded so a file with a containment cycle cannot hang the viewer.
fn world_transform(model: &Model, mut id: GlobalId) -> DMat4 {
    let mut transform = DMat4::IDENTITY;
    for _ in 0..64 {
        let Some(element) = model.get(&id) else { break };
        transform = element.placement.to_matrix() * transform;
        match &element.container {
            Some(parent) if parent != &id => id = parent.clone(),
            _ => break,
        }
    }
    transform
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
    /// Left-drag orbits, right- or middle-drag pans.
    dragging: Option<MouseButton>,
    cursor: Option<DVec2>,
    /// Where the current press started, to tell a click from the end of a drag.
    press_at: Option<DVec2>,
    /// Render this many frames then exit, so the viewport is testable without a human.
    frames_left: Option<u32>,
}

impl App {
    fn new(scene: Scene, frames: Option<u32>) -> Self {
        Self {
            scene,
            window: None,
            gpu: None,
            camera: Camera::default(),
            dragging: None,
            cursor: None,
            press_at: None,
            frames_left: frames,
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

        println!("gpu                 {}", renderer.adapter_description());
        println!("window              {}×{}", size.width, size.height);

        self.gpu = Some(Surface {
            surface,
            config,
            renderer,
            depth,
        });
        self.window = Some(window);

        self.camera.set_viewport(size.width, size.height);
        self.camera.frame(&self.scene.bounds);
        self.window.as_ref().unwrap().request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                self.configure(size.width, size.height);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed {
                    self.dragging = Some(button);
                    self.press_at = self.cursor;
                    return;
                }
                self.dragging = None;

                // A click is a release close to where the press landed. Anything further is
                // the end of a drag, and orbiting should not also select.
                let moved = match (self.press_at, self.cursor) {
                    (Some(from), Some(to)) => (to - from).length(),
                    _ => f64::INFINITY,
                };
                if button != MouseButton::Left || moved > 4.0 {
                    return;
                }
                let (Some(gpu), Some(cursor)) = (&self.gpu, self.cursor) else {
                    return;
                };
                match gpu.renderer.pick(
                    &self.scene.mesh_data(),
                    &self.camera,
                    cursor.x.max(0.0) as u32,
                    cursor.y.max(0.0) as u32,
                ) {
                    Ok(Some(id)) => {
                        let index = self.scene.index_of(id);
                        if let Some(i) = index {
                            println!("selected            {}", self.scene.labels[i]);
                        }
                        self.scene.selected = index;
                    }
                    Ok(None) => {
                        println!("selected            nothing");
                        self.scene.selected = None;
                    }
                    Err(e) => eprintln!("pick failed: {e}"),
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let now = DVec2::new(position.x, position.y);
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
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                self.cursor = Some(now);
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                };
                self.camera.dolly(0.9f64.powf(notches));
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
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
                    &self.scene.mesh_data(),
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
