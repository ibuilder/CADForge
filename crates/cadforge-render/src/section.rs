//! Section planes.
//!
//! A section plane hides everything on one side of it, which is how you look inside a
//! building without deleting anything. It is pure view state: the model never learns a
//! section exists, and closing the view restores the geometry because the geometry was never
//! touched.
//!
//! GPU-free, like the rest of this crate's types. The clipping happens in a shader, but
//! deciding what a plane means does not need a GPU and is worth being able to test without
//! one.

use cadforge_core::BoundingBox;
use glam::DVec3;
use serde::{Deserialize, Serialize};

/// How many planes the shader uniform has room for.
///
/// Four covers a section box on three axes plus one oblique cut, which is more than any
/// interaction currently offers. It is a fixed array rather than a storage buffer because
/// four vec4s cost nothing and a storage binding would rule out some downlevel targets.
pub const MAX_SECTIONS: usize = 4;

/// A half-space. Everything on the positive side of the plane is hidden.
///
/// Stored as a normal and an offset, so `dot(normal, point) + offset > 0` is the test — the
/// same form the shader uses, which keeps the CPU and GPU answers identical by construction
/// rather than by agreement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SectionPlane {
    /// Unit normal pointing at the half-space that gets hidden.
    pub normal: DVec3,
    /// Signed distance from the origin along `-normal`.
    pub offset: f64,
}

impl SectionPlane {
    /// A plane through `point`, hiding everything the normal points toward.
    ///
    /// A degenerate normal yields a plane that hides nothing, rather than one that hides
    /// everything — failing open is the safer direction for a view filter.
    pub fn through(point: DVec3, normal: DVec3) -> Self {
        match normal.try_normalize() {
            Some(normal) => Self {
                normal,
                offset: -normal.dot(point),
            },
            None => Self::none(),
        }
    }

    /// A plane that hides nothing.
    pub fn none() -> Self {
        Self {
            normal: DVec3::ZERO,
            offset: -1.0,
        }
    }

    /// Cut a box in half along an axis, hiding the positive side.
    ///
    /// The common gesture: section the model at its own centre and look in.
    pub fn halving(bounds: &BoundingBox, axis: DVec3) -> Self {
        Self::through(bounds.center(), axis)
    }

    /// Whether a point survives this plane.
    pub fn keeps(&self, point: DVec3) -> bool {
        self.normal.dot(point) + self.offset <= 0.0
    }

    pub fn is_inactive(&self) -> bool {
        self.normal == DVec3::ZERO
    }

    /// Slide the plane along its own normal.
    pub fn offset_by(self, distance: f64) -> Self {
        Self {
            offset: self.offset - distance,
            ..self
        }
    }

    /// `[nx, ny, nz, offset]`, the layout the shader uniform expects.
    pub fn to_array(self) -> [f32; 4] {
        [
            self.normal.x as f32,
            self.normal.y as f32,
            self.normal.z as f32,
            self.offset as f32,
        ]
    }
}

impl Default for SectionPlane {
    fn default() -> Self {
        Self::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plane_hides_the_side_its_normal_points_at() {
        let plane = SectionPlane::through(DVec3::ZERO, DVec3::X);
        assert!(
            !plane.keeps(DVec3::new(1.0, 0.0, 0.0)),
            "+X should be hidden"
        );
        assert!(plane.keeps(DVec3::new(-1.0, 0.0, 0.0)), "-X should survive");
        // A point exactly on the plane is kept, so a section flush with a face does not make
        // that face flicker.
        assert!(plane.keeps(DVec3::ZERO));
    }

    #[test]
    fn halving_cuts_through_the_centre() {
        let bounds = BoundingBox::new(DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 4.0, 3.0));
        let plane = SectionPlane::halving(&bounds, DVec3::X);

        assert!(plane.keeps(DVec3::new(4.9, 2.0, 1.5)));
        assert!(!plane.keeps(DVec3::new(5.1, 2.0, 1.5)));
    }

    #[test]
    fn sliding_moves_the_cut_along_the_normal() {
        let plane = SectionPlane::through(DVec3::ZERO, DVec3::Z);
        assert!(!plane.keeps(DVec3::new(0.0, 0.0, 1.0)));

        // Pushing the plane 2 m up puts that point back on the visible side.
        let raised = plane.offset_by(2.0);
        assert!(raised.keeps(DVec3::new(0.0, 0.0, 1.0)));
        assert!(!raised.keeps(DVec3::new(0.0, 0.0, 3.0)));
    }

    #[test]
    fn an_inactive_plane_hides_nothing() {
        let plane = SectionPlane::none();
        assert!(plane.is_inactive());
        for point in [DVec3::ZERO, DVec3::splat(1e6), DVec3::splat(-1e6)] {
            assert!(plane.keeps(point));
        }
    }

    #[test]
    fn a_degenerate_normal_fails_open() {
        // Hiding everything because a caller passed a zero vector would look like a crash.
        let plane = SectionPlane::through(DVec3::ONE, DVec3::ZERO);
        assert!(plane.is_inactive());
        assert!(plane.keeps(DVec3::splat(100.0)));
    }

    #[test]
    fn the_normal_is_normalised_so_the_offset_is_metric() {
        let plane = SectionPlane::through(DVec3::new(0.0, 0.0, 2.0), DVec3::Z * 17.0);
        assert!((plane.normal.length() - 1.0).abs() < 1e-12);
        // Offset is now a real distance, which is what makes offset_by move in metres.
        assert!((plane.offset + 2.0).abs() < 1e-12);
    }

    #[test]
    fn the_shader_layout_matches_the_test() {
        let plane = SectionPlane::through(DVec3::new(1.0, 0.0, 0.0), DVec3::X);
        let [nx, ny, nz, offset] = plane.to_array();
        // Same expression the shader evaluates, so CPU and GPU cannot drift apart.
        let evaluate =
            |p: DVec3| nx as f64 * p.x + ny as f64 * p.y + nz as f64 * p.z + offset as f64;
        assert!(evaluate(DVec3::new(2.0, 0.0, 0.0)) > 0.0);
        assert!(evaluate(DVec3::new(0.0, 0.0, 0.0)) < 0.0);
    }
}
