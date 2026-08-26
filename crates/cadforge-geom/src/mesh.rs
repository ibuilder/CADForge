//! Triangle meshes — the derived, disposable output of geometry evaluation.
//!
//! A mesh is never authoritative. Delete every mesh in the system and the model is intact
//! (`docs/ifc-semantics.md` ADR-001).

use cadforge_core::BoundingBox;
use glam::{DMat4, DVec3};
use serde::{Deserialize, Serialize};

/// An indexed triangle mesh with per-vertex normals.
///
/// Faces do not share vertices: each triangle contributes three, so flat shading is exact and
/// hard edges stay hard. Sweeps produce hard-edged geometry almost everywhere, and smoothing
/// them by accident looks wrong on a building.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexedMesh {
    pub positions: Vec<DVec3>,
    pub normals: Vec<DVec3>,
    pub indices: Vec<u32>,
}

impl IndexedMesh {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(triangles: usize) -> Self {
        Self {
            positions: Vec::with_capacity(triangles * 3),
            normals: Vec::with_capacity(triangles * 3),
            indices: Vec::with_capacity(triangles * 3),
        }
    }

    /// Append a triangle, deriving its normal from the winding.
    ///
    /// Degenerate triangles are dropped rather than stored with a NaN normal — a zero-area
    /// face is worthless to the renderer and poisons downstream normal maths.
    pub fn push_triangle(&mut self, a: DVec3, b: DVec3, c: DVec3) {
        let normal = match (b - a).cross(c - a).try_normalize() {
            Some(n) => n,
            None => return,
        };
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&[a, b, c]);
        self.normals.extend_from_slice(&[normal; 3]);
        self.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    /// Append another mesh, rebasing its indices.
    pub fn append(&mut self, other: &IndexedMesh) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.normals.extend_from_slice(&other.normals);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn bounds(&self) -> BoundingBox {
        BoundingBox::from_points(self.positions.iter().copied())
    }

    /// Transform in place. Normals get the inverse transpose, so non-uniform scale still
    /// produces correct lighting.
    pub fn transform(&mut self, matrix: DMat4) {
        let normal_matrix = matrix.inverse().transpose();
        for p in &mut self.positions {
            *p = matrix.transform_point3(*p);
        }
        for n in &mut self.normals {
            let transformed = normal_matrix.transform_vector3(*n);
            *n = transformed.try_normalize().unwrap_or(*n);
        }
    }

    pub fn transformed(&self, matrix: DMat4) -> Self {
        let mut out = self.clone();
        out.transform(matrix);
        out
    }

    /// Total surface area. Cheap sanity check for a sweep, and the basis for the
    /// `IfcQuantityArea` a takeoff needs.
    pub fn surface_area(&self) -> f64 {
        self.triangles()
            .map(|(a, b, c)| (b - a).cross(c - a).length() * 0.5)
            .sum()
    }

    /// Signed volume via the divergence theorem. Meaningful only for a closed mesh, where it
    /// is positive for outward-facing normals — which makes it a useful closure check.
    pub fn signed_volume(&self) -> f64 {
        self.triangles()
            .map(|(a, b, c)| a.dot(b.cross(c)) / 6.0)
            .sum()
    }

    pub fn triangles(&self) -> impl Iterator<Item = (DVec3, DVec3, DVec3)> + '_ {
        self.indices.chunks_exact(3).map(move |t| {
            (
                self.positions[t[0] as usize],
                self.positions[t[1] as usize],
                self.positions[t[2] as usize],
            )
        })
    }

    /// Structural check: parallel arrays agree, indices are in range, index count is a
    /// multiple of three.
    pub fn is_well_formed(&self) -> bool {
        self.positions.len() == self.normals.len()
            && self.indices.len() % 3 == 0
            && self
                .indices
                .iter()
                .all(|&i| (i as usize) < self.positions.len())
    }

    /// Wavefront OBJ, for eyeballing output and for golden-file tests.
    pub fn to_obj(&self, name: &str) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(self.positions.len() * 48);
        let _ = writeln!(s, "# CADForge {name}");
        let _ = writeln!(s, "o {name}");
        for p in &self.positions {
            let _ = writeln!(s, "v {:.6} {:.6} {:.6}", p.x, p.y, p.z);
        }
        for n in &self.normals {
            let _ = writeln!(s, "vn {:.6} {:.6} {:.6}", n.x, n.y, n.z);
        }
        for t in self.indices.chunks_exact(3) {
            // OBJ indices are 1-based.
            let (a, b, c) = (t[0] + 1, t[1] + 1, t[2] + 1);
            let _ = writeln!(s, "f {a}//{a} {b}//{b} {c}//{c}");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit cube centred on the origin, built face by face with outward normals.
    fn unit_cube() -> IndexedMesh {
        let mut mesh = IndexedMesh::with_capacity(12);
        let h = 0.5;
        let corners = [
            DVec3::new(-h, -h, -h),
            DVec3::new(h, -h, -h),
            DVec3::new(h, h, -h),
            DVec3::new(-h, h, -h),
            DVec3::new(-h, -h, h),
            DVec3::new(h, -h, h),
            DVec3::new(h, h, h),
            DVec3::new(-h, h, h),
        ];
        let faces = [
            [0, 3, 2, 1], // bottom, -Z
            [4, 5, 6, 7], // top, +Z
            [0, 1, 5, 4], // -Y
            [1, 2, 6, 5], // +X
            [2, 3, 7, 6], // +Y
            [3, 0, 4, 7], // -X
        ];
        for [a, b, c, d] in faces {
            mesh.push_triangle(corners[a], corners[b], corners[c]);
            mesh.push_triangle(corners[a], corners[c], corners[d]);
        }
        mesh
    }

    #[test]
    fn a_cube_has_the_expected_area_and_volume() {
        let cube = unit_cube();
        assert_eq!(cube.triangle_count(), 12);
        assert!(cube.is_well_formed());
        assert!((cube.surface_area() - 6.0).abs() < 1e-12);
        assert!(
            (cube.signed_volume() - 1.0).abs() < 1e-12,
            "volume was {}, so a face is wound inward",
            cube.signed_volume()
        );
    }

    #[test]
    fn degenerate_triangles_are_dropped() {
        let mut mesh = IndexedMesh::new();
        mesh.push_triangle(DVec3::ZERO, DVec3::X, DVec3::new(2.0, 0.0, 0.0)); // collinear
        mesh.push_triangle(DVec3::ZERO, DVec3::ZERO, DVec3::ZERO); // coincident
        assert!(mesh.is_empty());
        assert!(mesh.normals.iter().all(|n| n.is_finite()));
    }

    #[test]
    fn append_rebases_indices() {
        let mut a = unit_cube();
        let b = unit_cube();
        a.append(&b);
        assert_eq!(a.triangle_count(), 24);
        assert!(a.is_well_formed());
    }

    #[test]
    fn translation_moves_bounds_and_leaves_normals_alone() {
        let cube = unit_cube();
        let moved = cube.transformed(DMat4::from_translation(DVec3::new(10.0, 0.0, 0.0)));
        assert!((moved.bounds().center().x - 10.0).abs() < 1e-12);
        assert_eq!(moved.normals, cube.normals);
        assert!((moved.surface_area() - cube.surface_area()).abs() < 1e-12);
    }

    #[test]
    fn non_uniform_scale_keeps_normals_unit_length() {
        let cube = unit_cube();
        let scaled = cube.transformed(DMat4::from_scale(DVec3::new(4.0, 1.0, 0.25)));
        for n in &scaled.normals {
            assert!(
                (n.length() - 1.0).abs() < 1e-9,
                "normal {n} is not unit length"
            );
        }
        assert!((scaled.signed_volume() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn obj_output_has_one_line_per_element() {
        let obj = unit_cube().to_obj("cube");
        assert_eq!(obj.lines().filter(|l| l.starts_with("v ")).count(), 36);
        assert_eq!(obj.lines().filter(|l| l.starts_with("f ")).count(), 12);
        assert!(obj.contains("o cube"));
    }

    #[test]
    fn an_empty_mesh_is_well_formed() {
        let mesh = IndexedMesh::new();
        assert!(mesh.is_well_formed());
        assert!(mesh.bounds().is_empty());
        assert_eq!(mesh.surface_area(), 0.0);
    }
}
