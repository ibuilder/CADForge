//! CADForge geometry.
//!
//! Deliberately **not** a B-rep kernel. There is no production-grade pure-Rust B-rep kernel
//! in 2026 — Fornjot ended without reaching its goals, `truck`'s crates.io releases are two
//! years stale, and `opencascade-rs` needs a C++ toolchain that cannot ship to mobile
//! (ADR-0004, `docs/research/LANDSCAPE.md` §6).
//!
//! So the canonical geometry of an authored element is a **parametric recipe** — a profile
//! swept along a direction — and the mesh here is the derived, disposable result. Walls,
//! slabs, columns, beams, openings, ducts, and pipes are all profile sweeps, which covers
//! the large majority of real building elements.
//!
//! Everything in this crate is deterministic. The same profile and settings produce
//! byte-identical output on every platform, which is what makes meshes cacheable by hash and
//! golden-file tests meaningful.

pub mod bsp;
pub mod csg;
pub mod mesh;
pub mod profile;
pub mod sweep;
pub mod tess;

pub use bsp::{is_edge_manifold, is_watertight, BspCsg};
pub use csg::{CsgBackend, UnavailableCsg};
pub use mesh::IndexedMesh;
pub use profile::Profile;
pub use sweep::{extrude, extrude_along};
pub use tess::TessellationSettings;

/// Everything that can go wrong building geometry.
///
/// Failures are explicit and loud. Silently producing degenerate geometry is the failure mode
/// that corrupts a model file, and it is far more expensive than a rejected command
/// (`docs/ifc-semantics.md` §11 Phase 4).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GeometryError {
    #[error("a closed profile needs at least 3 distinct points, got {0}")]
    TooFewPoints(usize),

    #[error("profile encloses no area")]
    ZeroArea,

    #[error("input contains a non-finite coordinate")]
    NonFinite,

    #[error("extrusion depth must be non-zero and finite, got {0}")]
    InvalidDepth(f64),

    #[error("extrusion direction is degenerate")]
    DegenerateDirection,

    /// Profile holes are not tessellated in v1. Openings are modelled as CSG voids
    /// (`IfcRelVoidsElement`), not as holes in a profile, so this is rare in practice.
    #[error(
        "profiles with holes are not yet tessellated ({0} holes); model the void as an opening"
    )]
    HolesNotSupported(usize),

    #[error("triangulation failed: the polygon is likely self-intersecting")]
    TriangulationFailed,

    /// No CSG backend is wired up. Per ADR-0004 the default backend fails loudly rather than
    /// approximating a boolean.
    #[error("no CSG backend is configured")]
    CsgUnavailable,

    /// A boolean between open surfaces has no meaning, so it is refused rather than
    /// answered with confident nonsense.
    #[error("boolean operand {0} is not a closed solid")]
    OpenOperand(&'static str),

    #[error("boolean operand {0} is empty")]
    EmptyOperand(&'static str),

    #[error("boolean operand {0} is structurally malformed")]
    MalformedOperand(&'static str),
}
