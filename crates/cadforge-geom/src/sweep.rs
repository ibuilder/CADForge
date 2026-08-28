//! Linear sweeps.
//!
//! This is the workhorse of the whole geometry strategy. Walls, slabs, columns, beams,
//! openings, coverings, ducts, and pipes are all a closed profile swept along a direction —
//! which is exactly `IfcExtrudedAreaSolid` over `IfcArbitraryClosedProfileDef`, so the result
//! exports as **native parametric IFC** rather than as a tessellated blob (ADR-0004).

use crate::mesh::IndexedMesh;
use crate::profile::Profile;
use crate::GeometryError;
use glam::DVec3;

/// Extrude a profile along +Z. The common case: the profile is already in its element's
/// local plane.
pub fn extrude(profile: &Profile, depth: f64) -> Result<IndexedMesh, GeometryError> {
    extrude_along(profile, DVec3::Z, depth)
}

/// Extrude a profile along an arbitrary direction.
///
/// **The profile does not rotate.** It stays in its own XY plane and the far cap is that same
/// profile displaced by `direction * depth`, which for an off-axis direction gives an oblique
/// prism. This is what `IfcExtrudedAreaSolid` means: the profile lies in the XY plane of
/// `Position`, and `ExtrudedDirection` is a vector expressed in that same coordinate system,
/// not a new local Z.
///
/// Re-basing the profile onto `direction` instead looks identical for a +Z sweep and for
/// volume, which is how it survived here for two phases. It is visibly wrong the moment
/// anything sweeps downward: the basis completing a right-handed frame with -Z mirrors the
/// profile in Y, so a slab lands on the wrong side of its own outline.
///
/// A negative depth extrudes the other way and still produces outward-facing normals. A
/// direction lying in the profile's own plane encloses nothing and is refused.
pub fn extrude_along(
    profile: &Profile,
    direction: DVec3,
    depth: f64,
) -> Result<IndexedMesh, GeometryError> {
    if !depth.is_finite() || depth == 0.0 {
        return Err(GeometryError::InvalidDepth(depth));
    }
    if !direction.is_finite() {
        return Err(GeometryError::NonFinite);
    }
    let axis = direction
        .try_normalize()
        .ok_or(GeometryError::DegenerateDirection)?;

    let triangles = profile.triangulate()?;
    let along = axis * depth;

    // A sweep parallel to the profile's own plane encloses no volume. Refused rather than
    // returned as a flat shell, because a zero-volume "solid" corrupts every boolean and
    // every quantity downstream.
    if along.z.abs() < f64::EPSILON {
        return Err(GeometryError::DegenerateDirection);
    }

    // The profile is counter-clockwise in XY, so its normal is +Z, and the cap windings below
    // assume `top` is the ring on the +Z side. When the sweep goes down, the profile itself is
    // the top and the displaced ring is the bottom.
    let (to_bottom, to_top) = if along.z > 0.0 {
        (DVec3::ZERO, along)
    } else {
        (along, DVec3::ZERO)
    };

    let outer = profile.outer();
    let bottom: Vec<DVec3> = outer
        .iter()
        .map(|p| DVec3::new(p.x, p.y, 0.0) + to_bottom)
        .collect();
    let top: Vec<DVec3> = outer
        .iter()
        .map(|p| DVec3::new(p.x, p.y, 0.0) + to_top)
        .collect();

    let n = outer.len();
    let mut mesh = IndexedMesh::with_capacity(triangles.len() * 2 + n * 2);

    // Bottom cap: reversed, so its normal faces away from the extrusion.
    for [a, b, c] in &triangles {
        mesh.push_triangle(bottom[*a], bottom[*c], bottom[*b]);
    }
    // Top cap: as triangulated.
    for [a, b, c] in &triangles {
        mesh.push_triangle(top[*a], top[*b], top[*c]);
    }
    // Sides. The profile is counter-clockwise, so this winding faces outward.
    for i in 0..n {
        let j = (i + 1) % n;
        mesh.push_triangle(bottom[i], bottom[j], top[j]);
        mesh.push_triangle(bottom[i], top[j], top[i]);
    }

    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tess::TessellationSettings;

    #[test]
    fn a_box_has_the_expected_volume_and_is_closed() {
        let profile = Profile::rectangle(2.0, 3.0).unwrap();
        let mesh = extrude(&profile, 4.0).unwrap();

        assert!(mesh.is_well_formed());
        // 2 caps × 2 triangles + 4 sides × 2 triangles.
        assert_eq!(mesh.triangle_count(), 12);
        assert!(
            (mesh.signed_volume() - 24.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
        // 2 × (2×3) caps + perimeter 10 × height 4.
        assert!((mesh.surface_area() - 52.0).abs() < 1e-9);
    }

    #[test]
    fn normals_point_outward() {
        // A positive signed volume is exactly the statement that every normal faces out.
        let mesh = extrude(&Profile::rectangle(1.0, 1.0).unwrap(), 1.0).unwrap();
        assert!(mesh.signed_volume() > 0.0);

        // And explicitly: the top cap must face +Z.
        let top_normals = mesh.normals.iter().filter(|n| n.z > 0.9).count();
        assert!(
            top_normals >= 6,
            "expected a +Z cap, found {top_normals} vertices"
        );
    }

    #[test]
    fn negative_depth_extrudes_the_other_way_and_stays_outward() {
        let profile = Profile::rectangle(2.0, 2.0).unwrap();
        let down = extrude(&profile, -3.0).unwrap();

        assert!(
            down.signed_volume() > 0.0,
            "winding must survive a negative depth"
        );
        assert!((down.signed_volume() - 12.0).abs() < 1e-9);
        let bounds = down.bounds();
        assert!((bounds.min.z + 3.0).abs() < 1e-12);
        assert!(bounds.max.z.abs() < 1e-12);
    }

    #[test]
    fn an_off_axis_sweep_shears_the_solid_rather_than_rotating_it() {
        // The profile keeps its own plane, so the result is an oblique prism and Cavalieri
        // applies: volume is the base area times the sweep's rise, not times its length.
        //
        // The test this replaced compared volume and surface area against a +Z sweep and
        // called it "a rigid rotation must not change volume". Both quantities are invariant
        // under a rotation, so it agreed with the wrong implementation for two phases.
        let profile = Profile::rectangle(2.0, 3.0).unwrap();
        let skew = extrude_along(&profile, DVec3::new(1.0, 1.0, 1.0), 4.0).unwrap();

        let rise = 4.0 / 3.0f64.sqrt();
        assert!(
            (skew.signed_volume() - 6.0 * rise).abs() < 1e-9,
            "got {}",
            skew.signed_volume()
        );

        // And the base is exactly where the profile was, not somewhere a basis put it.
        let bounds = skew.bounds();
        assert!((bounds.min.x + 1.0).abs() < 1e-12 && (bounds.min.y + 1.5).abs() < 1e-12);
        assert!(bounds.min.z.abs() < 1e-12);
    }

    #[test]
    fn sweeping_down_does_not_mirror_the_profile() {
        // A slab is a plan outline extruded downward so its top face is the level it was set
        // out on. Re-basing the profile onto -Z flips it in Y, which put a 7x5 m slab
        // alongside the room it was drawn in instead of underneath it.
        let profile = Profile::new([
            glam::DVec2::new(0.0, 0.0),
            glam::DVec2::new(7.0, 0.0),
            glam::DVec2::new(7.0, 5.0),
            glam::DVec2::new(0.0, 5.0),
        ])
        .unwrap();
        let slab = extrude_along(&profile, DVec3::NEG_Z, 0.2).unwrap();

        let bounds = slab.bounds();
        assert!((bounds.min.y).abs() < 1e-12, "got {:?}", bounds.min);
        assert!((bounds.max.y - 5.0).abs() < 1e-12);
        assert!((bounds.min.z + 0.2).abs() < 1e-12);
        assert!(bounds.max.z.abs() < 1e-12);
        // Still a solid with outward normals, not an inside-out one.
        assert!((slab.signed_volume() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn a_sweep_inside_the_profile_plane_is_refused() {
        let profile = Profile::rectangle(2.0, 2.0).unwrap();
        assert_eq!(
            extrude_along(&profile, DVec3::X, 1.0),
            Err(GeometryError::DegenerateDirection)
        );
    }

    #[test]
    fn a_concave_profile_extrudes_correctly() {
        // L-shape: area 5, so a 2 m extrusion is 10 m³.
        let profile = Profile::new([
            glam::DVec2::new(0.0, 0.0),
            glam::DVec2::new(3.0, 0.0),
            glam::DVec2::new(3.0, 1.0),
            glam::DVec2::new(1.0, 1.0),
            glam::DVec2::new(1.0, 3.0),
            glam::DVec2::new(0.0, 3.0),
        ])
        .unwrap();
        let mesh = extrude(&profile, 2.0).unwrap();
        assert!(
            (mesh.signed_volume() - 10.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
    }

    #[test]
    fn a_cylinder_approaches_the_analytic_volume() {
        let profile = Profile::circle(1.0, &TessellationSettings::fine()).unwrap();
        let mesh = extrude(&profile, 2.0).unwrap();
        let exact = std::f64::consts::PI * 1.0 * 1.0 * 2.0;
        let error = (exact - mesh.signed_volume()) / exact;
        assert!(error > 0.0, "an inscribed prism must under-estimate");
        // The tolerance bounds *chord distance*, not volume: a 1 mm sagitta at r = 1 m gives
        // 71 segments and ~0.14% volume error. Asserting a tighter volume bound here would be
        // asserting something the setting never promised.
        assert!(error < 0.002, "relative error was {error}");
    }

    #[test]
    fn invalid_input_is_rejected_loudly() {
        let profile = Profile::rectangle(1.0, 1.0).unwrap();
        assert_eq!(
            extrude(&profile, 0.0),
            Err(GeometryError::InvalidDepth(0.0))
        );
        assert!(matches!(
            extrude(&profile, f64::NAN),
            Err(GeometryError::InvalidDepth(_))
        ));
        assert_eq!(
            extrude_along(&profile, DVec3::ZERO, 1.0),
            Err(GeometryError::DegenerateDirection)
        );
        assert_eq!(
            extrude_along(&profile, DVec3::new(f64::NAN, 0.0, 1.0), 1.0),
            Err(GeometryError::NonFinite)
        );
    }

    #[test]
    fn the_same_input_always_produces_the_same_mesh() {
        // Determinism is what makes meshes cacheable by hash and golden tests meaningful.
        let profile = Profile::circle(0.75, &TessellationSettings::standard()).unwrap();
        let a = extrude_along(&profile, DVec3::new(0.3, -0.5, 1.0), 2.5).unwrap();
        let b = extrude_along(&profile, DVec3::new(0.3, -0.5, 1.0), 2.5).unwrap();
        assert_eq!(a, b);
    }
}
