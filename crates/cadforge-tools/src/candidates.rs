//! Where the points worth snapping to come from.
//!
//! [`crate::draft`] takes candidates as a plain slice because geometry should not have to know
//! what a model is. This module is the other half: it reads a model and produces that slice.

use cadforge_core::{GlobalId, Model, Representation};
use glam::{DMat4, DVec3};

/// How many candidates to hand back. Beyond this the nearest ones are all that can matter —
/// the tolerance is a few pixels wide.
const MAX_CANDIDATES: usize = 64;

/// Two candidates closer than this are the same corner arriving twice, which a mesh does
/// once per incident triangle.
const MERGE: f64 = 1e-6;

/// Compose an element's placement with its containers, up to the spatial root.
///
/// Placements are stored relative to whatever contains them, so anything working in world
/// space needs the chain. Bounded, so a file with a containment cycle cannot hang the caller —
/// imported files are not required to be sane.
pub fn world_transform(model: &Model, mut id: GlobalId) -> DMat4 {
    let mut transform = DMat4::IDENTITY;
    for _ in 0..64 {
        let Some(element) = model.get(&id) else { break };
        transform = element.placement.to_matrix() * transform;
        match &element.container {
            Some(parent) if parent != &id => id = parent.clone(),
            _ => break,
        }
    }
    transform
}

/// Points near `near` that a click should latch onto: element corners, in world space.
///
/// A linear scan over every vertex in the model. That is honest at the scale this is drawn
/// at — a few hundred thousand distance checks costs about a millisecond — and if a project
/// ever makes it hurt, the fix is an R-tree of candidate points rather than a cleverer scan.
///
/// Corners and edge midpoints. Wall-axis intersections — the crossing of two centrelines that
/// belongs to neither wall — are the obvious next candidate and are named here so their absence
/// is a decision rather than an oversight.
pub fn snap_candidates(model: &Model, near: DVec3, radius: f64) -> Vec<DVec3> {
    let mut found: Vec<(f64, DVec3)> = Vec::new();
    let radius_squared = radius * radius;

    for element in model.iter() {
        let Some(representation) = &element.representation else {
            continue;
        };
        let transform = world_transform(model, element.global_id.clone());
        snap_points(representation, &transform, |point| {
            let distance = point.distance_squared(near);
            if distance <= radius_squared {
                found.push((distance, point));
            }
        });
    }

    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut points: Vec<DVec3> = Vec::with_capacity(found.len().min(MAX_CANDIDATES));
    for (_, point) in found {
        // Sorted by distance, so duplicates of the same corner are adjacent.
        if points
            .last()
            .is_some_and(|last: &DVec3| last.distance_squared(point) < MERGE * MERGE)
        {
            continue;
        }
        points.push(point);
        if points.len() == MAX_CANDIDATES {
            break;
        }
    }
    points
}

/// Feed every point of one element worth latching onto to `emit`, in world space.
fn snap_points(representation: &Representation, transform: &DMat4, mut emit: impl FnMut(DVec3)) {
    match representation {
        Representation::ExtrudedAreaSolid {
            profile,
            direction,
            depth,
        } => {
            let sweep = DVec3::from_array(*direction) * *depth;
            let at = |i: usize| DVec3::new(profile[i][0], profile[i][1], 0.0);
            for i in 0..profile.len() {
                let corner = at(i);
                // The midpoint of a profile edge is not a decoration: for a wall, whose
                // profile is the centreline spread half a thickness either side, the midpoint
                // of an end edge *is* the centreline endpoint. That is the point walls are set
                // out from, and without it a wall drawn onto another one lands on a face
                // corner half a thickness off — which looks joined and measures wrong.
                let midpoint = (corner + at((i + 1) % profile.len())) * 0.5;
                for local in [corner, midpoint] {
                    // Both caps. The top of a wall is a real thing to snap to — it is where
                    // the next storey's slab meets it.
                    emit(transform.transform_point3(local));
                    emit(transform.transform_point3(local + sweep));
                }
            }
        }
        Representation::TriangulatedFaceSet { vertices, .. } => {
            for vertex in vertices {
                emit(transform.transform_point3(DVec3::from_array(*vertex)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadforge_core::{ElementRecord, GlobalId, IfcClass, ModelCommand, Placement};

    fn wall_at(model: &mut Model, location: DVec3, length: f64) -> GlobalId {
        let id = GlobalId::new();
        let mut element = ElementRecord::new(id.clone(), IfcClass::Wall);
        element.placement = Placement::at(location);
        element.representation = Some(Representation::extrusion(
            vec![[0.0, 0.0], [length, 0.0], [length, 0.2], [0.0, 0.2]],
            [0.0, 0.0, 1.0],
            3.0,
        ));
        model
            .apply(ModelCommand::CreateElement {
                element: Box::new(element),
            })
            .unwrap();
        id
    }

    #[test]
    fn a_wall_offers_its_centreline_end_not_just_its_face_corners() {
        // The failure this exists to catch: snapping to the nearest *corner* of a 200 mm wall
        // puts the next wall 100 mm off the one it was aimed at. Every wall in the room is
        // then almost-joined, which is the exact error snapping is for.
        let mut model = Model::new();
        wall_at(&mut model, DVec3::ZERO, 4.0);

        let end = DVec3::new(4.0, 0.1, 0.0); // the profile is 0..0.2 in Y, so the axis is 0.1
        let candidates = snap_candidates(&model, end + DVec3::new(0.02, -0.015, 0.0), 0.3);
        assert_eq!(
            candidates.first(),
            Some(&end),
            "the centreline end should win, got {candidates:?}"
        );
    }

    #[test]
    fn a_walls_corners_are_candidates_in_world_space() {
        let mut model = Model::new();
        wall_at(&mut model, DVec3::new(10.0, 0.0, 0.0), 4.0);

        let candidates = snap_candidates(&model, DVec3::new(14.0, 0.0, 0.0), 0.5);
        assert!(
            candidates.contains(&DVec3::new(14.0, 0.0, 0.0)),
            "the far end of the wall should be reachable, got {candidates:?}"
        );
        // Its own local origin is 10 m away and must not appear.
        assert!(!candidates
            .iter()
            .any(|p| p.distance(DVec3::new(10.0, 0.0, 0.0)) < 1e-9));
    }

    #[test]
    fn the_top_of_a_wall_is_a_candidate_too() {
        let mut model = Model::new();
        wall_at(&mut model, DVec3::ZERO, 4.0);

        let candidates = snap_candidates(&model, DVec3::new(0.0, 0.0, 3.0), 0.2);
        assert!(candidates.contains(&DVec3::new(0.0, 0.0, 3.0)));
    }

    #[test]
    fn a_container_elevation_moves_its_contents() {
        // The composition test: a wall in a storey at +3 m has corners at +3 m, not at zero.
        let mut model = Model::new();
        let storey = GlobalId::new();
        let mut level = ElementRecord::new(storey.clone(), IfcClass::BuildingStorey);
        level.placement = Placement::at(DVec3::new(0.0, 0.0, 3.0));
        model
            .apply(ModelCommand::CreateElement {
                element: Box::new(level),
            })
            .unwrap();

        let wall = wall_at(&mut model, DVec3::ZERO, 4.0);
        model
            .apply(ModelCommand::AssignContainer {
                global_id: wall,
                container: Some(storey),
            })
            .unwrap();

        let candidates = snap_candidates(&model, DVec3::new(4.0, 0.0, 3.0), 0.1);
        assert!(candidates.contains(&DVec3::new(4.0, 0.0, 3.0)));
    }

    #[test]
    fn candidates_are_sorted_nearest_first_and_deduplicated() {
        let mut model = Model::new();
        wall_at(&mut model, DVec3::ZERO, 4.0);
        wall_at(&mut model, DVec3::new(4.0, 0.0, 0.0), 4.0);

        // Two walls meeting at x=4 contribute the same corner twice.
        let near = DVec3::new(4.0, 0.0, 0.0);
        let candidates = snap_candidates(&model, near, 1.0);
        assert_eq!(candidates.first(), Some(&near));
        assert_eq!(
            candidates
                .iter()
                .filter(|p| p.distance(near) < 1e-9)
                .count(),
            1,
            "a shared corner must appear once, or ties become arbitrary"
        );

        let distances: Vec<f64> = candidates.iter().map(|p| p.distance(near)).collect();
        assert!(distances.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn nothing_within_reach_yields_nothing() {
        let mut model = Model::new();
        wall_at(&mut model, DVec3::ZERO, 4.0);
        assert!(snap_candidates(&model, DVec3::new(100.0, 100.0, 0.0), 0.5).is_empty());
    }

    #[test]
    fn a_containment_cycle_does_not_hang() {
        // Imported files are not required to be sane, and a viewer that freezes on a bad file
        // is worse than one that draws it slightly wrong.
        //
        // It takes two storeys to build this: the command model already refuses a
        // non-spatial container and a self-reference, so the only cycle it permits is one
        // between spatial elements.
        let mut model = Model::new();
        let mut storeys = Vec::new();
        for elevation in [0.0, 3.0] {
            let id = GlobalId::new();
            let mut level = ElementRecord::new(id.clone(), IfcClass::BuildingStorey);
            level.placement = Placement::at(DVec3::new(0.0, 0.0, elevation));
            model
                .apply(ModelCommand::CreateElement {
                    element: Box::new(level),
                })
                .unwrap();
            storeys.push(id);
        }
        for (child, parent) in [(0, 1), (1, 0)] {
            model
                .apply(ModelCommand::AssignContainer {
                    global_id: storeys[child].clone(),
                    container: Some(storeys[parent].clone()),
                })
                .unwrap();
        }

        let wall = wall_at(&mut model, DVec3::ZERO, 4.0);
        model
            .apply(ModelCommand::AssignContainer {
                global_id: wall,
                container: Some(storeys[0].clone()),
            })
            .unwrap();

        // Terminating at all is the assertion. The transform it lands on is meaningless
        // because the file is meaningless.
        let _ = snap_candidates(&model, DVec3::ZERO, 1.0);
    }
}
