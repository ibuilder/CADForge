//! The IFC boundary.
//!
//! CADForge does not implement an IFC parser and does not marry one either. Every IFC library
//! lives behind [`IfcBackend`], and no third-party IFC type is allowed into a `cadforge-core`
//! signature (ADR-0003).
//!
//! That indirection is not architectural neatness — it is a hedge against a specific risk.
//! The best Rust option, `ifc-lite`, covers IFC2X3 through IFC5 and verifies its geometry
//! against IfcOpenShell at 99.9%+ agreement, but it has roughly 1,157 downloads a month, five
//! dependent crates, and one organisation behind it. The intended desktop escape hatch is an
//! IfcOpenShell subprocess. Both plug in here; the rest of CADForge cannot tell which is in
//! use.
//!
//! Today the registered default is [`UnimplementedBackend`], which fails loudly. Phase 2
//! swaps in a real one (PLAN.md §7).

pub mod backend;
pub mod schema;
pub mod spf;

pub use backend::{
    BackendCapabilities, IfcBackend, ImportReport, ImportWarning, UnimplementedBackend,
};
pub use schema::IfcSchema;
pub use spf::{ExportContext, ExportedType, SpfBackend};

/// Everything that can go wrong crossing the IFC boundary.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IfcError {
    #[error("no IFC backend is configured")]
    NoBackend,

    #[error("backend {backend:?} does not support {schema}")]
    UnsupportedSchema {
        backend: &'static str,
        schema: IfcSchema,
    },

    #[error("backend {backend:?} cannot write IFC, only read it")]
    ReadOnlyBackend { backend: &'static str },

    #[error("backend {backend:?} cannot read IFC, only write it")]
    WriteOnlyBackend { backend: &'static str },

    #[error("could not determine the IFC schema from the file header")]
    UnknownSchema,

    #[error("malformed IFC: {0}")]
    Malformed(String),

    #[error("{0}")]
    Backend(String),
}
