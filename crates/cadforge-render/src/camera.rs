//! An orbit camera.
//!
//! Orbit-around-a-target is the right default for building models: the user is nearly always
//! inspecting an object or a room rather than walking. Projection follows the wgpu convention
//! of a 0..1 depth range, matching `glam`'s `perspective_rh`.

use cadforge_core::BoundingBox;
use glam::{DMat4, DVec3, Mat4};
// glam 0.33 deprecated the inherent `DMat4::look_at_rh` / `perspective_rh` constructors in
// favour of these explicit modules. `directx` is the 0..1 depth, Y-up convention wgpu uses;
// the `vulkan` sibling flips Y and would render the whole scene upside down.
use glam::dcamera::rh::proj::directx::perspective;
use glam::dcamera::rh::view::look_at_mat4;
use serde::{Deserialize, Serialize};

/// Pitch is clamped just short of vertical: looking exactly along the up axis makes the view
/// matrix degenerate and the camera flip.
const MAX_PITCH: f64 = std::f64::consts::FRAC_PI_2 - 1e-3;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub target: DVec3,
    /// Distance from the target.
    pub distance: f64,
    /// Rotation about the up axis, radians.
    pub yaw: f64,
    /// Elevation above the horizon, radians. Clamped away from vertical.
    pub pitch: f64,
    /// Vertical field of view, radians.
    pub fov_y: f64,
    pub aspect: f64,
    pub z_near: f64,
    pub z_far: f64,
    /// World up. Z-up, because IFC is Z-up and converting at the camera is one less place to
    /// get it wrong.
    pub up: DVec3,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            target: DVec3::ZERO,
            distance: 10.0,
            yaw: std::f64::consts::FRAC_PI_4,
            pitch: std::f64::consts::FRAC_PI_6,
            fov_y: std::f64::consts::FRAC_PI_4,
            aspect: 16.0 / 9.0,
            z_near: 0.05,
            z_far: 5_000.0,
            up: DVec3::Z,
        }
    }
}

impl Camera {
    /// Eye position, derived from the orbit parameters.
    pub fn eye(&self) -> DVec3 {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        self.target
            + DVec3::new(
                self.distance * cos_pitch * cos_yaw,
                self.distance * cos_pitch * sin_yaw,
                self.distance * sin_pitch,
            )
    }

    pub fn view(&self) -> DMat4 {
        look_at_mat4(self.eye(), self.target, self.up)
    }

    pub fn projection(&self) -> DMat4 {
        perspective(self.fov_y, self.aspect.max(1e-6), self.z_near, self.z_far)
    }

    pub fn view_projection(&self) -> DMat4 {
        self.projection() * self.view()
    }

    /// The matrix as the GPU wants it. Narrowing happens here and nowhere else.
    pub fn view_projection_f32(&self) -> Mat4 {
        self.view_projection().as_mat4()
    }

    /// Orbit by deltas in radians.
    pub fn orbit(&mut self, delta_yaw: f64, delta_pitch: f64) {
        self.yaw = (self.yaw + delta_yaw).rem_euclid(std::f64::consts::TAU);
        self.pitch = (self.pitch + delta_pitch).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Multiply the orbit distance. `factor < 1` moves closer.
    ///
    /// Multiplicative rather than additive so that zoom feels the same at 2 m and at 200 m,
    /// and cannot cross zero into an inverted view.
    pub fn dolly(&mut self, factor: f64) {
        if factor > 0.0 && factor.is_finite() {
            self.distance = (self.distance * factor).clamp(1e-3, 1e7);
        }
    }

    /// Pan in screen space, in metres at the target plane.
    pub fn pan(&mut self, right: f64, up: f64) {
        let view = self.view();
        // View-space basis vectors live in the rows of the view matrix.
        let right_axis = DVec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up_axis = DVec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        self.target += right_axis * right + up_axis * up;
    }

    /// Frame a bounding box: centre on it and back off far enough to fit it.
    pub fn frame(&mut self, bounds: &BoundingBox) {
        if bounds.is_empty() {
            return;
        }
        self.target = bounds.center();
        let radius = bounds.size().length() * 0.5;
        if radius <= 0.0 {
            self.distance = 1.0;
            return;
        }
        // Fit the vertical field of view, then allow for a narrow viewport, plus margin.
        let fit_vertical = radius / (self.fov_y * 0.5).tan();
        let horizontal_fov = 2.0 * ((self.fov_y * 0.5).tan() * self.aspect.max(1e-6)).atan();
        let fit_horizontal = radius / (horizontal_fov * 0.5).tan();
        self.distance = fit_vertical.max(fit_horizontal) * 1.2;

        // Keep the clip range sane for the new scale, or the depth buffer collapses.
        self.z_near = (self.distance * 1e-4).max(1e-3);
        self.z_far = (self.distance + radius * 4.0).max(self.z_near * 10.0);
    }

    pub fn set_viewport(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.aspect = width as f64 / height as f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eye_sits_at_the_orbit_distance() {
        let camera = Camera::default();
        assert!((camera.eye().distance(camera.target) - camera.distance).abs() < 1e-9);
    }

    #[test]
    fn the_view_matrix_puts_the_target_down_the_negative_z_axis() {
        let camera = Camera::default();
        let target_in_view = camera.view().transform_point3(camera.target);
        assert!(target_in_view.x.abs() < 1e-9);
        assert!(target_in_view.y.abs() < 1e-9);
        assert!(
            (target_in_view.z + camera.distance).abs() < 1e-9,
            "right-handed view space looks down -Z"
        );
    }

    #[test]
    fn pitch_is_clamped_away_from_vertical() {
        let mut camera = Camera::default();
        camera.orbit(0.0, 100.0);
        assert!(camera.pitch < std::f64::consts::FRAC_PI_2);
        assert!(
            camera.view().determinant().abs() > 1e-9,
            "view must stay invertible"
        );

        camera.orbit(0.0, -200.0);
        assert!(camera.pitch > -std::f64::consts::FRAC_PI_2);
        assert!(camera.view().determinant().abs() > 1e-9);
    }

    #[test]
    fn yaw_wraps_instead_of_growing_without_bound() {
        let mut camera = Camera::default();
        for _ in 0..1000 {
            camera.orbit(1.0, 0.0);
        }
        assert!((0.0..std::f64::consts::TAU).contains(&camera.yaw));
    }

    #[test]
    fn dolly_is_multiplicative_and_cannot_invert() {
        let mut camera = Camera::default();
        camera.dolly(0.5);
        assert!((camera.distance - 5.0).abs() < 1e-12);

        camera.dolly(-1.0); // rejected
        camera.dolly(0.0); // rejected
        camera.dolly(f64::NAN); // rejected
        assert!(camera.distance > 0.0);
        assert!((camera.distance - 5.0).abs() < 1e-12);
    }

    #[test]
    fn framing_fits_the_box_in_view() {
        let mut camera = Camera::default();
        let bounds = BoundingBox::new(DVec3::new(-20.0, -5.0, 0.0), DVec3::new(20.0, 5.0, 12.0));
        camera.frame(&bounds);

        assert_eq!(camera.target, bounds.center());

        // Every corner must land inside the clip volume.
        let view_proj = camera.view_projection();
        for i in 0..8 {
            let corner = DVec3::new(
                if i & 1 == 0 {
                    bounds.min.x
                } else {
                    bounds.max.x
                },
                if i & 2 == 0 {
                    bounds.min.y
                } else {
                    bounds.max.y
                },
                if i & 4 == 0 {
                    bounds.min.z
                } else {
                    bounds.max.z
                },
            );
            let clip = view_proj * corner.extend(1.0);
            assert!(clip.w > 0.0, "corner {i} is behind the camera");
            let ndc = clip.truncate() / clip.w;
            assert!(
                ndc.x.abs() <= 1.0,
                "corner {i} is off screen: x = {}",
                ndc.x
            );
            assert!(
                ndc.y.abs() <= 1.0,
                "corner {i} is off screen: y = {}",
                ndc.y
            );
            assert!(
                (0.0..=1.0).contains(&ndc.z),
                "corner {i} is outside the depth range"
            );
        }
    }

    #[test]
    fn framing_an_empty_box_leaves_the_camera_alone() {
        let mut camera = Camera::default();
        let before = camera;
        camera.frame(&BoundingBox::empty());
        assert_eq!(camera, before);
    }

    #[test]
    fn framing_adjusts_the_clip_range_to_the_scale() {
        let mut camera = Camera::default();
        camera.frame(&BoundingBox::new(DVec3::ZERO, DVec3::splat(2000.0)));
        assert!(camera.z_far > camera.distance);
        assert!(camera.z_near > 0.0);
        assert!(
            camera.z_far / camera.z_near < 1e8,
            "depth precision would collapse"
        );
    }

    #[test]
    fn panning_moves_the_target_across_the_view() {
        let mut camera = Camera::default();
        let before = camera.target;
        camera.pan(3.0, 0.0);
        let moved = camera.target - before;

        assert!((moved.length() - 3.0).abs() < 1e-9);
        // A pure sideways pan must not move along the viewing direction.
        let forward = (camera.target - camera.eye()).normalize();
        assert!(moved.normalize().dot(forward).abs() < 1e-9);
    }

    #[test]
    fn a_degenerate_viewport_does_not_produce_nan() {
        let mut camera = Camera::default();
        camera.set_viewport(0, 0);
        assert!(camera.aspect.is_finite() && camera.aspect > 0.0);
        camera.set_viewport(1920, 1080);
        assert!((camera.aspect - 16.0 / 9.0).abs() < 1e-9);
        assert!(camera.view_projection_f32().is_finite());
    }
}
