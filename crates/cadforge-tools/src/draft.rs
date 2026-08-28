//! Drawing tools: picked points in, commands out.

use cadforge_core::{ElementRecord, GlobalId, IfcClass, ModelCommand, Placement, Representation};
use cadforge_geom::{snap, GeometryError, Profile, Snap, SnapSettings};
use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Below this, two picks are the same pick. One millimetre — small enough never to reject a
/// deliberate short wall, large enough that a double-click cannot author a degenerate solid.
const MIN_LENGTH: f64 = 0.001;

/// Which tool is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Tool {
    /// Clicking selects rather than draws.
    #[default]
    Select,
    /// Two points: a centreline. Thickness and height come from the settings.
    Wall,
    /// A closed polygon, finished by clicking the first point again or by [`Draft::finish`].
    Slab,
    /// One point.
    Column,
}

impl Tool {
    /// The word that appears in an element's name and in the status line.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Wall => "Wall",
            Tool::Slab => "Slab",
            Tool::Column => "Column",
        }
    }

    /// How many points this tool needs before it can produce anything.
    pub fn minimum_points(self) -> usize {
        match self {
            Tool::Select => 0,
            Tool::Column => 1,
            Tool::Wall => 2,
            Tool::Slab => 3,
        }
    }

    /// Whether reaching [`Tool::minimum_points`] finishes the element.
    ///
    /// A slab does not: three points is the minimum, not the intent, so it waits to be closed
    /// explicitly. A wall does, because a second click means one wall and nothing else.
    pub fn commits_at_minimum(self) -> bool {
        !matches!(self, Tool::Slab | Tool::Select)
    }
}

/// What the container of new elements is, and where it sits.
///
/// The origin is not decoration. Placements are stored relative to the container, so a storey
/// at +3 m holding a wall whose placement was recorded in world coordinates puts that wall at
/// +6 m. Carrying the origin here makes the subtraction impossible to forget.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerRef {
    pub id: GlobalId,
    pub origin: DVec3,
}

impl ContainerRef {
    pub fn new(id: GlobalId, origin: DVec3) -> Self {
        Self { id, origin }
    }
}

/// The dimensions a tool draws with, and how hard it snaps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftSettings {
    pub wall_thickness: f64,
    pub wall_height: f64,
    pub slab_thickness: f64,
    pub column_width: f64,
    pub column_depth: f64,
    pub column_height: f64,
    /// The level being drawn on, in world Z. Picked points are forced onto it, because a
    /// plan-view click that lands a few millimetres off the storey is never what was meant.
    pub elevation: f64,
    pub snap: SnapSettings,
}

impl Default for DraftSettings {
    fn default() -> Self {
        Self {
            wall_thickness: 0.2,
            wall_height: 3.0,
            slab_thickness: 0.2,
            column_width: 0.4,
            column_depth: 0.4,
            column_height: 3.0,
            elevation: 0.0,
            snap: SnapSettings::default(),
        }
    }
}

/// What a click did.
#[derive(Debug, Clone, PartialEq)]
pub enum DraftOutcome {
    /// Not a drawing click — the select tool is active.
    Ignored,
    /// The point was taken and the tool wants more.
    Pending(Snap),
    /// An element is ready. The tool has reset itself.
    Commit {
        snap: Snap,
        global_id: GlobalId,
        commands: Vec<ModelCommand>,
    },
}

/// Why a click could not produce an element.
///
/// Refusing is deliberate: a zero-length wall is valid as far as the command model is
/// concerned and worthless as far as a building is concerned, and it is much cheaper to
/// reject here than to find it in an export three weeks later.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {
    #[error("those two points are {0:.4} m apart — too close to draw between")]
    TooShort(f64),
    #[error("that outline encloses no area")]
    Degenerate,
    #[error("geometry: {0}")]
    Geometry(#[from] GeometryError),
}

/// A drawing session: the active tool and the points picked so far.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    tool: Tool,
    points: Vec<DVec3>,
    pub settings: DraftSettings,
    /// Where new elements are filed. `None` leaves them uncontained, which the exporter
    /// handles but a real project should not rely on.
    pub container: Option<ContainerRef>,
    /// Names elements "Wall 1", "Wall 2" and so on, counted per tool so drawing a slab does
    /// not push the next wall's number along. Session-local and monotonic: after an undo the
    /// numbering skips rather than repeating, which is the honest behaviour — the identity
    /// that matters is the `GlobalId`, and names are for humans reading a list.
    counters: BTreeMap<&'static str, u32>,
}

impl Draft {
    pub fn new(settings: DraftSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// Switch tools, abandoning anything half-drawn.
    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.points.clear();
    }

    /// The points picked so far, for drawing the rubber band.
    pub fn points(&self) -> &[DVec3] {
        &self.points
    }

    pub fn is_drawing(&self) -> bool {
        !self.points.is_empty()
    }

    /// Abandon the current element. The tool stays selected.
    pub fn cancel(&mut self) {
        self.points.clear();
    }

    /// Take back the last point, so a misplaced click costs one keystroke rather than the
    /// whole outline.
    pub fn undo_point(&mut self) -> Option<DVec3> {
        self.points.pop()
    }

    /// Where a raw point would land, without taking it.
    ///
    /// The interface calls this every time the cursor moves so the crosshair sits exactly
    /// where a click would put it. If the preview and the click disagreed, snapping would
    /// feel like the tool arguing with you.
    pub fn resolve(&self, target: DVec3, candidates: &[DVec3]) -> Snap {
        let flat = DVec3::new(target.x, target.y, self.settings.elevation);
        snap(flat, candidates, &self.settings.snap)
    }

    /// What clicking here would produce, without producing it.
    ///
    /// The rubber band. Built by running the real construction against a hypothetical point,
    /// rather than by a second implementation of the same geometry — a preview that disagreed
    /// with the result would be worse than no preview.
    pub fn preview(&self, target: DVec3, candidates: &[DVec3]) -> Option<ElementRecord> {
        if self.tool == Tool::Select {
            return None;
        }
        let mut trial = self.clone();
        trial.points.push(trial.resolve(target, candidates).point);
        if trial.points.len() < trial.tool.minimum_points() {
            return None;
        }
        trial.build().ok()
    }

    /// Take a point.
    ///
    /// On error the point is discarded and everything already picked is kept, so a bad second
    /// click on a wall costs that click and not the start point.
    pub fn click(
        &mut self,
        target: DVec3,
        candidates: &[DVec3],
    ) -> Result<DraftOutcome, ToolError> {
        if self.tool == Tool::Select {
            return Ok(DraftOutcome::Ignored);
        }
        let snapped = self.resolve(target, candidates);

        // Clicking the first point again closes a polygon. Checked before the point is
        // recorded, or the outline gains a duplicate vertex on the way out.
        if self.tool == Tool::Slab
            && self.points.len() >= self.tool.minimum_points()
            && self.points[0].distance(snapped.point) <= self.settings.snap.tolerance
        {
            return self.commit(snapped);
        }

        self.points.push(snapped.point);
        if self.points.len() >= self.tool.minimum_points() && self.tool.commits_at_minimum() {
            // Put the rejected point back off the stack. Keeping it would leave a wall with
            // three points, and the next click would then draw from the bad one.
            return self.commit(snapped).inspect_err(|_| {
                self.points.pop();
            });
        }
        Ok(DraftOutcome::Pending(snapped))
    }

    /// Finish an open-ended tool — a slab outline, closed from the keyboard.
    pub fn finish(&mut self) -> Result<DraftOutcome, ToolError> {
        if self.points.len() < self.tool.minimum_points() {
            return Ok(DraftOutcome::Ignored);
        }
        let last = Snap::free(*self.points.last().expect("checked non-empty above"));
        self.commit(last)
    }

    fn commit(&mut self, snapped: Snap) -> Result<DraftOutcome, ToolError> {
        let element = self.build()?;
        let global_id = element.global_id.clone();
        // Only reached once the geometry is known good, so a rejected click cannot consume a
        // number or clear the points behind the user's back.
        self.points.clear();
        *self.counters.entry(self.tool.label()).or_default() += 1;
        Ok(DraftOutcome::Commit {
            snap: snapped,
            global_id,
            commands: vec![ModelCommand::CreateElement {
                element: Box::new(element),
            }],
        })
    }

    /// Turn the picked points into an element.
    fn build(&self) -> Result<ElementRecord, ToolError> {
        let origin = self
            .container
            .as_ref()
            .map(|c| c.origin)
            .unwrap_or(DVec3::ZERO);

        let (class, placement, representation) = match self.tool {
            Tool::Wall => {
                let (a, b) = (self.points[0], self.points[1]);
                // Walls are vertical, so the run is a plan distance. Using the 3D length
                // would make a wall between two storeys lean.
                let run = DVec3::new(b.x - a.x, b.y - a.y, 0.0);
                let length = run.length();
                if length < MIN_LENGTH {
                    return Err(ToolError::TooShort(length));
                }
                let half = self.settings.wall_thickness * 0.5;
                // Local X runs along the wall, so the profile is the plan footprint: the
                // centreline from 0 to length, spread half a thickness either side.
                let profile = Profile::new([
                    DVec2::new(0.0, -half),
                    DVec2::new(length, -half),
                    DVec2::new(length, half),
                    DVec2::new(0.0, half),
                ])?;
                (
                    IfcClass::Wall,
                    Placement::new(a - origin, DVec3::Z, run),
                    Representation::extrusion(
                        outer_of(&profile),
                        [0.0, 0.0, 1.0],
                        self.settings.wall_height,
                    ),
                )
            }

            Tool::Slab => {
                let base = self.points[0];
                let profile = Profile::new(
                    self.points
                        .iter()
                        .map(|p| DVec2::new(p.x - base.x, p.y - base.y)),
                )
                .map_err(|e| match e {
                    // "Encloses no area" is the one a person can act on: the outline doubled
                    // back on itself. The rest are genuinely geometric.
                    GeometryError::ZeroArea => ToolError::Degenerate,
                    other => ToolError::Geometry(other),
                })?;
                (
                    IfcClass::Slab,
                    Placement::new(base - origin, DVec3::Z, DVec3::X),
                    // Downwards: the picked outline is the finished floor level, which is the
                    // level people set out from and the one a storey elevation refers to.
                    Representation::extrusion(
                        outer_of(&profile),
                        [0.0, 0.0, -1.0],
                        self.settings.slab_thickness,
                    ),
                )
            }

            Tool::Column => {
                let profile =
                    Profile::rectangle(self.settings.column_width, self.settings.column_depth)?;
                (
                    IfcClass::Column,
                    Placement::new(self.points[0] - origin, DVec3::Z, DVec3::X),
                    Representation::extrusion(
                        outer_of(&profile),
                        [0.0, 0.0, 1.0],
                        self.settings.column_height,
                    ),
                )
            }

            Tool::Select => unreachable!("the select tool never reaches commit"),
        };

        debug_assert!(
            representation.is_valid(),
            "a tool must never author a representation the exporter would reject"
        );

        let mut element = ElementRecord::new(GlobalId::new(), class);
        let label = self.tool.label();
        element.name = Some(format!(
            "{label} {}",
            self.counters.get(label).copied().unwrap_or(0) + 1
        ));
        element.placement = placement;
        element.container = self.container.as_ref().map(|c| c.id.clone());
        element.representation = Some(representation);
        Ok(element)
    }
}

/// Read the boundary back out of a [`Profile`] rather than reusing the input.
///
/// `Profile::new` normalises the winding, and IFC wants what it produced, not what we handed
/// it. Building the profile and then ignoring its output would put a clockwise loop in the
/// file about half the time.
fn outer_of(profile: &Profile) -> Vec<[f64; 2]> {
    profile.outer().iter().map(|p| [p.x, p.y]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadforge_core::Model;

    fn wall_draft() -> Draft {
        let mut draft = Draft::default();
        draft.set_tool(Tool::Wall);
        draft
    }

    /// The representation a commit produced, for tests that care about geometry.
    fn representation(outcome: &DraftOutcome) -> &Representation {
        let DraftOutcome::Commit { commands, .. } = outcome else {
            panic!("expected a commit, got {outcome:?}");
        };
        let [ModelCommand::CreateElement { element }] = &commands[..] else {
            panic!("a tool should author exactly one element");
        };
        element.representation.as_ref().expect("geometry")
    }

    fn name(outcome: &DraftOutcome) -> Option<String> {
        let DraftOutcome::Commit { commands, .. } = outcome else {
            panic!("expected a commit");
        };
        let [ModelCommand::CreateElement { element }] = &commands[..] else {
            panic!("one element");
        };
        element.name.clone()
    }

    fn placement(outcome: &DraftOutcome) -> Placement {
        let DraftOutcome::Commit { commands, .. } = outcome else {
            panic!("expected a commit");
        };
        let [ModelCommand::CreateElement { element }] = &commands[..] else {
            panic!("one element");
        };
        element.placement
    }

    #[test]
    fn two_clicks_make_a_wall() {
        let mut draft = wall_draft();
        assert!(matches!(
            draft.click(DVec3::ZERO, &[]).unwrap(),
            DraftOutcome::Pending(_)
        ));
        assert!(draft.is_drawing());

        let outcome = draft.click(DVec3::new(5.0, 0.0, 0.0), &[]).unwrap();
        let Representation::ExtrudedAreaSolid { profile, depth, .. } = representation(&outcome)
        else {
            panic!(
                "a wall must be a swept solid, not a mesh — that is the whole point of ADR-0004"
            );
        };
        // 5 m long, 0.2 m thick: the profile is the plan footprint.
        let xs: Vec<f64> = profile.iter().map(|p| p[0]).collect();
        let ys: Vec<f64> = profile.iter().map(|p| p[1]).collect();
        assert!((xs.iter().cloned().fold(f64::MIN, f64::max) - 5.0).abs() < 1e-12);
        assert!((ys.iter().cloned().fold(f64::MIN, f64::max) - 0.1).abs() < 1e-12);
        assert!((depth - 3.0).abs() < 1e-12);

        // And it reset, ready for the next wall.
        assert!(!draft.is_drawing());
    }

    #[test]
    fn a_wall_runs_along_its_own_local_x() {
        // The profile is measured from the origin along local X, so the placement has to
        // rotate with the wall. Get this wrong and every wall draws due east.
        let mut draft = wall_draft();
        draft.click(DVec3::new(1.0, 1.0, 0.0), &[]).unwrap();
        let outcome = draft.click(DVec3::new(1.0, 4.0, 0.0), &[]).unwrap();

        let placement = placement(&outcome);
        assert!((placement.location - DVec3::new(1.0, 1.0, 0.0)).length() < 1e-12);
        assert!(
            (placement.ref_direction - DVec3::Y).length() < 1e-12,
            "a wall drawn north should point north, got {:?}",
            placement.ref_direction
        );

        // The far end of the profile must land back on the second click.
        let far = placement
            .to_matrix()
            .transform_point3(DVec3::new(3.0, 0.0, 0.0));
        assert!((far - DVec3::new(1.0, 4.0, 0.0)).length() < 1e-12);
    }

    #[test]
    fn a_zero_length_wall_is_refused_without_losing_the_start() {
        let mut draft = wall_draft();
        draft.click(DVec3::new(2.0, 2.0, 0.0), &[]).unwrap();

        let error = draft.click(DVec3::new(2.0, 2.0, 0.0), &[]).unwrap_err();
        assert!(matches!(error, ToolError::TooShort(_)));
        // A double-click must not throw away the point that was placed correctly.
        assert_eq!(draft.points(), &[DVec3::new(2.0, 2.0, 0.0)]);

        // Clicking somewhere real still finishes the wall.
        let outcome = draft.click(DVec3::new(6.0, 2.0, 0.0), &[]).unwrap();
        assert!(matches!(outcome, DraftOutcome::Commit { .. }));
    }

    #[test]
    fn a_container_offset_is_subtracted_from_the_placement() {
        // The bug this exists to prevent: a storey at +3 m holding a wall placed at world
        // coordinates draws the wall at +6 m, because the viewer composes the chain.
        let mut draft = wall_draft();
        draft.settings.elevation = 3.0;
        draft.container = Some(ContainerRef::new(
            GlobalId::new(),
            DVec3::new(0.0, 0.0, 3.0),
        ));

        draft.click(DVec3::new(0.0, 0.0, 3.0), &[]).unwrap();
        let outcome = draft.click(DVec3::new(4.0, 0.0, 3.0), &[]).unwrap();
        assert!(
            placement(&outcome).location.z.abs() < 1e-12,
            "storey-relative placement should sit at the storey's own zero"
        );
    }

    #[test]
    fn clicks_snap_to_candidates_and_to_the_level() {
        let mut draft = wall_draft();
        draft.settings.elevation = 3.0;
        let corner = DVec3::new(4.02, 0.0, 3.0);

        // Aimed near an existing corner, and at the wrong elevation: both are corrected.
        draft.click(DVec3::ZERO, &[]).unwrap();
        let outcome = draft.click(DVec3::new(4.05, 0.01, 0.0), &[corner]).unwrap();
        let DraftOutcome::Commit { snap, .. } = &outcome else {
            panic!("expected a commit");
        };
        assert_eq!(snap.point, corner);
    }

    #[test]
    fn a_slab_waits_to_be_closed() {
        let mut draft = Draft::default();
        draft.set_tool(Tool::Slab);
        for point in [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(4.0, 0.0, 0.0),
            DVec3::new(4.0, 3.0, 0.0),
            DVec3::new(0.0, 3.0, 0.0),
        ] {
            assert!(matches!(
                draft.click(point, &[]).unwrap(),
                DraftOutcome::Pending(_)
            ));
        }

        // Clicking the start again closes it, without leaving a duplicate vertex behind.
        let outcome = draft.click(DVec3::new(0.0, 0.02, 0.0), &[]).unwrap();
        let Representation::ExtrudedAreaSolid {
            profile, direction, ..
        } = representation(&outcome)
        else {
            panic!("a slab must be a swept solid");
        };
        assert_eq!(profile.len(), 4, "closing must not repeat the first point");
        assert_eq!(
            *direction,
            [0.0, 0.0, -1.0],
            "the picked outline is the top of the slab"
        );
    }

    #[test]
    fn a_slab_outline_that_encloses_nothing_is_refused() {
        let mut draft = Draft::default();
        draft.set_tool(Tool::Slab);
        for point in [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(4.0, 0.0, 0.0),
            DVec3::new(8.0, 0.0, 0.0),
        ] {
            draft.click(point, &[]).unwrap();
        }
        assert_eq!(draft.finish().unwrap_err(), ToolError::Degenerate);
        // Still drawable: the points survive so another click can rescue the outline.
        assert_eq!(draft.points().len(), 3);
        draft.click(DVec3::new(4.0, 3.0, 0.0), &[]).unwrap();
        assert!(matches!(
            draft.finish().unwrap(),
            DraftOutcome::Commit { .. }
        ));
    }

    #[test]
    fn one_click_makes_a_column() {
        let mut draft = Draft::default();
        draft.set_tool(Tool::Column);
        let outcome = draft.click(DVec3::new(3.0, 3.0, 0.0), &[]).unwrap();
        let Representation::ExtrudedAreaSolid { profile, .. } = representation(&outcome) else {
            panic!("a column must be a swept solid");
        };
        // Centred on the pick: a column is set out from its own centreline, not a corner.
        let cx: f64 = profile.iter().map(|p| p[0]).sum::<f64>() / profile.len() as f64;
        let cy: f64 = profile.iter().map(|p| p[1]).sum::<f64>() / profile.len() as f64;
        assert!(cx.abs() < 1e-12 && cy.abs() < 1e-12);
        assert!((placement(&outcome).location - DVec3::new(3.0, 3.0, 0.0)).length() < 1e-12);
    }

    #[test]
    fn the_preview_is_what_the_click_would_build() {
        // If these could drift apart, the ghost would sit somewhere the wall does not.
        let mut draft = wall_draft();
        draft.click(DVec3::ZERO, &[]).unwrap();

        let cursor = DVec3::new(4.03, 0.0, 0.0);
        let candidate = DVec3::new(4.0, 0.0, 0.0);
        let ghost = draft.preview(cursor, &[candidate]).expect("a pending wall");
        let outcome = draft.click(cursor, &[candidate]).unwrap();

        assert_eq!(
            ghost.representation.as_ref(),
            Some(representation(&outcome))
        );
        assert_eq!(ghost.placement, placement(&outcome));
        assert_eq!(ghost.class, IfcClass::Wall);
    }

    #[test]
    fn a_column_previews_before_its_only_click() {
        // One-click tools have nothing picked yet, so the preview has to come from the cursor
        // alone or a column would never show one.
        let mut draft = Draft::default();
        draft.set_tool(Tool::Column);
        let ghost = draft.preview(DVec3::new(2.0, 5.0, 0.0), &[]).unwrap();
        assert!((ghost.placement.location - DVec3::new(2.0, 5.0, 0.0)).length() < 1e-12);
        assert!(!draft.is_drawing(), "previewing must not pick anything");
    }

    #[test]
    fn there_is_nothing_to_preview_before_a_wall_starts() {
        let draft = wall_draft();
        assert!(draft.preview(DVec3::ZERO, &[]).is_none());
        assert!(Draft::default().preview(DVec3::ZERO, &[]).is_none());
    }

    #[test]
    fn the_select_tool_draws_nothing() {
        let mut draft = Draft::default();
        assert_eq!(
            draft.click(DVec3::ZERO, &[]).unwrap(),
            DraftOutcome::Ignored
        );
        assert!(!draft.is_drawing());
    }

    #[test]
    fn each_tool_numbers_its_own_elements() {
        // A shared counter numbers the first wall after a slab "Wall 2", which reads like a
        // wall went missing.
        let mut draft = Draft::default();
        draft.set_tool(Tool::Column);
        let first = draft.click(DVec3::ZERO, &[]).unwrap();
        draft.set_tool(Tool::Wall);
        draft.click(DVec3::ZERO, &[]).unwrap();
        let wall = draft.click(DVec3::new(4.0, 0.0, 0.0), &[]).unwrap();

        assert_eq!(name(&first).as_deref(), Some("Column 1"));
        assert_eq!(name(&wall).as_deref(), Some("Wall 1"));
    }

    #[test]
    fn switching_tools_abandons_a_half_drawn_element() {
        let mut draft = wall_draft();
        draft.click(DVec3::ZERO, &[]).unwrap();
        draft.set_tool(Tool::Column);
        assert!(!draft.is_drawing());

        // If the abandoned point survived, this single click would draw a column at the
        // origin instead of where it was aimed.
        let outcome = draft.click(DVec3::new(9.0, 9.0, 0.0), &[]).unwrap();
        assert!((placement(&outcome).location - DVec3::new(9.0, 9.0, 0.0)).length() < 1e-12);
    }

    #[test]
    fn taking_a_point_back_leaves_the_rest() {
        let mut draft = Draft::default();
        draft.set_tool(Tool::Slab);
        draft.click(DVec3::ZERO, &[]).unwrap();
        draft.click(DVec3::new(4.0, 0.0, 0.0), &[]).unwrap();
        assert_eq!(draft.undo_point(), Some(DVec3::new(4.0, 0.0, 0.0)));
        assert_eq!(draft.points(), &[DVec3::ZERO]);
    }

    #[test]
    fn drawn_elements_are_ordinary_commands() {
        // The claim the whole crate rests on: a hand-drawn wall is indistinguishable from an
        // authored one, so undo, redo, and export need to know nothing about tools.
        let mut model = Model::new();
        let mut draft = wall_draft();
        draft.click(DVec3::ZERO, &[]).unwrap();
        let DraftOutcome::Commit {
            commands,
            global_id,
            ..
        } = draft.click(DVec3::new(4.0, 0.0, 0.0), &[]).unwrap()
        else {
            panic!("expected a commit");
        };

        model.apply_all(commands).unwrap();
        assert!(model.contains(&global_id));

        model.undo().unwrap();
        assert!(model.is_empty());
        model.redo().unwrap();
        assert!(model.get(&global_id).unwrap().representation.is_some());
    }

    #[test]
    fn every_tool_authors_native_parametric_geometry() {
        // Phase 5's actual requirement. A tool that emitted triangles would look identical on
        // screen and be dead on arrival in Revit or Archicad.
        let mut draft = Draft::default();

        draft.set_tool(Tool::Wall);
        draft.click(DVec3::ZERO, &[]).unwrap();
        let wall = draft.click(DVec3::new(3.0, 0.0, 0.0), &[]).unwrap();

        draft.set_tool(Tool::Column);
        let column = draft.click(DVec3::new(1.0, 1.0, 0.0), &[]).unwrap();

        draft.set_tool(Tool::Slab);
        for point in [
            DVec3::ZERO,
            DVec3::new(3.0, 0.0, 0.0),
            DVec3::new(3.0, 3.0, 0.0),
        ] {
            draft.click(point, &[]).unwrap();
        }
        let slab = draft.finish().unwrap();

        for outcome in [&wall, &column, &slab] {
            let representation = representation(outcome);
            assert!(representation.is_native_parametric());
            assert!(representation.is_valid());
        }
    }
}
