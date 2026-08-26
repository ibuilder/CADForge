//! Frustum culling.
//!
//! Six planes extracted from the view-projection matrix (Gribb–Hartmann), tested against
//! element bounds. The cheapest large win available on a building model: most of a project is
//! behind you or outside the view most of the time.

use cadforge_core::BoundingBox;
use glam::{DMat4, DVec3, DVec4};

/// A view frustum as six inward-facing planes.
///
/// Each plane is `(a, b, c, d)` with `ax + by + cz + d ≥ 0` inside.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    planes: [DVec4; 6],
}

impl Frustum {
    /// Extract from a view-projection matrix.
    ///
    /// Assumes a 0..1 depth range (wgpu, Metal, Vulkan, DX12 — and `glam::perspective_rh`),
    /// which is why the near plane is row 2 alone rather than `row3 + row2` as it would be
    /// for OpenGL's -1..1 range. Getting this wrong culls everything near the camera.
    pub fn from_view_projection(view_projection: DMat4) -> Self {
        let m = view_projection;
        let row = |i: usize| -> DVec4 { m.row(i) };
        let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));

        let planes = [
            r3 + r0, // left
            r3 - r0, // right
            r3 + r1, // bottom
            r3 - r1, // top
            r2,      // near (0..1 depth)
            r3 - r2, // far
        ];

        // Normalise so plane distances are metric, which lets callers use them for LOD
        // selection rather than only for a boolean test.
        Self {
            planes: planes.map(|p| {
                let length = p.truncate().length();
                if length > 0.0 {
                    p / length
                } else {
                    p
                }
            }),
        }
    }

    /// Whether a box is at least partly inside.
    ///
    /// Conservative: it can return true for a box just outside a frustum corner. That costs a
    /// few wasted draws; the opposite error makes geometry vanish, so the bias is deliberate.
    pub fn intersects(&self, bounds: &BoundingBox) -> bool {
        if bounds.is_empty() {
            return false;
        }
        // A box is outside if it lies entirely behind any one plane. Testing the box corner
        // furthest along the plane normal answers that in one dot product.
        !self.planes.iter().any(|plane| {
            let normal = plane.truncate();
            let positive = DVec3::new(
                if normal.x >= 0.0 {
                    bounds.max.x
                } else {
                    bounds.min.x
                },
                if normal.y >= 0.0 {
                    bounds.max.y
                } else {
                    bounds.min.y
                },
                if normal.z >= 0.0 {
                    bounds.max.z
                } else {
                    bounds.min.z
                },
            );
            normal.dot(positive) + plane.w < 0.0
        })
    }

    pub fn contains_point(&self, point: DVec3) -> bool {
        self.planes
            .iter()
            .all(|plane| plane.truncate().dot(point) + plane.w >= 0.0)
    }

    /// Signed distance from a point to the near plane. Positive in front. Used for LOD.
    pub fn depth_of(&self, point: DVec3) -> f64 {
        let near = self.planes[4];
        near.truncate().dot(point) + near.w
    }

    pub fn planes(&self) -> &[DVec4; 6] {
        &self.planes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    fn frustum() -> Frustum {
        let camera = Camera {
            target: DVec3::ZERO,
            distance: 20.0,
            ..Camera::default()
        };
        Frustum::from_view_projection(camera.view_projection())
    }

    #[test]
    fn the_target_is_inside() {
        assert!(frustum().contains_point(DVec3::ZERO));
    }

    #[test]
    fn geometry_behind_the_camera_is_culled() {
        let camera = Camera::default();
        let frustum = Frustum::from_view_projection(camera.view_projection());
        // Twice the orbit distance past the eye, directly away from the target.
        let behind = camera.eye() + (camera.eye() - camera.target).normalize() * 50.0;
        assert!(!frustum.contains_point(behind));
        assert!(!frustum.intersects(&BoundingBox::new(behind - DVec3::ONE, behind + DVec3::ONE)));
    }

    #[test]
    fn geometry_far_off_to_the_side_is_culled() {
        let frustum = frustum();
        let far_off = BoundingBox::new(
            DVec3::new(10_000.0, 10_000.0, 0.0),
            DVec3::new(10_001.0, 10_001.0, 1.0),
        );
        assert!(!frustum.intersects(&far_off));
    }

    #[test]
    fn a_box_straddling_the_camera_is_kept() {
        // A huge box containing the eye must never be culled — this is the failure that makes
        // the ground plane disappear when you stand on it.
        let frustum = frustum();
        let huge = BoundingBox::new(DVec3::splat(-1000.0), DVec3::splat(1000.0));
        assert!(frustum.intersects(&huge));
    }

    #[test]
    fn an_empty_box_is_culled() {
        assert!(!frustum().intersects(&BoundingBox::empty()));
    }

    #[test]
    fn planes_are_normalised_so_distances_are_metric() {
        for plane in frustum().planes() {
            let length = plane.truncate().length();
            assert!(
                (length - 1.0).abs() < 1e-9,
                "plane normal length was {length}"
            );
        }
    }

    #[test]
    fn depth_increases_away_from_the_camera() {
        let camera = Camera {
            target: DVec3::ZERO,
            distance: 20.0,
            ..Camera::default()
        };
        let frustum = Frustum::from_view_projection(camera.view_projection());

        let forward = (camera.target - camera.eye()).normalize();
        let near_point = camera.eye() + forward * 5.0;
        let far_point = camera.eye() + forward * 40.0;
        assert!(frustum.depth_of(far_point) > frustum.depth_of(near_point));
        assert!(frustum.depth_of(near_point) > 0.0);
    }

    #[test]
    fn culling_a_grid_keeps_a_sensible_fraction() {
        // A sanity check on the whole pipeline: from a framed view of a 20 × 20 grid, most
        // cells should survive, and some should not.
        let mut camera = Camera::default();
        let cells: Vec<BoundingBox> = (0..20)
            .flat_map(|x| {
                (0..20).map(move |y| {
                    let origin = DVec3::new(x as f64 * 4.0, y as f64 * 4.0, 0.0);
                    BoundingBox::new(origin, origin + DVec3::new(3.0, 3.0, 3.0))
                })
            })
            .collect();
        let all = cells.iter().fold(BoundingBox::empty(), |b, c| b.union(*c));
        camera.frame(&all);

        let frustum = Frustum::from_view_projection(camera.view_projection());
        let visible = cells.iter().filter(|c| frustum.intersects(c)).count();
        assert_eq!(
            visible,
            cells.len(),
            "a framed view must contain everything"
        );

        // Now zoom in hard; most of the grid should fall away.
        camera.dolly(0.05);
        let close = Frustum::from_view_projection(camera.view_projection());
        let still_visible = cells.iter().filter(|c| close.intersects(c)).count();
        assert!(
            still_visible < cells.len(),
            "zooming in must cull something, kept {still_visible}"
        );
    }
}
