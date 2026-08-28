//! Snapping.
//!
//! A drawn point almost never wants to be exactly where the cursor was. It wants to be on the
//! grid, or on the end of the wall you are extending, or on the corner you are closing to.
//! Without snapping, a model looks right and measures wrong — two walls that appear to meet
//! but are 3 mm apart, which becomes a quantity error and then an invoice.
//!
//! Pure geometry, deliberately: it knows about points and a grid, not about elements. The
//! caller supplies whatever candidates it thinks are interesting, which keeps this testable
//! and keeps the model out of the geometry crate.

use glam::DVec3;
use serde::{Deserialize, Serialize};

/// What a snap latched onto, so the interface can say why the point moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapKind {
    /// Nothing was near; the raw point was used.
    Free,
    /// Quantised to the grid.
    Grid,
    /// Latched to a candidate — a vertex, an endpoint, a centre.
    Point,
}

/// A snapped point and the reason it landed there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub point: DVec3,
    pub kind: SnapKind,
}

impl Snap {
    pub fn free(point: DVec3) -> Self {
        Self {
            point,
            kind: SnapKind::Free,
        }
    }

    pub fn is_free(&self) -> bool {
        self.kind == SnapKind::Free
    }
}

/// How aggressively to snap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SnapSettings {
    /// Grid spacing in metres. Zero or negative disables grid snapping.
    pub grid: f64,
    /// How close a candidate must be to win, in metres. Should scale with zoom: a tolerance
    /// that feels right at 1 m is unusable at 100 m.
    pub tolerance: f64,
    /// Whether the grid applies to the vertical axis too. Usually not — storeys set
    /// elevations, and rounding Z to the plan grid moves things between floors.
    pub grid_vertical: bool,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self {
            grid: 0.1,
            tolerance: 0.25,
            grid_vertical: false,
        }
    }
}

impl SnapSettings {
    /// Scale the tolerance to what a pixel is worth at this viewing distance.
    ///
    /// Snapping is a screen-space gesture wearing world-space clothes: the user means "near
    /// where I clicked", and how much world that covers depends entirely on zoom.
    pub fn at_scale(self, metres_per_pixel: f64, pixels: f64) -> Self {
        Self {
            tolerance: (metres_per_pixel * pixels).max(1e-6),
            ..self
        }
    }
}

/// Snap a point, preferring candidates over the grid.
///
/// Candidates win because they are what the user is aiming at. Landing on the grid *near* a
/// wall end, rather than on the end, is the failure that leaves models almost-joined.
pub fn snap(target: DVec3, candidates: &[DVec3], settings: &SnapSettings) -> Snap {
    let nearest = candidates
        .iter()
        .map(|candidate| (candidate, candidate.distance(target)))
        .filter(|(_, distance)| *distance <= settings.tolerance)
        // Deliberately not `min_by_key`: distances are floats, and ties should resolve to the
        // first candidate given, which is the caller's stated priority order.
        .fold(None, |best: Option<(&DVec3, f64)>, next| match best {
            Some((_, best_distance)) if best_distance <= next.1 => best,
            _ => Some(next),
        });

    if let Some((point, _)) = nearest {
        return Snap {
            point: *point,
            kind: SnapKind::Point,
        };
    }

    if settings.grid > 0.0 {
        return Snap {
            point: DVec3::new(
                quantise(target.x, settings.grid),
                quantise(target.y, settings.grid),
                if settings.grid_vertical {
                    quantise(target.z, settings.grid)
                } else {
                    target.z
                },
            ),
            kind: SnapKind::Grid,
        };
    }

    Snap::free(target)
}

/// Round to the nearest multiple, keeping non-finite input intact rather than turning it into
/// a plausible-looking zero.
fn quantise(value: f64, step: f64) -> f64 {
    if !value.is_finite() || step <= 0.0 {
        return value;
    }
    (value / step).round() * step
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> SnapSettings {
        SnapSettings {
            grid: 0.1,
            tolerance: 0.25,
            grid_vertical: false,
        }
    }

    #[test]
    fn a_nearby_candidate_wins_over_the_grid() {
        // The whole point: aiming at a wall end must land on the wall end, not on the grid
        // node next to it. Otherwise the walls look joined and measure 3 mm apart.
        let end = DVec3::new(4.03, 2.02, 0.0);
        let result = snap(DVec3::new(4.05, 2.04, 0.0), &[end], &settings());

        assert_eq!(result.kind, SnapKind::Point);
        assert_eq!(result.point, end);
    }

    #[test]
    fn a_distant_candidate_is_ignored_and_the_grid_applies() {
        let far = DVec3::new(10.0, 10.0, 0.0);
        let result = snap(DVec3::new(4.04, 2.06, 0.0), &[far], &settings());

        assert_eq!(result.kind, SnapKind::Grid);
        assert!((result.point.x - 4.0).abs() < 1e-12);
        assert!((result.point.y - 2.1).abs() < 1e-12);
    }

    #[test]
    fn the_closest_candidate_wins() {
        let near = DVec3::new(1.02, 0.0, 0.0);
        let nearer = DVec3::new(1.005, 0.0, 0.0);
        let result = snap(DVec3::new(1.0, 0.0, 0.0), &[near, nearer], &settings());
        assert_eq!(result.point, nearer);
    }

    #[test]
    fn elevation_is_left_alone_by_default() {
        // Storeys set elevations. Rounding Z to the plan grid quietly moves things between
        // floors, which is a much worse error than being 4 cm off in plan.
        let result = snap(DVec3::new(1.04, 1.04, 2.87), &[], &settings());
        assert_eq!(result.point.z, 2.87);

        let vertical = SnapSettings {
            grid_vertical: true,
            ..settings()
        };
        let snapped = snap(DVec3::new(1.04, 1.04, 2.87), &[], &vertical);
        assert!((snapped.point.z - 2.9).abs() < 1e-12);
    }

    #[test]
    fn a_disabled_grid_leaves_the_point_alone() {
        let free = SnapSettings {
            grid: 0.0,
            ..settings()
        };
        let target = DVec3::new(1.23456, 7.89, 0.5);
        let result = snap(target, &[], &free);

        assert_eq!(result.kind, SnapKind::Free);
        assert_eq!(result.point, target);
        assert!(result.is_free());
    }

    #[test]
    fn tolerance_scales_with_zoom() {
        // A 10-pixel reach is 0.1 m when zoomed in and 10 m when zoomed out. Fixing the
        // tolerance in metres makes snapping useless at one end or the other.
        let close = settings().at_scale(0.01, 10.0);
        let far = settings().at_scale(1.0, 10.0);
        assert!((close.tolerance - 0.1).abs() < 1e-12);
        assert!((far.tolerance - 10.0).abs() < 1e-12);
        assert!(close.tolerance < far.tolerance);
    }

    #[test]
    fn quantising_handles_negatives_and_halves() {
        assert!((quantise(-4.04, 0.1) + 4.0).abs() < 1e-12);
        assert!((quantise(0.05, 0.1) - 0.1).abs() < 1e-12);
        assert!((quantise(-0.05, 0.1) + 0.1).abs() < 1e-12);
        assert_eq!(
            quantise(1.5, 0.0),
            1.5,
            "a zero step must not divide by zero"
        );
        assert!(quantise(f64::NAN, 0.1).is_nan(), "NaN must not become 0.0");
    }

    #[test]
    fn snapping_is_idempotent() {
        // Snapping an already-snapped point must not move it again, or dragging drifts.
        let once = snap(DVec3::new(4.037, 2.061, 0.0), &[], &settings());
        let twice = snap(once.point, &[], &settings());
        assert!((twice.point - once.point).length() < 1e-12);
    }
}
