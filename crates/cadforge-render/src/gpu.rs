//! The wgpu backend.
//!
//! This is the payoff for [ADR-0001](../../../docs/adr/0001-native-shell-over-webview.md):
//! one renderer, natively, on DX12 / Metal / Vulkan, with no webview in the hot path and no
//! WebGL2 ceiling on mobile.
//!
//! It renders **headlessly** — to a texture, not a window. Two reasons, both deliberate:
//!
//! 1. **It is testable.** A window needs a display server, an event loop, and a human. A
//!    texture needs none of those, so the entire pipeline runs in CI and on a build machine.
//! 2. **Windowing is a shell concern.** `winit` belongs in `cadforge-shell` (ADR-0002).
//!    Everything here works identically whether the target is an offscreen texture, a
//!    desktop swapchain, or an iOS `CAMetalLayer` — only the target changes.
//!
//! Feature-gated behind `gpu` so that `cargo test -p cadforge-core` still needs no GPU.

use crate::camera::Camera;
use crate::fragment::FragmentId;
use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use std::borrow::Cow;
use wgpu::util::DeviceExt;

/// Render target format. sRGB so the bytes written to a PNG are display-ready.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Identity buffer format. **Not** sRGB: these bytes are an integer, and a gamma curve
/// applied to an id turns it into a different id.
const PICK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// `copy_texture_to_buffer` requires each row to start on a 256-byte boundary.
const COPY_ALIGNMENT: u32 = 256;

/// Everything that can go wrong talking to a GPU.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter available: {0}")]
    NoAdapter(String),

    #[error("could not create a device: {0}")]
    Device(String),

    #[error("reading the framebuffer back failed: {0}")]
    Readback(String),

    #[error("writing the image failed: {0}")]
    Image(String),
}

/// One mesh to draw, borrowed rather than owned so the renderer never copies the model.
///
/// Takes raw slices instead of a `cadforge_geom::IndexedMesh` on purpose: the renderer has no
/// business knowing how geometry was authored, and this keeps `cadforge-render` free of a
/// dependency on `cadforge-geom`.
#[derive(Debug, Clone, Copy)]
pub struct MeshData<'a> {
    pub positions: &'a [DVec3],
    pub normals: &'a [DVec3],
    pub indices: &'a [u32],
    /// Linear RGB.
    pub color: [f32; 3],
    /// What a click on this mesh resolves to. `FragmentId::NONE` (zero) makes it unpickable,
    /// which is what a grid or a gizmo wants.
    pub id: FragmentId,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
    /// Flat-interpolated through to the pick shader.
    id: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view_projection: [[f32; 4]; 4],
    /// `xyz` is the light direction; `w` is unused padding to keep the 16-byte alignment
    /// WGSL requires.
    light: [f32; 4],
}

/// A headless wgpu renderer.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    pick_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    /// Colour format the pipeline was built for. Headless uses [`COLOR_FORMAT`]; a window
    /// uses whatever its swapchain offers, which on most desktops is BGRA rather than RGBA.
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    backend: String,
    adapter_name: String,
}

impl Renderer {
    /// Bring up a device with no surface attached.
    ///
    /// `PowerPreference::HighPerformance` picks the discrete GPU on a laptop with two.
    pub fn new_headless(width: u32, height: u32) -> Result<Self, GpuError> {
        // wgpu 30 has no `InstanceDescriptor::default()`; headless means no display handle.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        Self::new_for_target(&instance, None, Some(COLOR_FORMAT), width, height)
    }

    /// Bring up a device against a window surface.
    ///
    /// The surface must be passed as `compatible_surface` so the adapter chosen can actually
    /// present to it — on a laptop with two GPUs, the fast one is not always the one wired to
    /// the display.
    /// The format is chosen from what the surface actually supports, preferring sRGB, rather
    /// than guessed. Hardcoding `Bgra8UnormSrgb` works on most desktops and fails on the ones
    /// it does not.
    pub fn for_surface(
        instance: &wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        width: u32,
        height: u32,
    ) -> Result<Self, GpuError> {
        Self::new_for_target(instance, Some(surface), None, width, height)
    }

    fn new_for_target(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        format: Option<wgpu::TextureFormat>,
        width: u32,
        height: u32,
    ) -> Result<Self, GpuError> {
        let width = width.max(1);
        let height = height.max(1);

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: surface,
            apply_limit_buckets: false,
        }))
        .map_err(|e| GpuError::NoAdapter(e.to_string()))?;

        let info = adapter.get_info();
        let format = match (format, surface) {
            (Some(format), _) => format,
            (None, Some(surface)) => {
                let capabilities = surface.get_capabilities(&adapter);
                capabilities
                    .formats
                    .iter()
                    .copied()
                    .find(|f| f.is_srgb())
                    .or_else(|| capabilities.formats.first().copied())
                    .ok_or_else(|| {
                        GpuError::Device("the surface supports no texture format".into())
                    })?
            }
            (None, None) => COLOR_FORMAT,
        };

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cadforge"),
            // Nothing exotic: the baseline feature set is what ships everywhere, including
            // the mobile targets ADR-0006 cares about.
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            ..Default::default()
        }))
        .map_err(|e| GpuError::Device(e.to_string()))?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cadforge shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniforms"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cadforge"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cadforge"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GpuVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Uint32
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Back-face culling is the cheap correctness check: it only looks right if
                // every sweep and every boolean got its winding right.
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // The pick pipeline differs from the shading one only in its fragment entry point and
        // target format. Same geometry, same depth test, so what the user sees and what the
        // user clicks can never disagree.
        let pick_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cadforge pick"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GpuVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Uint32
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_pick"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: PICK_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            pick_pipeline,
            bind_group,
            uniform_buffer,
            format,
            width,
            height,
            backend: format!("{:?}", info.backend),
            adapter_name: info.name,
        })
    }

    /// Which GPU and API actually got picked. Worth reporting: it is the evidence that the
    /// native path works on this platform.
    pub fn adapter_description(&self) -> String {
        format!("{} via {}", self.adapter_name, self.backend)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// The device, so a shell can configure its own surface against it.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Tell the renderer the target changed size. The caller reconfigures its own surface.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    /// A depth buffer matching the current size. Recreate it on every resize — a depth
    /// texture smaller than the colour target is a validation error, not a soft failure.
    pub fn create_depth_view(&self) -> wgpu::TextureView {
        self.device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("depth"),
                size: wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default())
    }

    /// Draw into an existing colour view. This is the whole renderer; everything else is
    /// about where the pixels end up.
    ///
    /// Identical for an offscreen texture, a desktop swapchain, and an iOS `CAMetalLayer` —
    /// which is the claim ADR-0001 rests on.
    pub fn render_to_view(
        &self,
        meshes: &[MeshData<'_>],
        camera: &Camera,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> wgpu::CommandBuffer {
        let (vertices, indices) = flatten(meshes);
        self.write_uniforms(camera);

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.09,
                            g: 0.10,
                            b: 0.12,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if !indices.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
            }
        }
        encoder.finish()
    }

    fn write_uniforms(&self, camera: &Camera) {
        let uniforms = Uniforms {
            view_projection: camera.view_projection_f32().to_cols_array_2d(),
            // Over the viewer's shoulder, so surfaces facing the camera are lit.
            light: {
                let d = (camera.eye() - camera.target)
                    .try_normalize()
                    .unwrap_or(DVec3::Z);
                [d.x as f32, d.y as f32, (d.z.abs() + 0.4) as f32, 0.0]
            },
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Draw the meshes and read the framebuffer back as RGBA8.
    pub fn render(&self, meshes: &[MeshData<'_>], camera: &Camera) -> Result<Vec<u8>, GpuError> {
        let color = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color"),
            size: wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color.create_view(&Default::default());
        let depth_view = self.create_depth_view();

        let draw = self.render_to_view(meshes, camera, &color_view, &depth_view);

        let bytes_per_row = (self.width * 4).div_ceil(COPY_ALIGNMENT) * COPY_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(bytes_per_row) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([draw, encoder.finish()]);

        // Map, wait, and un-pad the rows back to a tight RGBA8 image.
        let slice = readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        receiver
            .recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;

        let padded = slice
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let row_bytes = (self.width * 4) as usize;
        let mut pixels = Vec::with_capacity(row_bytes * self.height as usize);
        for row in 0..self.height as usize {
            let start = row * bytes_per_row as usize;
            pixels.extend_from_slice(&padded[start..start + row_bytes]);
        }
        drop(padded);
        readback.unmap();

        Ok(pixels)
    }

    /// Resolve a pixel to the mesh under it.
    ///
    /// Renders the scene a second time writing identities instead of colour, then reads back
    /// the single pixel asked for. Rendering the whole buffer to read one pixel sounds
    /// wasteful, and is: it happens on click, not per frame, and it is the only approach that
    /// agrees with what is on screen down to the last pixel of a silhouette. Ray casting
    /// against the model would disagree wherever the depth test does.
    ///
    /// Returns `None` for a click on the background or outside the viewport.
    pub fn pick(
        &self,
        meshes: &[MeshData<'_>],
        camera: &Camera,
        x: u32,
        y: u32,
    ) -> Result<Option<FragmentId>, GpuError> {
        if x >= self.width || y >= self.height {
            return Ok(None);
        }

        let (vertices, indices) = flatten(meshes);
        self.write_uniforms(camera);

        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pick"),
            size: wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PICK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());
        let depth_view = self.create_depth_view();

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pick vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pick indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        // One pixel, but the copy still has to respect the 256-byte row alignment.
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pick readback"),
            size: u64::from(COPY_ALIGNMENT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pick"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pick"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Zero is FragmentId::NONE, so cleared background reads as a miss.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if !indices.is_empty() {
                pass.set_pipeline(&self.pick_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
            }
        }

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(COPY_ALIGNMENT),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        receiver
            .recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;

        let mapped = slice
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let id = FragmentId::from_pick_color([mapped[0], mapped[1], mapped[2], mapped[3]]);
        drop(mapped);
        readback.unmap();

        Ok((!id.is_none()).then_some(id))
    }

    /// Render and write a PNG in one step.
    pub fn render_to_png(
        &self,
        meshes: &[MeshData<'_>],
        camera: &Camera,
        path: &std::path::Path,
    ) -> Result<Vec<u8>, GpuError> {
        let pixels = self.render(meshes, camera)?;
        write_png(path, self.width, self.height, &pixels)?;
        Ok(pixels)
    }
}

/// Pack the meshes into one vertex and one index buffer.
///
/// Vertices are not shared between meshes, so each keeps its own colour and its own flat
/// normals. Instancing (`FragmentSet::distinct_geometries`) is the Phase 3b optimisation;
/// correctness first.
fn flatten(meshes: &[MeshData<'_>]) -> (Vec<GpuVertex>, Vec<u32>) {
    let vertex_total: usize = meshes.iter().map(|m| m.positions.len()).sum();
    let index_total: usize = meshes.iter().map(|m| m.indices.len()).sum();
    let mut vertices = Vec::with_capacity(vertex_total);
    let mut indices = Vec::with_capacity(index_total);

    for mesh in meshes {
        let base = vertices.len() as u32;
        for (i, position) in mesh.positions.iter().enumerate() {
            let normal = mesh.normals.get(i).copied().unwrap_or(DVec3::Z);
            vertices.push(GpuVertex {
                // f64 world coordinates narrow to f32 only here, at the very last step
                // before upload — see the crate docs on why the maths stays f64.
                position: [position.x as f32, position.y as f32, position.z as f32],
                normal: [normal.x as f32, normal.y as f32, normal.z as f32],
                color: mesh.color,
                id: mesh.id.0,
            });
        }
        indices.extend(mesh.indices.iter().map(|i| i + base));
    }
    (vertices, indices)
}

/// Write RGBA8 pixels as a PNG.
pub fn write_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<(), GpuError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| GpuError::Image(e.to_string()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| GpuError::Image(e.to_string()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|e| GpuError::Image(e.to_string()))?
        .write_image_data(pixels)
        .map_err(|e| GpuError::Image(e.to_string()))?;
    Ok(())
}

const SHADER: &str = r#"
struct Uniforms {
    view_projection: mat4x4<f32>,
    light: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
    // Flat: an identity must not be smeared across a triangle.
    @location(2) @interpolate(flat) id: u32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) id: u32,
) -> VsOut {
    var out: VsOut;
    out.clip_position = u.view_projection * vec4<f32>(position, 1.0);
    out.normal = normal;
    out.color = color;
    out.id = id;
    return out;
}

// Little-endian bytes of the id, matching FragmentId::to_pick_color on the CPU side. The
// target is Rgba8Unorm, so each channel stores its byte exactly.
@fragment
fn fs_pick(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(
        f32(in.id & 0xFFu) / 255.0,
        f32((in.id >> 8u) & 0xFFu) / 255.0,
        f32((in.id >> 16u) & 0xFFu) / 255.0,
        f32((in.id >> 24u) & 0xFFu) / 255.0,
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(u.light.xyz);
    // Half-lambert, so faces turned away from the light stay readable rather than going
    // black - which matters when you are inspecting a model, not lighting a scene.
    let diffuse = max(dot(n, l), 0.0);
    let wrap = 0.5 + 0.5 * dot(n, l);
    let shade = 0.25 + 0.45 * wrap + 0.30 * diffuse;
    return vec4<f32>(in.color * shade, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit cube, wound counter-clockwise so back-face culling keeps it.
    fn cube() -> (Vec<DVec3>, Vec<DVec3>, Vec<u32>) {
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let h = 0.5;
        let corners = [
            DVec3::new(-h, -h, -h),
            DVec3::new(h, -h, -h),
            DVec3::new(h, h, -h),
            DVec3::new(-h, h, -h),
            DVec3::new(-h, -h, h),
            DVec3::new(h, -h, h),
            DVec3::new(h, h, h),
            DVec3::new(-h, h, h),
        ];
        let faces = [
            [0, 3, 2, 1],
            [4, 5, 6, 7],
            [0, 1, 5, 4],
            [1, 2, 6, 5],
            [2, 3, 7, 6],
            [3, 0, 4, 7],
        ];
        for [a, b, c, d] in faces {
            for tri in [[a, b, c], [a, c, d]] {
                let (p0, p1, p2) = (corners[tri[0]], corners[tri[1]], corners[tri[2]]);
                let normal = (p1 - p0).cross(p2 - p0).normalize();
                for p in [p0, p1, p2] {
                    positions.push(p);
                    normals.push(normal);
                }
            }
        }
        let indices = (0..positions.len() as u32).collect();
        (positions, normals, indices)
    }

    /// Skip rather than fail where no GPU exists — a headless CI box without a software
    /// adapter is a real situation, and it is not a defect in this code.
    fn renderer(width: u32, height: u32) -> Option<Renderer> {
        match Renderer::new_headless(width, height) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("skipping GPU test: {e}");
                None
            }
        }
    }

    #[test]
    fn a_device_comes_up_and_names_its_backend() {
        let Some(renderer) = renderer(64, 64) else {
            return;
        };
        let description = renderer.adapter_description();
        assert!(!description.is_empty());
        assert_eq!(renderer.size(), (64, 64));
        eprintln!("adapter: {description}");
    }

    #[test]
    fn an_empty_scene_renders_as_the_clear_colour() {
        let Some(renderer) = renderer(64, 64) else {
            return;
        };
        let pixels = renderer.render(&[], &Camera::default()).unwrap();
        assert_eq!(pixels.len(), 64 * 64 * 4);
        // Every pixel identical, and fully opaque.
        let first = &pixels[0..4];
        assert!(pixels.chunks_exact(4).all(|p| p == first));
        assert_eq!(first[3], 255);
    }

    #[test]
    fn a_cube_actually_appears_in_the_framebuffer() {
        let Some(renderer) = renderer(256, 256) else {
            return;
        };
        let (positions, normals, indices) = cube();
        let mut camera = Camera::default();
        camera.set_viewport(256, 256);
        camera.frame(&cadforge_core::BoundingBox::new(
            DVec3::splat(-0.5),
            DVec3::splat(0.5),
        ));

        let pixels = renderer
            .render(
                &[MeshData {
                    positions: &positions,
                    normals: &normals,
                    indices: &indices,
                    color: [0.85, 0.55, 0.25],
                    id: FragmentId::NONE,
                }],
                &camera,
            )
            .unwrap();

        // The clear colour is a dark blue-grey; the cube is orange. Count pixels where red
        // clearly dominates blue, which the background can never satisfy.
        let lit = pixels
            .chunks_exact(4)
            .filter(|p| p[0] > 60 && p[0] > p[2] + 30)
            .count();
        assert!(
            lit > 256 * 256 / 20,
            "expected the cube to cover a meaningful area, got {lit} pixels"
        );

        // A framed cube must not fill the whole viewport either.
        assert!(lit < 256 * 256, "the cube should not cover everything");
    }

    #[test]
    fn rendering_is_deterministic() {
        let Some(renderer) = renderer(128, 128) else {
            return;
        };
        let (positions, normals, indices) = cube();
        let mesh = MeshData {
            positions: &positions,
            normals: &normals,
            indices: &indices,
            color: [0.8, 0.4, 0.2],
            id: FragmentId::NONE,
        };
        let camera = Camera::default();
        let a = renderer.render(&[mesh], &camera).unwrap();
        let b = renderer.render(&[mesh], &camera).unwrap();
        assert_eq!(a, b, "the same scene must produce the same frame");
    }

    #[test]
    fn back_faces_are_culled() {
        // A single triangle facing the camera is drawn; the same triangle wound the other way
        // is not. That is the property that makes a correctly-wound sweep or boolean show up
        // and an inside-out one obviously wrong.
        //
        // Note what this test does NOT do: flip a closed cube and expect it to vanish. It
        // would not. Reversing the winding culls the near faces and reveals the far ones, so
        // an inside-out cube is still perfectly visible — you are just looking at the inside
        // of its back wall. The first version of this test asserted the empty framebuffer and
        // was simply wrong about the geometry.
        let Some(renderer) = renderer(128, 128) else {
            return;
        };
        let camera = Camera::default();
        let eye = camera.eye();
        let forward = (camera.target - eye).normalize();
        let right = forward.cross(DVec3::Z).normalize();
        let up = right.cross(forward);
        let centre = camera.target;

        // Wound so that (p1 - p0) x (p2 - p0) points back along -forward, i.e. at the camera.
        let facing = [centre - right - up, centre + right - up, centre + up * 1.5];
        let normal = (facing[1] - facing[0]).cross(facing[2] - facing[0]);
        assert!(
            normal.dot(eye - centre) > 0.0,
            "test setup is wrong: this winding does not face the camera"
        );

        let count_lit = |positions: &[DVec3], indices: &[u32]| {
            let normals = vec![normal.normalize(); positions.len()];
            let pixels = renderer
                .render(
                    &[MeshData {
                        positions,
                        normals: &normals,
                        indices,
                        color: [0.9, 0.6, 0.2],
                        id: FragmentId::NONE,
                    }],
                    &camera,
                )
                .unwrap();
            pixels
                .chunks_exact(4)
                .filter(|p| p[0] > 60 && p[0] > p[2] + 30)
                .count()
        };

        let front = count_lit(&facing, &[0, 1, 2]);
        let back = count_lit(&facing, &[0, 2, 1]);

        // A ~2 m triangle at the default 10 m orbit distance covers a few hundred pixels of
        // a 128x128 frame. The contrast that matters is "clearly drawn" against "exactly
        // zero", not the absolute count.
        assert!(
            front > 100,
            "a camera-facing triangle should be drawn, got {front}"
        );
        assert_eq!(
            back, 0,
            "the same triangle wound away should be culled entirely"
        );
    }

    #[test]
    fn an_inside_out_solid_renders_differently_from_a_correct_one() {
        // The practical statement about a closed solid: flipping its winding does not make it
        // disappear, it makes it render the wrong faces. Different frame, same silhouette.
        let Some(renderer) = renderer(128, 128) else {
            return;
        };
        let (positions, normals, indices) = cube();
        let mut flipped = indices.clone();
        for tri in flipped.chunks_exact_mut(3) {
            tri.swap(1, 2);
        }

        let mut camera = Camera::default();
        camera.set_viewport(128, 128);
        camera.frame(&cadforge_core::BoundingBox::new(
            DVec3::splat(-0.5),
            DVec3::splat(0.5),
        ));

        let render = |indices: &[u32]| {
            renderer
                .render(
                    &[MeshData {
                        positions: &positions,
                        normals: &normals,
                        indices,
                        color: [0.85, 0.55, 0.25],
                        id: FragmentId::NONE,
                    }],
                    &camera,
                )
                .unwrap()
        };

        let correct = render(&indices);
        let inverted = render(&flipped);
        assert_ne!(correct, inverted, "winding must change what is drawn");

        // Both cover roughly the same silhouette; only the visible faces differ.
        let covered = |pixels: &[u8]| {
            pixels
                .chunks_exact(4)
                .filter(|p| p[0] > 60 && p[0] > p[2] + 30)
                .count()
        };
        let (a, b) = (covered(&correct), covered(&inverted));
        assert!(a > 500 && b > 500, "both should be visible: {a} vs {b}");
    }

    /// A cube translated along X, tagged with an identity.
    fn tagged_cube(offset: f64, id: u32) -> (Vec<DVec3>, Vec<DVec3>, Vec<u32>, FragmentId) {
        let (positions, normals, indices) = cube();
        let moved = positions
            .iter()
            .map(|p| *p + DVec3::new(offset, 0.0, 0.0))
            .collect();
        (moved, normals, indices, FragmentId(id))
    }

    #[test]
    fn a_pick_resolves_to_the_mesh_under_the_pixel() {
        let Some(renderer) = renderer(256, 256) else {
            return;
        };
        let (left_pos, left_norm, left_idx, left_id) = tagged_cube(-2.0, 7);
        let (right_pos, right_norm, right_idx, right_id) = tagged_cube(2.0, 9);

        let meshes = [
            MeshData {
                positions: &left_pos,
                normals: &left_norm,
                indices: &left_idx,
                color: [0.8, 0.3, 0.3],
                id: left_id,
            },
            MeshData {
                positions: &right_pos,
                normals: &right_norm,
                indices: &right_idx,
                color: [0.3, 0.3, 0.8],
                id: right_id,
            },
        ];

        let mut camera = Camera::default();
        camera.set_viewport(256, 256);
        camera.frame(&cadforge_core::BoundingBox::new(
            DVec3::new(-2.5, -0.5, -0.5),
            DVec3::new(2.5, 0.5, 0.5),
        ));

        // Project each cube's centre to a pixel rather than guessing at quarter-width. The
        // first version picked at 64 and 192 and missed both cubes, which says more about
        // guessing than about picking.
        let project = |point: DVec3| -> (u32, u32) {
            let clip = camera.view_projection() * point.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            (
                ((ndc.x * 0.5 + 0.5) * 256.0) as u32,
                ((0.5 - ndc.y * 0.5) * 256.0) as u32,
            )
        };

        let (lx, ly) = project(DVec3::new(-2.0, 0.0, 0.0));
        let (rx, ry) = project(DVec3::new(2.0, 0.0, 0.0));

        assert_eq!(
            renderer.pick(&meshes, &camera, lx, ly).unwrap(),
            Some(left_id),
            "the cube at x = -2 should pick as itself"
        );
        assert_eq!(
            renderer.pick(&meshes, &camera, rx, ry).unwrap(),
            Some(right_id),
            "the cube at x = 2 should pick as itself"
        );
    }

    #[test]
    fn a_pick_on_the_background_is_a_miss() {
        // The pick target clears to zero, which is FragmentId::NONE, so empty space must not
        // resolve to a real fragment.
        let Some(renderer) = renderer(256, 256) else {
            return;
        };
        let (positions, normals, indices, id) = tagged_cube(0.0, 5);
        let meshes = [MeshData {
            positions: &positions,
            normals: &normals,
            indices: &indices,
            color: [0.8, 0.5, 0.2],
            id,
        }];

        let mut camera = Camera::default();
        camera.set_viewport(256, 256);
        camera.frame(&cadforge_core::BoundingBox::new(
            DVec3::splat(-0.5),
            DVec3::splat(0.5),
        ));

        assert_eq!(renderer.pick(&meshes, &camera, 2, 2).unwrap(), None);
        assert_eq!(renderer.pick(&meshes, &camera, 128, 128).unwrap(), Some(id));
    }

    #[test]
    fn a_pick_outside_the_viewport_is_a_miss_not_a_panic() {
        let Some(renderer) = renderer(128, 128) else {
            return;
        };
        assert_eq!(
            renderer.pick(&[], &Camera::default(), 500, 500).unwrap(),
            None
        );
        assert_eq!(renderer.pick(&[], &Camera::default(), 0, 0).unwrap(), None);
    }

    #[test]
    fn the_nearer_surface_wins() {
        // Same pixel, two cubes, one in front. The pick pass shares the shading pass depth
        // test, so what you click can never disagree with what you see.
        let Some(renderer) = renderer(256, 256) else {
            return;
        };
        let mut camera = Camera::default();
        camera.set_viewport(256, 256);
        camera.target = DVec3::ZERO;
        camera.distance = 8.0;

        // One cube at the origin, one pushed directly away from the camera behind it.
        let behind = (camera.target - camera.eye()).normalize() * 3.0;
        let (front_pos, front_norm, front_idx, front_id) = tagged_cube(0.0, 11);
        let (raw_pos, back_norm, back_idx, back_id) = tagged_cube(0.0, 13);
        let back_pos: Vec<DVec3> = raw_pos.iter().map(|p| *p + behind).collect();

        let meshes = [
            MeshData {
                positions: &back_pos,
                normals: &back_norm,
                indices: &back_idx,
                color: [0.3, 0.3, 0.8],
                id: back_id,
            },
            // Drawn second, but depth decides — not draw order.
            MeshData {
                positions: &front_pos,
                normals: &front_norm,
                indices: &front_idx,
                color: [0.8, 0.3, 0.3],
                id: front_id,
            },
        ];

        assert_eq!(
            renderer.pick(&meshes, &camera, 128, 128).unwrap(),
            Some(front_id),
            "the nearer cube should win the depth test"
        );
    }

    #[test]
    fn an_unpickable_mesh_reads_as_background() {
        // FragmentId::NONE means "drawn but not selectable" — a grid, a gizmo, a ghost.
        let Some(renderer) = renderer(128, 128) else {
            return;
        };
        let (positions, normals, indices, _) = tagged_cube(0.0, 0);
        let meshes = [MeshData {
            positions: &positions,
            normals: &normals,
            indices: &indices,
            color: [0.5, 0.5, 0.5],
            id: FragmentId::NONE,
        }];

        let mut camera = Camera::default();
        camera.set_viewport(128, 128);
        camera.frame(&cadforge_core::BoundingBox::new(
            DVec3::splat(-0.5),
            DVec3::splat(0.5),
        ));
        assert_eq!(renderer.pick(&meshes, &camera, 64, 64).unwrap(), None);
    }

    #[test]
    fn picking_is_deterministic() {
        let Some(renderer) = renderer(128, 128) else {
            return;
        };
        let (positions, normals, indices, id) = tagged_cube(0.0, 42);
        let meshes = [MeshData {
            positions: &positions,
            normals: &normals,
            indices: &indices,
            color: [0.8, 0.5, 0.2],
            id,
        }];
        let mut camera = Camera::default();
        camera.set_viewport(128, 128);
        camera.frame(&cadforge_core::BoundingBox::new(
            DVec3::splat(-0.5),
            DVec3::splat(0.5),
        ));

        let first = renderer.pick(&meshes, &camera, 64, 64).unwrap();
        let second = renderer.pick(&meshes, &camera, 64, 64).unwrap();
        assert_eq!(first, second);
        assert_eq!(first, Some(id));
    }

    #[test]
    fn readback_handles_a_width_that_needs_row_padding() {
        // 250 px * 4 bytes = 1000, which is not a multiple of the 256-byte copy alignment.
        // Getting the un-padding wrong here shears the image.
        let Some(renderer) = renderer(250, 64) else {
            return;
        };
        let pixels = renderer.render(&[], &Camera::default()).unwrap();
        assert_eq!(pixels.len(), 250 * 64 * 4);
    }
}
