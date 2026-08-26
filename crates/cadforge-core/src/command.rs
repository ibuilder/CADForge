//! The command model.
//!
//! Every mutation to a [`Model`](crate::model::Model) is a `ModelCommand`. Nothing else may
//! touch the store. That single rule is what buys undo, audit, replay, deterministic export,
//! and — later — multi-user sync, none of which can be retrofitted onto ad-hoc mutation
//! (`docs/ifc-semantics.md` §5).
//!
//! **Every variant has an exact inverse.** `CreateElement` carries the whole record, so
//! deleting and undoing restores the element byte-for-byte rather than approximately.
//! `SetName` and `SetProperty` take `Option`, so setting and clearing are the same operation
//! in opposite directions.

use crate::element::{ElementRecord, Placement};
use crate::id::GlobalId;
use crate::property::PropertyValue;
use crate::representation::Representation;
use glam::DVec3;
use serde::{Deserialize, Serialize};

/// A single durable edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelCommand {
    /// Insert an element. Boxed because this variant is far larger than the others and would
    /// otherwise inflate every command in the log.
    CreateElement {
        element: Box<ElementRecord>,
    },
    DeleteElement {
        global_id: GlobalId,
    },
    SetName {
        global_id: GlobalId,
        name: Option<String>,
    },
    SetProperty {
        global_id: GlobalId,
        set: String,
        name: String,
        /// `None` clears the property.
        value: Option<PropertyValue>,
    },
    /// Translate an element. Kept distinct from `SetPlacement` because its inverse is exact
    /// without reading the current state.
    MoveElement {
        global_id: GlobalId,
        delta: DVec3,
    },
    SetPlacement {
        global_id: GlobalId,
        placement: Placement,
    },
    /// `IfcRelContainedInSpatialStructure`.
    AssignContainer {
        global_id: GlobalId,
        container: Option<GlobalId>,
    },
    /// Attach evaluated geometry. `None` clears it.
    ///
    /// A command rather than a cache write, unlike bounds: the representation is what gets
    /// exported to IFC, so it is part of the model and belongs in the audit trail.
    SetRepresentation {
        global_id: GlobalId,
        representation: Option<Box<Representation>>,
    },
    /// `IfcRelDefinesByType` — bind an instance to a family type.
    AssignType {
        global_id: GlobalId,
        type_ref: Option<GlobalId>,
    },
    /// `IfcRelVoidsElement` — an opening cuts its host.
    AddVoid {
        host: GlobalId,
        opening: GlobalId,
    },
    RemoveVoid {
        host: GlobalId,
        opening: GlobalId,
    },
    /// `IfcRelFillsElement` — a door or window fills an opening.
    AddFill {
        opening: GlobalId,
        filler: GlobalId,
    },
    RemoveFill {
        opening: GlobalId,
        filler: GlobalId,
    },
}

impl ModelCommand {
    /// The element this command is about, for permission checks and change reporting.
    pub fn primary_target(&self) -> &GlobalId {
        match self {
            Self::CreateElement { element } => &element.global_id,
            Self::DeleteElement { global_id }
            | Self::SetName { global_id, .. }
            | Self::SetProperty { global_id, .. }
            | Self::MoveElement { global_id, .. }
            | Self::SetPlacement { global_id, .. }
            | Self::AssignContainer { global_id, .. }
            | Self::SetRepresentation { global_id, .. }
            | Self::AssignType { global_id, .. } => global_id,
            Self::AddVoid { host, .. } | Self::RemoveVoid { host, .. } => host,
            Self::AddFill { opening, .. } | Self::RemoveFill { opening, .. } => opening,
        }
    }

    /// Whether applying this command requires geometry to be rebuilt.
    ///
    /// A rename must not invalidate a mesh — that distinction is the whole point of tracking
    /// `semantic_revision` separately from `representation_revision`.
    pub fn invalidates_geometry(&self) -> bool {
        match self {
            Self::CreateElement { .. }
            | Self::DeleteElement { .. }
            | Self::MoveElement { .. }
            | Self::SetPlacement { .. }
            | Self::AddVoid { .. }
            | Self::RemoveVoid { .. }
            | Self::AddFill { .. }
            | Self::RemoveFill { .. }
            | Self::SetRepresentation { .. } => true,
            // Type assignment can change geometry via the family, but the family evaluator
            // re-issues explicit geometry commands rather than relying on a side effect.
            Self::SetName { .. }
            | Self::SetProperty { .. }
            | Self::AssignType { .. }
            | Self::AssignContainer { .. } => false,
        }
    }
}

/// What happened when a command was applied.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutcome {
    /// The revision the model is at after applying.
    pub revision: u64,
    /// The command that undoes this one exactly.
    pub inverse: ModelCommand,
    /// Elements whose state changed.
    pub changed: Vec<GlobalId>,
    /// Elements whose cached geometry is now stale.
    pub geometry_invalidated: Vec<GlobalId>,
}

/// Why a command was rejected.
///
/// Commands are validated before they mutate anything: a rejected command leaves the model
/// exactly as it was.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("no element {0}")]
    UnknownElement(GlobalId),

    #[error("element {0} already exists")]
    DuplicateElement(GlobalId),

    #[error("{host} is a {class} and cannot host openings")]
    NotAHost { host: GlobalId, class: String },

    #[error("{0} is not an IfcOpeningElement")]
    NotAnOpening(GlobalId),

    #[error("an element cannot reference itself ({0})")]
    SelfReference(GlobalId),

    #[error("{global_id} is still referenced by {relationship}; remove the relationship first")]
    StillReferenced {
        global_id: GlobalId,
        relationship: &'static str,
    },

    #[error("relationship {relationship} between {a} and {b} does not exist")]
    NoSuchRelationship {
        relationship: &'static str,
        a: GlobalId,
        b: GlobalId,
    },

    #[error("relationship {relationship} between {a} and {b} already exists")]
    RelationshipExists {
        relationship: &'static str,
        a: GlobalId,
        b: GlobalId,
    },

    #[error("a spatial container must be spatial; {0} is not")]
    NotSpatial(GlobalId),

    #[error("the representation for {0} is malformed and would produce an unreadable file")]
    InvalidRepresentation(GlobalId),

    #[error("nothing to undo")]
    NothingToUndo,

    #[error("nothing to redo")]
    NothingToRedo,
}
