//! CADForge semantic core.
//!
//! This crate is the authority on what a building *is*. Meshes, fragments, and GPU buffers
//! are caches derived from here and are always disposable (see `docs/ifc-semantics.md` ADR-001).
//!
//! Two invariants govern this crate, both enforceable at review time:
//!
//! 1. **No platform dependencies.** It compiles and tests without a window, a GPU, or an
//!    event loop — `cargo test -p cadforge-core` on a headless machine is the check
//!    (ADR-0002).
//! 2. **No third-party IFC types in public signatures.** The IFC library is swappable
//!    precisely because nothing here knows which one is in use (ADR-0003).

pub mod command;
pub mod element;
pub mod id;
pub mod model;
pub mod property;
pub mod representation;
pub mod spatial;

pub use command::{CommandError, CommandOutcome, ModelCommand};
pub use element::{BoundingBox, ElementRecord, IfcClass, Placement};
pub use id::GlobalId;
pub use model::{Model, Revision};
pub use property::{PropertySet, PropertySets, PropertyValue};
pub use representation::Representation;
pub use spatial::{IndexedElement, SpatialIndex};
