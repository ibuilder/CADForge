//! Tessellation quality.
//!
//! Segment counts are derived from a chord tolerance, never hard-coded. A 50 mm pipe and a
//! 5 m silo then meet the same visual tolerance, instead of the pipe being over-tessellated
//! and the silo visibly faceted.

use serde::{Deserialize, Serialize};

/// How finely curved geometry is approximated.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TessellationSettings {
    /// Maximum chord deviation, in metres. The sagitta between the true curve and its chord.
    pub deflection: f64,
    /// Maximum angle per segment, in radians. Bounds segment count on very large radii.
    pub angular: f64,
    pub min_segments: usize,
    pub max_segments: usize,
}

impl TessellationSettings {
    /// 5 mm chord tolerance — the working default for interactive viewing.
    pub fn standard() -> Self {
        Self {
            deflection: 0.005,
            angular: std::f64::consts::FRAC_PI_8,
            min_segments: 8,
            max_segments: 360,
        }
    }

    /// 1 mm — export, fabrication, close inspection.
    pub fn fine() -> Self {
        Self {
            deflection: 0.001,
            angular: std::f64::consts::FRAC_PI_8 * 0.5,
            min_segments: 12,
            max_segments: 720,
        }
    }

    /// 20 mm — distant LODs and mobile, where triangle budget matters more than silhouette
    /// (ADR-0006).
    pub fn coarse() -> Self {
        Self {
            deflection: 0.02,
            angular: std::f64::consts::FRAC_PI_4,
            min_segments: 6,
            max_segments: 64,
        }
    }

    /// Segments needed to approximate a full circle of this radius within tolerance.
    ///
    /// From the sagitta of a chord subtending `2π/n`: `s = r · (1 − cos(π/n))`. Solving for
    /// `n` given `s ≤ deflection` gives `n ≥ π / arccos(1 − deflection/r)`.
    pub fn circle_segments(&self, radius: f64) -> usize {
        let r = radius.abs();
        let from_deflection = if !r.is_finite() || r <= self.deflection {
            // The tolerance is as large as the circle; the minimum is already enough.
            self.min_segments
        } else {
            let ratio = (1.0 - self.deflection / r).clamp(-1.0, 1.0);
            let n = std::f64::consts::PI / ratio.acos();
            if n.is_finite() {
                n.ceil() as usize
            } else {
                self.max_segments
            }
        };
        let from_angle = (std::f64::consts::TAU / self.angular).ceil() as usize;
        from_deflection
            .max(from_angle)
            .clamp(self.min_segments, self.max_segments)
    }
}

impl Default for TessellationSettings {
    fn default() -> Self {
        Self::standard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigger_radius_needs_more_segments() {
        let s = TessellationSettings::standard();
        assert!(s.circle_segments(10.0) > s.circle_segments(0.1));
    }

    #[test]
    fn finer_tolerance_needs_more_segments() {
        let r = 1.0;
        assert!(
            TessellationSettings::fine().circle_segments(r)
                > TessellationSettings::coarse().circle_segments(r)
        );
    }

    #[test]
    fn segment_count_stays_within_bounds() {
        let s = TessellationSettings::standard();
        for radius in [0.0, 1e-9, 0.001, 1.0, 1e6, f64::INFINITY] {
            let n = s.circle_segments(radius);
            assert!(
                (s.min_segments..=s.max_segments).contains(&n),
                "radius {radius} gave {n} segments"
            );
        }
    }

    #[test]
    fn the_chord_tolerance_is_actually_met() {
        let s = TessellationSettings::fine();
        for radius in [0.05, 0.5, 5.0, 50.0] {
            let n = s.circle_segments(radius) as f64;
            let sagitta = radius * (1.0 - (std::f64::consts::PI / n).cos());
            assert!(
                sagitta <= s.deflection * 1.001,
                "radius {radius}: sagitta {sagitta} exceeds tolerance {}",
                s.deflection
            );
        }
    }
}
