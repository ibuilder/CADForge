//! Parametric families.
//!
//! A family is a versioned, reusable component definition: typed parameters, named types, a
//! deterministic geometry recipe, and a hosting behaviour. It is what makes a door a *door*
//! rather than a box that happens to sit in a wall.
//!
//! This is the crate that does not exist anywhere else in open AEC (ADR-0005). Viewers,
//! validators, and BCF managers are all built; a family system is not.
//!
//! Two properties are load-bearing:
//!
//! 1. **Recipes are deterministic.** Same parameters, same mesh, on every platform. That is
//!    what makes geometry cacheable by hash and golden-file tests meaningful.
//! 2. **Recipes declare their own export capability.** A single extrusion maps exactly onto
//!    `IfcExtrudedAreaSolid`; anything else degrades to a tessellated representation and says
//!    so ([`RepresentationKind`]). No silent downgrades (ADR-0004).

pub mod family;
pub mod param;
pub mod recipe;

pub use family::{
    FamilyDefinition, FamilyType, HostBehavior, IfcTypeMapping, PlacementRequest,
    RepresentationKind,
};
pub use param::{ParamBag, ParamDef, ParamScope, ParamType, ParamValue};
pub use recipe::{Expr, GeometryRecipe, ProfileSpec, RecipeOp};

/// Everything that can go wrong defining, resolving, or evaluating a family.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FamilyError {
    #[error("no parameter named {0:?}")]
    UnknownParameter(String),

    #[error("parameter {name:?} expects {expected}, got {actual}")]
    TypeMismatch {
        name: String,
        expected: &'static str,
        actual: &'static str,
    },

    #[error("parameter {name:?} = {value} is outside {min}..={max}")]
    OutOfRange {
        name: String,
        value: f64,
        min: f64,
        max: f64,
    },

    #[error("parameter {0:?} is type-scoped and cannot be overridden per instance")]
    NotInstanceScoped(String),

    #[error("no family type named {0:?}")]
    UnknownType(String),

    #[error("duplicate parameter {0:?}")]
    DuplicateParameter(String),

    #[error("recipe step {index} refers to step {referenced}, which is not defined before it")]
    BadRecipeReference { index: usize, referenced: usize },

    #[error("recipe output step {0} does not exist")]
    BadRecipeOutput(usize),

    #[error("recipe has no steps")]
    EmptyRecipe,

    #[error("parameter {0:?} is not numeric and cannot be used in an expression")]
    NotNumeric(String),

    #[error("division by zero in a parameter expression")]
    DivideByZero,

    #[error("a {behavior} family needs a host element")]
    HostRequired { behavior: &'static str },

    #[error("a {behavior} family cannot be given a host")]
    HostNotAllowed { behavior: &'static str },

    #[error(transparent)]
    Geometry(#[from] cadforge_geom::GeometryError),
}
