//! Authoring tools — the interaction model.
//!
//! Phase 5. The commands, the geometry, and the renderer all existed before this crate; what
//! was missing was the part that turns *clicks* into them. That part is a state machine, and
//! a state machine does not need a window, so it does not live in the shell.
//!
//! The boundary this crate defends: **a tool never mutates a model.** It accumulates picked
//! points and returns [`ModelCommand`](cadforge_core::ModelCommand)s for the caller to apply.
//! Everything undo already knows how to reverse therefore keeps working, and a wall drawn by
//! hand is indistinguishable from one authored in code — which is exactly the property that
//! makes the demo pipeline a real test of the interactive path.
//!
//! ```
//! use cadforge_core::{IfcClass, Model};
//! use cadforge_tools::{Draft, DraftOutcome, Tool};
//! use glam::DVec3;
//!
//! let mut model = Model::new();
//! let mut draft = Draft::default();
//! draft.set_tool(Tool::Wall);
//!
//! draft.click(DVec3::ZERO, &[]).unwrap();
//! let DraftOutcome::Commit { commands, .. } = draft.click(DVec3::new(4.0, 0.0, 0.0), &[]).unwrap()
//! else {
//!     panic!("the second click finishes a wall");
//! };
//!
//! model.apply_all(commands).unwrap();
//! assert_eq!(model.by_class(&IfcClass::Wall).count(), 1);
//!
//! // And it is a normal edit, so it undoes like one.
//! model.undo().unwrap();
//! assert!(model.is_empty());
//! ```

pub mod candidates;
pub mod draft;

pub use candidates::{snap_candidates, world_transform};
pub use draft::{ContainerRef, Draft, DraftOutcome, DraftSettings, Tool, ToolError};
