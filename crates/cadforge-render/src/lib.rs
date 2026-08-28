//! Render-side types: fragments, camera, culling, picking.
//!
//! Deliberately **GPU-free**. Everything here is maths and bookkeeping that can be tested on
//! a headless machine; the wgpu device, queue, and pipelines arrive in Phase 3 as a thin
//! layer on top (PLAN.md §7).
//!
//! The renderer never owns truth. It consumes [`RenderFragment`]s and returns picks. Delete
//! every fragment in the system and the model is untouched (`docs/ifc-semantics.md` ADR-001).
//!
//! Maths is `f64` here and narrowed to `f32` only at upload. A site coordinate can be
//! hundreds of thousands of metres from the origin — georeferenced models routinely are — and
//! doing camera maths in `f32` at that distance produces visible jitter.

pub mod camera;
pub mod cull;
pub mod fragment;
pub mod section;

#[cfg(feature = "gpu")]
pub mod gpu;

pub use camera::Camera;
pub use cull::Frustum;
pub use fragment::{FragmentId, FragmentSet, GeometrySource, RenderFragment};
pub use section::{SectionPlane, MAX_SECTIONS};

#[cfg(feature = "gpu")]
pub use gpu::{GpuError, MeshData, Renderer};
