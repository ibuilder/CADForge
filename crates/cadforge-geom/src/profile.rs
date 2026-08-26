//! Closed 2D profiles — the input to every sweep.
//!
//! Maps onto `IfcArbitraryClosedProfileDef`, which is what makes an extrusion exportable as
//! native parametric IFC rather than as tessellated mush.

use crate::tess::TessellationSettings;
use crate::GeometryError;
use glam::DVec2;
use serde::{Deserialize, Serialize};

/// A closed profile in its own XY plane.
///
/// Invariants, established at construction and relied on by everything downstream: at least
/// three points, all finite, no repeated consecutive point, non-zero area, and the outer loop
/// wound counter-clockwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    outer: Vec<DVec2>,
    holes: Vec<Vec<DVec2>>,
}

impl Profile {
    /// Build from an outer boundary, validating and normalising the winding.
    ///
    /// The closing point may be repeated or omitted; either way it is stored once.
    pub fn new(outer: impl IntoIterator<Item = DVec2>) -> Result<Self, GeometryError> {
        let mut points: Vec<DVec2> = outer.into_iter().collect();

        if points.iter().any(|p| !p.is_finite()) {
            return Err(GeometryError::NonFinite);
        }

        // Drop an explicitly repeated closing point, then any consecutive duplicates.
        if points.len() >= 2 && points[0].abs_diff_eq(points[points.len() - 1], f64::EPSILON) {
            points.pop();
        }
        points.dedup_by(|a, b| a.abs_diff_eq(*b, 1e-12));
        if points.len() >= 2 && points[0].abs_diff_eq(points[points.len() - 1], 1e-12) {
            points.pop();
        }

        if points.len() < 3 {
            return Err(GeometryError::TooFewPoints(points.len()));
        }

        let area = signed_area(&points);
        if area.abs() < 1e-12 {
            return Err(GeometryError::ZeroArea);
        }
        if area < 0.0 {
            points.reverse();
        }

        Ok(Self {
            outer: points,
            holes: Vec::new(),
        })
    }

    /// Add an inner loop. Stored clockwise, opposite the outer loop, as IFC expects.
    ///
    /// Note that [`crate::sweep`] does not yet tessellate holes — see
    /// [`GeometryError::HolesNotSupported`].
    pub fn with_hole(
        mut self,
        hole: impl IntoIterator<Item = DVec2>,
    ) -> Result<Self, GeometryError> {
        let inner = Profile::new(hole)?;
        let mut points = inner.outer;
        points.reverse(); // outer is CCW by construction, so this makes the hole CW
        self.holes.push(points);
        Ok(self)
    }

    /// An axis-aligned rectangle centred on the origin. The wall, slab, and column workhorse.
    pub fn rectangle(width: f64, depth: f64) -> Result<Self, GeometryError> {
        let (hw, hd) = (width * 0.5, depth * 0.5);
        Self::new([
            DVec2::new(-hw, -hd),
            DVec2::new(hw, -hd),
            DVec2::new(hw, hd),
            DVec2::new(-hw, hd),
        ])
    }

    /// A circle approximated to the chord tolerance in `settings`.
    ///
    /// Segment count comes from the deflection tolerance rather than being hard-coded, so a
    /// 50 mm pipe and a 5 m silo both meet the same visual tolerance.
    pub fn circle(radius: f64, settings: &TessellationSettings) -> Result<Self, GeometryError> {
        if !radius.is_finite() {
            return Err(GeometryError::NonFinite);
        }
        let n = settings.circle_segments(radius);
        let points = (0..n).map(|i| {
            let t = std::f64::consts::TAU * (i as f64) / (n as f64);
            DVec2::new(radius * t.cos(), radius * t.sin())
        });
        Self::new(points)
    }

    /// The outer boundary, counter-clockwise, without a repeated closing point.
    pub fn outer(&self) -> &[DVec2] {
        &self.outer
    }

    pub fn holes(&self) -> &[Vec<DVec2>] {
        &self.holes
    }

    /// Enclosed area, holes subtracted. Always positive.
    pub fn area(&self) -> f64 {
        let outer = signed_area(&self.outer).abs();
        let holes: f64 = self.holes.iter().map(|h| signed_area(h).abs()).sum();
        (outer - holes).max(0.0)
    }

    /// Perimeter of the outer boundary.
    pub fn perimeter(&self) -> f64 {
        self.outer
            .iter()
            .zip(self.outer.iter().cycle().skip(1))
            .take(self.outer.len())
            .map(|(a, b)| a.distance(*b))
            .sum()
    }

    /// Triangulate the outer boundary by ear clipping.
    ///
    /// Returns indices into [`Profile::outer`]. O(n²), which is irrelevant at the point
    /// counts real building profiles have, and it needs no dependency.
    pub fn triangulate(&self) -> Result<Vec<[usize; 3]>, GeometryError> {
        if !self.holes.is_empty() {
            return Err(GeometryError::HolesNotSupported(self.holes.len()));
        }
        triangulate_simple(&self.outer)
    }
}

/// Positive for counter-clockwise, negative for clockwise. The shoelace formula.
pub fn signed_area(points: &[DVec2]) -> f64 {
    let n = points.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Ear clipping for a simple counter-clockwise polygon.
fn triangulate_simple(points: &[DVec2]) -> Result<Vec<[usize; 3]>, GeometryError> {
    let n = points.len();
    if n < 3 {
        return Err(GeometryError::TooFewPoints(n));
    }
    if n == 3 {
        return Ok(vec![[0, 1, 2]]);
    }

    let mut remaining: Vec<usize> = (0..n).collect();
    let mut triangles = Vec::with_capacity(n - 2);
    // Each successful clip removes one vertex; the guard bounds the pathological case where
    // no ear is found (self-intersecting input) instead of looping forever.
    let mut guard = 0;
    let max_iterations = n * n;

    while remaining.len() > 3 {
        guard += 1;
        if guard > max_iterations {
            return Err(GeometryError::TriangulationFailed);
        }

        let mut clipped = false;
        for i in 0..remaining.len() {
            let prev = remaining[(i + remaining.len() - 1) % remaining.len()];
            let curr = remaining[i];
            let next = remaining[(i + 1) % remaining.len()];

            if !is_ear(points, &remaining, prev, curr, next) {
                continue;
            }
            triangles.push([prev, curr, next]);
            remaining.remove(i);
            clipped = true;
            break;
        }

        if !clipped {
            return Err(GeometryError::TriangulationFailed);
        }
    }

    triangles.push([remaining[0], remaining[1], remaining[2]]);
    Ok(triangles)
}

fn is_ear(points: &[DVec2], remaining: &[usize], prev: usize, curr: usize, next: usize) -> bool {
    let (a, b, c) = (points[prev], points[curr], points[next]);

    // Reflex vertices cannot be ears (the polygon is counter-clockwise).
    if cross(b - a, c - b) <= 0.0 {
        return false;
    }
    // No other vertex may lie inside the candidate triangle.
    !remaining
        .iter()
        .filter(|&&i| i != prev && i != curr && i != next)
        .any(|&i| point_in_triangle(points[i], a, b, c))
}

fn cross(a: DVec2, b: DVec2) -> f64 {
    a.x * b.y - a.y * b.x
}

fn point_in_triangle(p: DVec2, a: DVec2, b: DVec2, c: DVec2) -> bool {
    // Inclusive of edges: a vertex lying exactly on an edge still blocks the ear, which
    // avoids emitting zero-area triangles.
    let d1 = cross(b - a, p - a);
    let d2 = cross(c - b, p - b);
    let d3 = cross(a - c, p - c);
    d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l_shape() -> Profile {
        // Concave, so it exercises ear clipping rather than a fan.
        Profile::new([
            DVec2::new(0.0, 0.0),
            DVec2::new(3.0, 0.0),
            DVec2::new(3.0, 1.0),
            DVec2::new(1.0, 1.0),
            DVec2::new(1.0, 3.0),
            DVec2::new(0.0, 3.0),
        ])
        .unwrap()
    }

    #[test]
    fn rectangle_has_the_expected_area_and_perimeter() {
        let p = Profile::rectangle(4.0, 2.0).unwrap();
        assert_eq!(p.outer().len(), 4);
        assert!((p.area() - 8.0).abs() < 1e-12);
        assert!((p.perimeter() - 12.0).abs() < 1e-12);
    }

    #[test]
    fn clockwise_input_is_normalised_to_counter_clockwise() {
        let cw = Profile::new([
            DVec2::new(0.0, 0.0),
            DVec2::new(0.0, 1.0),
            DVec2::new(1.0, 1.0),
            DVec2::new(1.0, 0.0),
        ])
        .unwrap();
        assert!(signed_area(cw.outer()) > 0.0, "outer loop must end up CCW");
    }

    #[test]
    fn a_repeated_closing_point_is_dropped() {
        let p = Profile::new([
            DVec2::new(0.0, 0.0),
            DVec2::new(1.0, 0.0),
            DVec2::new(1.0, 1.0),
            DVec2::new(0.0, 0.0),
        ])
        .unwrap();
        assert_eq!(p.outer().len(), 3);
    }

    #[test]
    fn degenerate_profiles_are_rejected() {
        assert_eq!(
            Profile::new([DVec2::ZERO, DVec2::X]),
            Err(GeometryError::TooFewPoints(2))
        );
        // Three collinear points enclose nothing.
        assert_eq!(
            Profile::new([DVec2::ZERO, DVec2::X, DVec2::new(2.0, 0.0)]),
            Err(GeometryError::ZeroArea)
        );
        assert_eq!(
            Profile::new([DVec2::ZERO, DVec2::X, DVec2::new(f64::NAN, 1.0)]),
            Err(GeometryError::NonFinite)
        );
        // Collapses to two distinct points once duplicates are removed.
        assert_eq!(
            Profile::new([DVec2::ZERO, DVec2::ZERO, DVec2::X, DVec2::X]),
            Err(GeometryError::TooFewPoints(2))
        );
    }

    #[test]
    fn circle_respects_the_chord_tolerance() {
        let fine = TessellationSettings::fine();
        let coarse = TessellationSettings::coarse();
        let a = Profile::circle(1.0, &fine).unwrap();
        let b = Profile::circle(1.0, &coarse).unwrap();

        assert!(
            a.outer().len() > b.outer().len(),
            "finer tolerance means more segments"
        );
        // An inscribed polygon under-estimates area, and never by much at these tolerances.
        let exact = std::f64::consts::PI;
        assert!(a.area() < exact);
        assert!((exact - a.area()) / exact < 0.01);
    }

    #[test]
    fn triangulating_a_convex_profile_gives_n_minus_2_triangles() {
        let p = Profile::rectangle(2.0, 2.0).unwrap();
        assert_eq!(p.triangulate().unwrap().len(), 2);
    }

    #[test]
    fn triangulating_a_concave_profile_conserves_area() {
        let p = l_shape();
        let triangles = p.triangulate().unwrap();
        assert_eq!(triangles.len(), p.outer().len() - 2);

        let total: f64 = triangles
            .iter()
            .map(|[a, b, c]| {
                let (a, b, c) = (p.outer()[*a], p.outer()[*b], p.outer()[*c]);
                cross(b - a, c - a).abs() * 0.5
            })
            .sum();
        assert!(
            (total - p.area()).abs() < 1e-9,
            "triangles cover {total}, profile is {}",
            p.area()
        );
    }

    #[test]
    fn every_triangle_is_wound_counter_clockwise() {
        let p = l_shape();
        for [a, b, c] in p.triangulate().unwrap() {
            let (a, b, c) = (p.outer()[a], p.outer()[b], p.outer()[c]);
            assert!(cross(b - a, c - a) > 0.0, "winding must be consistent");
        }
    }

    #[test]
    fn a_many_sided_profile_triangulates() {
        let p = Profile::circle(2.5, &TessellationSettings::fine()).unwrap();
        let n = p.outer().len();
        assert_eq!(p.triangulate().unwrap().len(), n - 2);
    }

    #[test]
    fn holes_subtract_area_but_are_not_yet_tessellated() {
        let p = Profile::rectangle(4.0, 4.0)
            .unwrap()
            .with_hole([
                DVec2::new(-1.0, -1.0),
                DVec2::new(1.0, -1.0),
                DVec2::new(1.0, 1.0),
                DVec2::new(-1.0, 1.0),
            ])
            .unwrap();

        assert!((p.area() - 12.0).abs() < 1e-12);
        assert!(
            signed_area(&p.holes()[0]) < 0.0,
            "holes are stored clockwise"
        );
        assert_eq!(p.triangulate(), Err(GeometryError::HolesNotSupported(1)));
    }
}
