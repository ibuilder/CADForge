//! The model store.
//!
//! Holds elements and their relationships, applies commands, and maintains the revision and
//! undo history. Validation happens before mutation, so a rejected command leaves the model
//! untouched.

use crate::command::{CommandError, CommandOutcome, ModelCommand};
use crate::element::{BoundingBox, ElementRecord, IfcClass};
use crate::id::GlobalId;
use crate::spatial::{IndexedElement, SpatialIndex};
use std::collections::{BTreeMap, BTreeSet};

const REL_VOIDS: &str = "IfcRelVoidsElement";
const REL_FILLS: &str = "IfcRelFillsElement";

/// One entry in the audit trail.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub number: u64,
    pub command: ModelCommand,
    pub changed: Vec<GlobalId>,
}

/// The semantic model.
///
/// `BTreeMap`/`BTreeSet` throughout, not hash containers: iteration order must be stable so
/// that export from a given revision is reproducible (`docs/ifc-semantics.md` §7.2).
#[derive(Debug, Clone, Default)]
pub struct Model {
    elements: BTreeMap<GlobalId, ElementRecord>,
    /// `(host, opening)`.
    voids: BTreeSet<(GlobalId, GlobalId)>,
    /// `(opening, filler)`.
    fills: BTreeSet<(GlobalId, GlobalId)>,
    revision: u64,
    history: Vec<Revision>,
    undo_stack: Vec<ModelCommand>,
    redo_stack: Vec<ModelCommand>,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- queries -------------------------------------------------------------------

    pub fn get(&self, id: &GlobalId) -> Option<&ElementRecord> {
        self.elements.get(id)
    }

    pub fn contains(&self, id: &GlobalId) -> bool {
        self.elements.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ElementRecord> {
        self.elements.values()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn history(&self) -> &[Revision] {
        &self.history
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Elements of a class, in stable order.
    pub fn by_class<'a>(&'a self, class: &'a IfcClass) -> impl Iterator<Item = &'a ElementRecord> {
        self.elements.values().filter(move |e| &e.class == class)
    }

    /// Elements contained in a spatial structure element.
    pub fn contained_in<'a>(
        &'a self,
        container: &'a GlobalId,
    ) -> impl Iterator<Item = &'a ElementRecord> {
        self.elements
            .values()
            .filter(move |e| e.container.as_ref() == Some(container))
    }

    /// Every `(host, opening)` pair, in stable order.
    ///
    /// Prefer this over calling [`Model::openings_of`] in a loop: that is a scan per call, so
    /// walking every element with it is quadratic. Exporting a 20k-wall model took 5.9 s that
    /// way and 0.9 s this way.
    pub fn voids(&self) -> impl Iterator<Item = (&GlobalId, &GlobalId)> {
        self.voids.iter().map(|(h, o)| (h, o))
    }

    /// Every `(opening, filler)` pair, in stable order. See [`Model::voids`].
    pub fn fills(&self) -> impl Iterator<Item = (&GlobalId, &GlobalId)> {
        self.fills.iter().map(|(o, f)| (o, f))
    }

    /// Openings cutting a host (`IfcRelVoidsElement`).
    ///
    /// **O(number of voids in the model)** — it scans. Fine for answering a question about
    /// one element, wrong inside a loop over all of them; use [`Model::voids`] there. If this
    /// ever shows up in a profile for single-element queries, the fix is a secondary index
    /// keyed by host, not a change at the call site.
    pub fn openings_of<'a>(&'a self, host: &'a GlobalId) -> impl Iterator<Item = &'a GlobalId> {
        self.voids
            .iter()
            .filter(move |(h, _)| h == host)
            .map(|(_, o)| o)
    }

    /// Elements filling an opening (`IfcRelFillsElement`).
    pub fn fills_of<'a>(&'a self, opening: &'a GlobalId) -> impl Iterator<Item = &'a GlobalId> {
        self.fills
            .iter()
            .filter(move |(o, _)| o == opening)
            .map(|(_, f)| f)
    }

    /// The host an opening cuts, if any.
    pub fn host_of(&self, opening: &GlobalId) -> Option<&GlobalId> {
        self.voids
            .iter()
            .find(|(_, o)| o == opening)
            .map(|(h, _)| h)
    }

    /// World bounds of everything with evaluated geometry.
    pub fn bounds(&self) -> BoundingBox {
        self.elements
            .values()
            .filter_map(|e| e.bounds)
            .fold(BoundingBox::empty(), BoundingBox::union)
    }

    /// Build a spatial index over elements that have bounds.
    ///
    /// Rebuilt rather than maintained incrementally: bulk loading an R-tree is fast, and a
    /// stale index is a far worse failure than a rebuild (`docs/ifc-semantics.md` §4.2).
    pub fn spatial_index(&self) -> SpatialIndex {
        SpatialIndex::build(self.elements.values().filter_map(|e| {
            e.bounds.map(|b| IndexedElement {
                global_id: e.global_id.clone(),
                bounds: b,
            })
        }))
    }

    /// Attach evaluated geometry bounds. Not a command — bounds are a derived cache, and
    /// caches do not belong in the audit trail (`docs/ifc-semantics.md` ADR-001).
    pub fn set_bounds(&mut self, id: &GlobalId, bounds: Option<BoundingBox>) -> bool {
        match self.elements.get_mut(id) {
            Some(e) => {
                e.bounds = bounds;
                true
            }
            None => false,
        }
    }

    // ---- mutation ------------------------------------------------------------------

    /// Apply a command, recording it for undo.
    pub fn apply(&mut self, command: ModelCommand) -> Result<CommandOutcome, CommandError> {
        let outcome = self.execute(command.clone())?;
        self.undo_stack.push(outcome.inverse.clone());
        self.redo_stack.clear();
        self.history.push(Revision {
            number: outcome.revision,
            command,
            changed: outcome.changed.clone(),
        });
        Ok(outcome)
    }

    /// Apply a batch, stopping at the first failure.
    ///
    /// Note this is *not* transactional — earlier commands stay applied. Callers wanting
    /// all-or-nothing should undo back to the starting revision.
    pub fn apply_all(
        &mut self,
        commands: impl IntoIterator<Item = ModelCommand>,
    ) -> Result<Vec<CommandOutcome>, CommandError> {
        commands.into_iter().map(|c| self.apply(c)).collect()
    }

    pub fn undo(&mut self) -> Result<CommandOutcome, CommandError> {
        let inverse = self.undo_stack.pop().ok_or(CommandError::NothingToUndo)?;
        match self.execute(inverse.clone()) {
            Ok(outcome) => {
                self.redo_stack.push(outcome.inverse.clone());
                self.history.push(Revision {
                    number: outcome.revision,
                    command: inverse,
                    changed: outcome.changed.clone(),
                });
                Ok(outcome)
            }
            Err(e) => {
                // Undo must never lose a step. If the inverse was rejected the model has a
                // consistency bug, but the stack stays intact so it can be inspected.
                self.undo_stack.push(inverse);
                Err(e)
            }
        }
    }

    pub fn redo(&mut self) -> Result<CommandOutcome, CommandError> {
        let command = self.redo_stack.pop().ok_or(CommandError::NothingToRedo)?;
        match self.execute(command.clone()) {
            Ok(outcome) => {
                self.undo_stack.push(outcome.inverse.clone());
                self.history.push(Revision {
                    number: outcome.revision,
                    command,
                    changed: outcome.changed.clone(),
                });
                Ok(outcome)
            }
            Err(e) => {
                self.redo_stack.push(command);
                Err(e)
            }
        }
    }

    // ---- execution -----------------------------------------------------------------

    /// Validate, mutate, and produce the inverse. Does not touch the undo stacks — that is
    /// what lets `undo` and `redo` reuse it without recursion.
    fn execute(&mut self, command: ModelCommand) -> Result<CommandOutcome, CommandError> {
        let invalidates = command.invalidates_geometry();
        let (inverse, changed) = match command {
            ModelCommand::CreateElement { element } => {
                let id = element.global_id.clone();
                if self.elements.contains_key(&id) {
                    return Err(CommandError::DuplicateElement(id));
                }
                if let Some(container) = &element.container {
                    self.require_spatial(container)?;
                }
                self.elements.insert(id.clone(), *element);
                (
                    ModelCommand::DeleteElement {
                        global_id: id.clone(),
                    },
                    vec![id],
                )
            }

            ModelCommand::DeleteElement { global_id } => {
                self.require_element(&global_id)?;
                self.require_unreferenced(&global_id)?;
                let element = self
                    .elements
                    .remove(&global_id)
                    .expect("existence checked above");
                (
                    ModelCommand::CreateElement {
                        element: Box::new(element),
                    },
                    vec![global_id],
                )
            }

            ModelCommand::SetName { global_id, name } => {
                let element = self.element_mut(&global_id)?;
                let previous = std::mem::replace(&mut element.name, name);
                element.semantic_revision += 1;
                (
                    ModelCommand::SetName {
                        global_id: global_id.clone(),
                        name: previous,
                    },
                    vec![global_id],
                )
            }

            ModelCommand::SetProperty {
                global_id,
                set,
                name,
                value,
            } => {
                let element = self.element_mut(&global_id)?;
                let previous = element.properties.set(&set, &name, value);
                element.semantic_revision += 1;
                (
                    ModelCommand::SetProperty {
                        global_id: global_id.clone(),
                        set,
                        name,
                        value: previous,
                    },
                    vec![global_id],
                )
            }

            ModelCommand::MoveElement { global_id, delta } => {
                let element = self.element_mut(&global_id)?;
                element.placement = element.placement.translated(delta);
                element.representation_revision += 1;
                (
                    ModelCommand::MoveElement {
                        global_id: global_id.clone(),
                        delta: -delta,
                    },
                    vec![global_id],
                )
            }

            ModelCommand::SetPlacement {
                global_id,
                placement,
            } => {
                let element = self.element_mut(&global_id)?;
                let previous = std::mem::replace(&mut element.placement, placement);
                element.representation_revision += 1;
                (
                    ModelCommand::SetPlacement {
                        global_id: global_id.clone(),
                        placement: previous,
                    },
                    vec![global_id],
                )
            }

            ModelCommand::AssignContainer {
                global_id,
                container,
            } => {
                self.require_element(&global_id)?;
                if let Some(c) = &container {
                    if c == &global_id {
                        return Err(CommandError::SelfReference(global_id));
                    }
                    self.require_spatial(c)?;
                }
                let element = self.element_mut(&global_id)?;
                let previous = std::mem::replace(&mut element.container, container);
                element.semantic_revision += 1;
                (
                    ModelCommand::AssignContainer {
                        global_id: global_id.clone(),
                        container: previous,
                    },
                    vec![global_id],
                )
            }

            ModelCommand::SetRepresentation {
                global_id,
                representation,
            } => {
                if let Some(r) = &representation {
                    if !r.is_valid() {
                        return Err(CommandError::InvalidRepresentation(global_id));
                    }
                }
                let element = self.element_mut(&global_id)?;
                let previous =
                    std::mem::replace(&mut element.representation, representation.map(|r| *r));
                element.representation_revision += 1;
                (
                    ModelCommand::SetRepresentation {
                        global_id: global_id.clone(),
                        representation: previous.map(Box::new),
                    },
                    vec![global_id],
                )
            }

            ModelCommand::AssignType {
                global_id,
                type_ref,
            } => {
                if type_ref.as_ref() == Some(&global_id) {
                    return Err(CommandError::SelfReference(global_id));
                }
                // The type may be a family type held outside the element store, so its
                // existence is deliberately not checked here (ADR-0005).
                let element = self.element_mut(&global_id)?;
                let previous = std::mem::replace(&mut element.type_ref, type_ref);
                element.semantic_revision += 1;
                (
                    ModelCommand::AssignType {
                        global_id: global_id.clone(),
                        type_ref: previous,
                    },
                    vec![global_id],
                )
            }

            ModelCommand::AddVoid { host, opening } => {
                self.validate_void(&host, &opening)?;
                if !self.voids.insert((host.clone(), opening.clone())) {
                    return Err(CommandError::RelationshipExists {
                        relationship: REL_VOIDS,
                        a: host,
                        b: opening,
                    });
                }
                self.bump_representation(&host);
                self.bump_representation(&opening);
                (
                    ModelCommand::RemoveVoid {
                        host: host.clone(),
                        opening: opening.clone(),
                    },
                    vec![host, opening],
                )
            }

            ModelCommand::RemoveVoid { host, opening } => {
                if !self.voids.remove(&(host.clone(), opening.clone())) {
                    return Err(CommandError::NoSuchRelationship {
                        relationship: REL_VOIDS,
                        a: host,
                        b: opening,
                    });
                }
                self.bump_representation(&host);
                self.bump_representation(&opening);
                (
                    ModelCommand::AddVoid {
                        host: host.clone(),
                        opening: opening.clone(),
                    },
                    vec![host, opening],
                )
            }

            ModelCommand::AddFill { opening, filler } => {
                self.validate_fill(&opening, &filler)?;
                if !self.fills.insert((opening.clone(), filler.clone())) {
                    return Err(CommandError::RelationshipExists {
                        relationship: REL_FILLS,
                        a: opening,
                        b: filler,
                    });
                }
                self.bump_representation(&filler);
                (
                    ModelCommand::RemoveFill {
                        opening: opening.clone(),
                        filler: filler.clone(),
                    },
                    vec![opening, filler],
                )
            }

            ModelCommand::RemoveFill { opening, filler } => {
                if !self.fills.remove(&(opening.clone(), filler.clone())) {
                    return Err(CommandError::NoSuchRelationship {
                        relationship: REL_FILLS,
                        a: opening,
                        b: filler,
                    });
                }
                self.bump_representation(&filler);
                (
                    ModelCommand::AddFill {
                        opening: opening.clone(),
                        filler: filler.clone(),
                    },
                    vec![opening, filler],
                )
            }
        };

        self.revision += 1;
        let geometry_invalidated = if invalidates {
            changed.clone()
        } else {
            Vec::new()
        };
        Ok(CommandOutcome {
            revision: self.revision,
            inverse,
            changed,
            geometry_invalidated,
        })
    }

    // ---- validation helpers --------------------------------------------------------

    fn require_element(&self, id: &GlobalId) -> Result<&ElementRecord, CommandError> {
        self.elements
            .get(id)
            .ok_or_else(|| CommandError::UnknownElement(id.clone()))
    }

    fn element_mut(&mut self, id: &GlobalId) -> Result<&mut ElementRecord, CommandError> {
        self.elements
            .get_mut(id)
            .ok_or_else(|| CommandError::UnknownElement(id.clone()))
    }

    fn require_spatial(&self, id: &GlobalId) -> Result<(), CommandError> {
        let element = self.require_element(id)?;
        if element.class.is_spatial() {
            Ok(())
        } else {
            Err(CommandError::NotSpatial(id.clone()))
        }
    }

    /// Refuse to delete anything still referenced.
    ///
    /// Cascading deletes would make exact inversion impossible — undo would have to restore
    /// an unbounded set of relationships. Forcing the caller to unwind explicitly keeps every
    /// command invertible (`docs/ifc-semantics.md` §11 Phase 3: deletions must not orphan
    /// relationships).
    fn require_unreferenced(&self, id: &GlobalId) -> Result<(), CommandError> {
        if self.voids.iter().any(|(h, o)| h == id || o == id) {
            return Err(CommandError::StillReferenced {
                global_id: id.clone(),
                relationship: REL_VOIDS,
            });
        }
        if self.fills.iter().any(|(o, f)| o == id || f == id) {
            return Err(CommandError::StillReferenced {
                global_id: id.clone(),
                relationship: REL_FILLS,
            });
        }
        if self
            .elements
            .values()
            .any(|e| e.container.as_ref() == Some(id))
        {
            return Err(CommandError::StillReferenced {
                global_id: id.clone(),
                relationship: "IfcRelContainedInSpatialStructure",
            });
        }
        if self
            .elements
            .values()
            .any(|e| e.type_ref.as_ref() == Some(id))
        {
            return Err(CommandError::StillReferenced {
                global_id: id.clone(),
                relationship: "IfcRelDefinesByType",
            });
        }
        Ok(())
    }

    fn validate_void(&self, host: &GlobalId, opening: &GlobalId) -> Result<(), CommandError> {
        if host == opening {
            return Err(CommandError::SelfReference(host.clone()));
        }
        let host_element = self.require_element(host)?;
        if !host_element.class.can_host_openings() {
            return Err(CommandError::NotAHost {
                host: host.clone(),
                class: host_element.class.ifc_name().to_owned(),
            });
        }
        let opening_element = self.require_element(opening)?;
        if opening_element.class != IfcClass::OpeningElement {
            return Err(CommandError::NotAnOpening(opening.clone()));
        }
        Ok(())
    }

    fn validate_fill(&self, opening: &GlobalId, filler: &GlobalId) -> Result<(), CommandError> {
        if opening == filler {
            return Err(CommandError::SelfReference(opening.clone()));
        }
        let opening_element = self.require_element(opening)?;
        if opening_element.class != IfcClass::OpeningElement {
            return Err(CommandError::NotAnOpening(opening.clone()));
        }
        self.require_element(filler)?;
        Ok(())
    }

    fn bump_representation(&mut self, id: &GlobalId) {
        if let Some(e) = self.elements.get_mut(id) {
            e.representation_revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Placement;
    use crate::property::PropertyValue;
    use glam::DVec3;

    fn element(class: IfcClass) -> ElementRecord {
        ElementRecord::new(GlobalId::new(), class)
    }

    fn create(element: ElementRecord) -> ModelCommand {
        ModelCommand::CreateElement {
            element: Box::new(element),
        }
    }

    /// Storey, wall, opening, door — the smallest model with every relationship in it.
    fn wall_with_door() -> (Model, GlobalId, GlobalId, GlobalId, GlobalId) {
        let mut m = Model::new();
        let storey = element(IfcClass::BuildingStorey);
        let wall = element(IfcClass::Wall);
        let opening = element(IfcClass::OpeningElement);
        let door = element(IfcClass::Door);
        let (s, w, o, d) = (
            storey.global_id.clone(),
            wall.global_id.clone(),
            opening.global_id.clone(),
            door.global_id.clone(),
        );

        m.apply(create(storey)).unwrap();
        m.apply(create(wall.with_container(s.clone()))).unwrap();
        m.apply(create(opening)).unwrap();
        m.apply(create(door)).unwrap();
        m.apply(ModelCommand::AddVoid {
            host: w.clone(),
            opening: o.clone(),
        })
        .unwrap();
        m.apply(ModelCommand::AddFill {
            opening: o.clone(),
            filler: d.clone(),
        })
        .unwrap();
        (m, s, w, o, d)
    }

    #[test]
    fn create_assigns_a_revision_and_is_queryable() {
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        let id = e.global_id.clone();

        let outcome = m.apply(create(e)).unwrap();
        assert_eq!(outcome.revision, 1);
        assert_eq!(outcome.changed, vec![id.clone()]);
        assert_eq!(outcome.geometry_invalidated, vec![id.clone()]);
        assert!(m.contains(&id));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn duplicate_create_is_rejected_without_mutating() {
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        m.apply(create(e.clone())).unwrap();
        let revision = m.revision();

        assert!(matches!(
            m.apply(create(e)),
            Err(CommandError::DuplicateElement(_))
        ));
        assert_eq!(
            m.revision(),
            revision,
            "a rejected command must not advance the revision"
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn every_command_inverts_exactly() {
        let (mut m, _s, w, o, d) = wall_with_door();
        let baseline = m.clone();

        let commands = vec![
            ModelCommand::SetName {
                global_id: w.clone(),
                name: Some("Exterior wall".into()),
            },
            ModelCommand::SetProperty {
                global_id: w.clone(),
                set: "Pset_WallCommon".into(),
                name: "IsExternal".into(),
                value: Some(PropertyValue::Boolean(true)),
            },
            ModelCommand::MoveElement {
                global_id: w.clone(),
                delta: DVec3::new(1.5, -2.0, 0.25),
            },
            ModelCommand::SetPlacement {
                global_id: d.clone(),
                placement: Placement::at(DVec3::new(3.0, 0.0, 0.0)),
            },
            ModelCommand::RemoveFill {
                opening: o.clone(),
                filler: d.clone(),
            },
            ModelCommand::AssignType {
                global_id: d.clone(),
                type_ref: Some(GlobalId::new()),
            },
            ModelCommand::SetRepresentation {
                global_id: w.clone(),
                representation: Some(Box::new(crate::Representation::extrusion(
                    vec![[0.0, 0.0], [4.0, 0.0], [4.0, 0.2], [0.0, 0.2]],
                    [0.0, 0.0, 1.0],
                    3.0,
                ))),
            },
        ];
        let count = commands.len();
        m.apply_all(commands).unwrap();

        assert_ne!(
            m.elements, baseline.elements,
            "the batch must actually change something"
        );

        for _ in 0..count {
            m.undo().unwrap();
        }

        // Revision counters advance — history is append-only — but the state must match.
        assert_eq!(m.elements.len(), baseline.elements.len());
        for (id, element) in &baseline.elements {
            let restored = m.get(id).expect("element restored");
            assert_eq!(restored.name, element.name);
            assert_eq!(restored.placement, element.placement);
            assert_eq!(restored.properties, element.properties);
            assert_eq!(restored.representation, element.representation);
            assert_eq!(restored.container, element.container);
            assert_eq!(restored.type_ref, element.type_ref);
        }
        assert_eq!(m.voids, baseline.voids);
        assert_eq!(m.fills, baseline.fills);
    }

    #[test]
    fn undo_then_redo_returns_to_the_edited_state() {
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        let id = e.global_id.clone();
        m.apply(create(e)).unwrap();
        m.apply(ModelCommand::SetName {
            global_id: id.clone(),
            name: Some("W-01".into()),
        })
        .unwrap();

        m.undo().unwrap();
        assert_eq!(m.get(&id).unwrap().name, None);
        assert!(m.can_redo());

        m.redo().unwrap();
        assert_eq!(m.get(&id).unwrap().name.as_deref(), Some("W-01"));
    }

    #[test]
    fn a_new_command_clears_the_redo_stack() {
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        let id = e.global_id.clone();
        m.apply(create(e)).unwrap();
        m.apply(ModelCommand::SetName {
            global_id: id.clone(),
            name: Some("W-01".into()),
        })
        .unwrap();
        m.undo().unwrap();
        assert!(m.can_redo());

        m.apply(ModelCommand::SetName {
            global_id: id,
            name: Some("W-02".into()),
        })
        .unwrap();
        assert!(
            !m.can_redo(),
            "branching history must discard the abandoned future"
        );
    }

    #[test]
    fn undo_on_an_empty_stack_is_an_error_not_a_panic() {
        let mut m = Model::new();
        assert_eq!(m.undo(), Err(CommandError::NothingToUndo));
        assert_eq!(m.redo(), Err(CommandError::NothingToRedo));
    }

    #[test]
    fn renaming_does_not_invalidate_geometry() {
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        let id = e.global_id.clone();
        m.apply(create(e)).unwrap();
        let representation = m.get(&id).unwrap().representation_revision;

        let outcome = m
            .apply(ModelCommand::SetName {
                global_id: id.clone(),
                name: Some("W-01".into()),
            })
            .unwrap();

        assert!(outcome.geometry_invalidated.is_empty());
        assert_eq!(m.get(&id).unwrap().representation_revision, representation);
        assert_eq!(m.get(&id).unwrap().semantic_revision, 1);
    }

    #[test]
    fn moving_invalidates_geometry() {
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        let id = e.global_id.clone();
        m.apply(create(e)).unwrap();

        let outcome = m
            .apply(ModelCommand::MoveElement {
                global_id: id.clone(),
                delta: DVec3::X,
            })
            .unwrap();

        assert_eq!(outcome.geometry_invalidated, vec![id.clone()]);
        assert_eq!(m.get(&id).unwrap().semantic_revision, 0);
        assert!(m.get(&id).unwrap().representation_revision > 0);
    }

    #[test]
    fn only_hosts_take_openings_and_only_openings_are_taken() {
        let mut m = Model::new();
        let furniture = element(IfcClass::Furniture);
        let wall = element(IfcClass::Wall);
        let opening = element(IfcClass::OpeningElement);
        let (f, w, o) = (
            furniture.global_id.clone(),
            wall.global_id.clone(),
            opening.global_id.clone(),
        );
        m.apply_all([create(furniture), create(wall), create(opening)])
            .unwrap();

        assert!(matches!(
            m.apply(ModelCommand::AddVoid {
                host: f,
                opening: o.clone()
            }),
            Err(CommandError::NotAHost { .. })
        ));
        assert!(matches!(
            m.apply(ModelCommand::AddVoid {
                host: w.clone(),
                opening: w.clone()
            }),
            Err(CommandError::SelfReference(_))
        ));
        m.apply(ModelCommand::AddVoid {
            host: w.clone(),
            opening: o.clone(),
        })
        .unwrap();
        assert!(matches!(
            m.apply(ModelCommand::AddVoid {
                host: w,
                opening: o
            }),
            Err(CommandError::RelationshipExists { .. })
        ));
    }

    #[test]
    fn deleting_a_referenced_element_is_refused() {
        let (mut m, storey, wall, opening, _door) = wall_with_door();

        // The wall hosts an opening.
        assert!(matches!(
            m.apply(ModelCommand::DeleteElement {
                global_id: wall.clone()
            }),
            Err(CommandError::StillReferenced { .. })
        ));
        // The storey contains the wall.
        assert!(matches!(
            m.apply(ModelCommand::DeleteElement { global_id: storey }),
            Err(CommandError::StillReferenced { .. })
        ));

        // Unwind explicitly, and it becomes possible.
        let door = m.fills_of(&opening).next().cloned().unwrap();
        m.apply(ModelCommand::RemoveFill {
            opening: opening.clone(),
            filler: door,
        })
        .unwrap();
        m.apply(ModelCommand::RemoveVoid {
            host: wall.clone(),
            opening,
        })
        .unwrap();
        m.apply(ModelCommand::AssignContainer {
            global_id: wall.clone(),
            container: None,
        })
        .unwrap();
        m.apply(ModelCommand::DeleteElement {
            global_id: wall.clone(),
        })
        .unwrap();
        assert!(!m.contains(&wall));
    }

    #[test]
    fn containers_must_be_spatial() {
        let mut m = Model::new();
        let wall = element(IfcClass::Wall);
        let door = element(IfcClass::Door);
        let (w, d) = (wall.global_id.clone(), door.global_id.clone());
        m.apply_all([create(wall), create(door)]).unwrap();

        assert!(matches!(
            m.apply(ModelCommand::AssignContainer {
                global_id: d,
                container: Some(w.clone()),
            }),
            Err(CommandError::NotSpatial(_))
        ));
        assert!(matches!(
            m.apply(ModelCommand::AssignContainer {
                global_id: w.clone(),
                container: Some(w),
            }),
            Err(CommandError::SelfReference(_))
        ));
    }

    #[test]
    fn relationships_are_navigable_in_both_directions() {
        let (m, _s, wall, opening, door) = wall_with_door();
        assert_eq!(m.openings_of(&wall).collect::<Vec<_>>(), vec![&opening]);
        assert_eq!(m.host_of(&opening), Some(&wall));
        assert_eq!(m.fills_of(&opening).collect::<Vec<_>>(), vec![&door]);
        assert_eq!(m.contained_in(&_s).count(), 1);
        assert_eq!(m.by_class(&IfcClass::Door).count(), 1);
    }

    #[test]
    fn a_malformed_representation_is_refused_before_it_reaches_the_model() {
        // Two collinear points enclose nothing, and an IfcExtrudedAreaSolid built from them
        // produces a file other applications cannot open. Reject it at the command, not at
        // export time.
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        let id = e.global_id.clone();
        m.apply(create(e)).unwrap();
        let revision = m.revision();

        let result = m.apply(ModelCommand::SetRepresentation {
            global_id: id.clone(),
            representation: Some(Box::new(crate::Representation::extrusion(
                vec![[0.0, 0.0], [1.0, 0.0]],
                [0.0, 0.0, 1.0],
                3.0,
            ))),
        });

        assert!(matches!(
            result,
            Err(CommandError::InvalidRepresentation(_))
        ));
        assert_eq!(m.revision(), revision);
        assert!(m.get(&id).unwrap().representation.is_none());
    }

    #[test]
    fn setting_a_representation_invalidates_geometry_not_semantics() {
        let mut m = Model::new();
        let e = element(IfcClass::Wall);
        let id = e.global_id.clone();
        m.apply(create(e)).unwrap();

        let outcome = m
            .apply(ModelCommand::SetRepresentation {
                global_id: id.clone(),
                representation: Some(Box::new(crate::Representation::extrusion(
                    vec![[0.0, 0.0], [4.0, 0.0], [4.0, 0.2], [0.0, 0.2]],
                    [0.0, 0.0, 1.0],
                    3.0,
                ))),
            })
            .unwrap();

        assert_eq!(outcome.geometry_invalidated, vec![id.clone()]);
        assert_eq!(m.get(&id).unwrap().semantic_revision, 0);
        assert!(m
            .get(&id)
            .unwrap()
            .representation
            .as_ref()
            .unwrap()
            .is_native_parametric());
    }

    #[test]
    fn history_records_every_mutation() {
        let (m, ..) = wall_with_door();
        assert_eq!(m.history().len(), 6);
        assert_eq!(m.revision(), 6);
        assert_eq!(m.history()[0].number, 1);
    }
}
