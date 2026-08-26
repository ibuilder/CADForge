//! A BSP-tree mesh boolean.
//!
//! Fills the [`CsgBackend`](crate::csg::CsgBackend) hole left by ADR-0004. Openings are the
//! motivating case: a door void through a wall is a hole in the middle of a solid, and no
//! amount of profile arithmetic expresses that — it needs a real boolean.
//!
//! The algorithm is the classic binary-space-partition CSG (Naylor, Amanatides & Thibault
//! 1990; the same construction as `csg.js`). Each solid becomes a set of polygons, each set
//! becomes a BSP tree, and the trees clip each other. It is compact, dependency-free,
//! deterministic, and exact for the axis-aligned box-through-box case that openings actually
//! are.
//!
//! # What this is not
//!
//! It is **not** an exact-arithmetic kernel. It works in `f64` with an epsilon, and it
//! inherits the known weaknesses of the approach:
//!
//! - **Coplanar faces** are decided by an epsilon comparison. Two solids sharing a face
//!   exactly — a wall flush against a slab — are the awkward case.
//! - **T-junctions** appear along cut edges. Volumes stay correct; a renderer may show
//!   hairline cracks under some rasterisation rules.
//! - **Open meshes** are undefined input. A boolean is only meaningful between closed solids,
//!   so both operands are checked and rejected if they are not.
//! - **Cost is superlinear.** Intended for element-scale operands — a wall and its openings,
//!   thousands of triangles — not for whole models. Tree construction and clipping recurse,
//!   so a pathological input can go deep.
//!
//! When `ifc-lite`'s exact-arithmetic kernel is measured and adopted, it plugs in behind the
//! same trait and this becomes the fallback (ADR-0008).

use crate::csg::CsgBackend;
use crate::mesh::IndexedMesh;
use crate::GeometryError;
use glam::DVec3;

/// Default coincidence tolerance, in metres.
///
/// 1 µm: far below anything a building model cares about, far above `f64` noise accumulated
/// through a placement transform.
pub const DEFAULT_EPSILON: f64 = 1e-6;

/// A mesh boolean over BSP trees.
#[derive(Debug, Clone, Copy)]
pub struct BspCsg {
    epsilon: f64,
}

impl Default for BspCsg {
    fn default() -> Self {
        Self {
            epsilon: DEFAULT_EPSILON,
        }
    }
}

impl BspCsg {
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the coincidence tolerance.
    pub fn with_epsilon(epsilon: f64) -> Self {
        Self {
            epsilon: epsilon.abs().max(f64::MIN_POSITIVE),
        }
    }

    pub fn epsilon(self) -> f64 {
        self.epsilon
    }

    /// Reject operands a boolean cannot be defined over.
    ///
    /// A boolean between open surfaces has no meaning, and the BSP will happily return
    /// confident nonsense for one. Checking here converts a silent wrong answer into a loud
    /// failure, which ADR-0004 requires.
    fn check_operand(self, mesh: &IndexedMesh, which: &'static str) -> Result<(), GeometryError> {
        if mesh.is_empty() {
            return Err(GeometryError::EmptyOperand(which));
        }
        if !mesh.is_well_formed() {
            return Err(GeometryError::MalformedOperand(which));
        }
        if !is_watertight(mesh) {
            return Err(GeometryError::OpenOperand(which));
        }
        Ok(())
    }

    fn run(self, a: &IndexedMesh, b: &IndexedMesh, op: Op) -> Result<IndexedMesh, GeometryError> {
        self.check_operand(a, "a")?;
        self.check_operand(b, "b")?;

        let mut left = Node::from_polygons(polygons_of(a), self.epsilon);
        let mut right = Node::from_polygons(polygons_of(b), self.epsilon);

        // The three classic clip sequences. Each leaves `left` holding the polygons of the
        // result, with `right`'s contribution grafted in.
        match op {
            Op::Union => {
                left.clip_to(&right);
                right.clip_to(&left);
                right.invert();
                right.clip_to(&left);
                right.invert();
                left.build(right.all_polygons());
            }
            Op::Difference => {
                left.invert();
                left.clip_to(&right);
                right.clip_to(&left);
                right.invert();
                right.clip_to(&left);
                right.invert();
                left.build(right.all_polygons());
                left.invert();
            }
            Op::Intersection => {
                left.invert();
                right.clip_to(&left);
                right.invert();
                left.clip_to(&right);
                right.clip_to(&left);
                left.build(right.all_polygons());
                left.invert();
            }
        }

        Ok(mesh_of(left.all_polygons()))
    }
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Union,
    Difference,
    Intersection,
}

impl CsgBackend for BspCsg {
    fn name(&self) -> &'static str {
        "bsp"
    }

    fn union(&self, a: &IndexedMesh, b: &IndexedMesh) -> Result<IndexedMesh, GeometryError> {
        self.run(a, b, Op::Union)
    }

    fn difference(&self, a: &IndexedMesh, b: &IndexedMesh) -> Result<IndexedMesh, GeometryError> {
        self.run(a, b, Op::Difference)
    }

    fn intersection(&self, a: &IndexedMesh, b: &IndexedMesh) -> Result<IndexedMesh, GeometryError> {
        self.run(a, b, Op::Intersection)
    }
}

// ---------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vertex {
    position: DVec3,
    normal: DVec3,
}

impl Vertex {
    fn lerp(self, other: Vertex, t: f64) -> Self {
        Self {
            position: self.position.lerp(other.position, t),
            normal: self
                .normal
                .lerp(other.normal, t)
                .try_normalize()
                .unwrap_or(self.normal),
        }
    }

    fn flipped(self) -> Self {
        Self {
            position: self.position,
            normal: -self.normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Plane {
    normal: DVec3,
    w: f64,
}

impl Plane {
    fn from_points(a: DVec3, b: DVec3, c: DVec3) -> Option<Self> {
        let normal = (b - a).cross(c - a).try_normalize()?;
        Some(Self {
            normal,
            w: normal.dot(a),
        })
    }

    fn flip(&mut self) {
        self.normal = -self.normal;
        self.w = -self.w;
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Polygon {
    vertices: Vec<Vertex>,
    plane: Plane,
}

impl Polygon {
    fn new(vertices: Vec<Vertex>) -> Option<Self> {
        let plane = Plane::from_points(
            vertices.first()?.position,
            vertices.get(1)?.position,
            vertices.get(2)?.position,
        )?;
        Some(Self { vertices, plane })
    }

    fn flip(&mut self) {
        self.vertices.reverse();
        for v in &mut self.vertices {
            *v = v.flipped();
        }
        self.plane.flip();
    }
}

/// Where a point or polygon sits relative to a plane.
const COPLANAR: u8 = 0;
const FRONT: u8 = 1;
const BACK: u8 = 2;
const SPANNING: u8 = 3;

/// The four buckets a plane sorts polygons into.
///
/// A struct rather than four `&mut Vec` arguments because callers legitimately want the same
/// destination for two buckets — `clip_polygons` treats coplanar-front as front — and Rust
/// will not alias two mutable references to one vector. The reference implementation
/// (`csg.js`) passes the same array twice; here the merge happens at the call site instead.
#[derive(Debug, Default)]
struct Split {
    coplanar_front: Vec<Polygon>,
    coplanar_back: Vec<Polygon>,
    front: Vec<Polygon>,
    back: Vec<Polygon>,
}

/// Sort a polygon into the four buckets, splitting it if it straddles the plane.
fn split_polygon(plane: &Plane, polygon: &Polygon, epsilon: f64, out: &mut Split) {
    let mut polygon_type = 0u8;
    let types: Vec<u8> = polygon
        .vertices
        .iter()
        .map(|v| {
            let distance = plane.normal.dot(v.position) - plane.w;
            let t = if distance < -epsilon {
                BACK
            } else if distance > epsilon {
                FRONT
            } else {
                COPLANAR
            };
            polygon_type |= t;
            t
        })
        .collect();

    match polygon_type {
        COPLANAR => {
            // Facing the same way as the splitting plane, or the opposite way.
            if plane.normal.dot(polygon.plane.normal) > 0.0 {
                out.coplanar_front.push(polygon.clone());
            } else {
                out.coplanar_back.push(polygon.clone());
            }
        }
        FRONT => out.front.push(polygon.clone()),
        BACK => out.back.push(polygon.clone()),
        _ => {
            // SPANNING: walk the loop, emitting the crossing points into both halves.
            let mut front_vertices = Vec::new();
            let mut back_vertices = Vec::new();
            let count = polygon.vertices.len();

            for i in 0..count {
                let j = (i + 1) % count;
                let (ti, tj) = (types[i], types[j]);
                let (vi, vj) = (polygon.vertices[i], polygon.vertices[j]);

                if ti != BACK {
                    front_vertices.push(vi);
                }
                if ti != FRONT {
                    back_vertices.push(vi);
                }
                if (ti | tj) == SPANNING {
                    let denominator = plane.normal.dot(vj.position - vi.position);
                    if denominator.abs() > f64::MIN_POSITIVE {
                        let t = (plane.w - plane.normal.dot(vi.position)) / denominator;
                        let crossing = vi.lerp(vj, t);
                        front_vertices.push(crossing);
                        back_vertices.push(crossing);
                    }
                }
            }

            if front_vertices.len() >= 3 {
                if let Some(p) = Polygon::new(front_vertices) {
                    out.front.push(p);
                }
            }
            if back_vertices.len() >= 3 {
                if let Some(p) = Polygon::new(back_vertices) {
                    out.back.push(p);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Node {
    plane: Option<Plane>,
    front: Option<Box<Node>>,
    back: Option<Box<Node>>,
    polygons: Vec<Polygon>,
    epsilon: f64,
}

impl Node {
    fn from_polygons(polygons: Vec<Polygon>, epsilon: f64) -> Self {
        let mut node = Node {
            epsilon,
            ..Default::default()
        };
        node.build(polygons);
        node
    }

    /// Turn the solid inside out. Union, difference, and intersection are all the same
    /// clipping dance with inversions in different places.
    fn invert(&mut self) {
        for polygon in &mut self.polygons {
            polygon.flip();
        }
        if let Some(plane) = &mut self.plane {
            plane.flip();
        }
        if let Some(front) = &mut self.front {
            front.invert();
        }
        if let Some(back) = &mut self.back {
            back.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }

    /// Drop the parts of `polygons` that fall inside this solid.
    fn clip_polygons(&self, polygons: Vec<Polygon>) -> Vec<Polygon> {
        let Some(plane) = &self.plane else {
            return polygons;
        };

        let mut split = Split::default();
        for polygon in &polygons {
            split_polygon(plane, polygon, self.epsilon, &mut split);
        }
        // Coplanar polygons follow the side their normal agrees with.
        let mut front = split.front;
        front.extend(split.coplanar_front);
        let mut back = split.back;
        back.extend(split.coplanar_back);

        let mut result = match &self.front {
            Some(node) => node.clip_polygons(front),
            None => front,
        };
        // Anything behind a leaf plane is inside the solid, so it is discarded.
        if let Some(node) = &self.back {
            result.extend(node.clip_polygons(back));
        }
        result
    }

    /// Remove everything in this tree that lies inside `other`.
    fn clip_to(&mut self, other: &Node) {
        self.polygons = other.clip_polygons(std::mem::take(&mut self.polygons));
        if let Some(front) = &mut self.front {
            front.clip_to(other);
        }
        if let Some(back) = &mut self.back {
            back.clip_to(other);
        }
    }

    fn all_polygons(&self) -> Vec<Polygon> {
        let mut out = self.polygons.clone();
        if let Some(front) = &self.front {
            out.extend(front.all_polygons());
        }
        if let Some(back) = &self.back {
            out.extend(back.all_polygons());
        }
        out
    }

    /// Insert polygons, choosing the first as the splitting plane.
    ///
    /// Deterministic by construction: no shuffling, no randomised plane choice. The same
    /// input always builds the same tree and therefore the same output mesh.
    fn build(&mut self, polygons: Vec<Polygon>) {
        if polygons.is_empty() {
            return;
        }
        if self.plane.is_none() {
            self.plane = Some(polygons[0].plane);
        }
        let plane = self.plane.expect("just set");

        let mut split = Split::default();
        for polygon in &polygons {
            split_polygon(&plane, polygon, self.epsilon, &mut split);
        }
        // Everything coplanar with the splitting plane belongs to this node.
        self.polygons.extend(split.coplanar_front);
        self.polygons.extend(split.coplanar_back);
        let (front, back) = (split.front, split.back);

        if !front.is_empty() {
            self.front
                .get_or_insert_with(|| {
                    Box::new(Node {
                        epsilon: self.epsilon,
                        ..Default::default()
                    })
                })
                .build(front);
        }
        if !back.is_empty() {
            self.back
                .get_or_insert_with(|| {
                    Box::new(Node {
                        epsilon: self.epsilon,
                        ..Default::default()
                    })
                })
                .build(back);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------------------

fn polygons_of(mesh: &IndexedMesh) -> Vec<Polygon> {
    mesh.indices
        .chunks_exact(3)
        .filter_map(|t| {
            let vertices: Vec<Vertex> = t
                .iter()
                .map(|i| Vertex {
                    position: mesh.positions[*i as usize],
                    normal: mesh.normals[*i as usize],
                })
                .collect();
            Polygon::new(vertices)
        })
        .collect()
}

fn mesh_of(polygons: Vec<Polygon>) -> IndexedMesh {
    let mut mesh = IndexedMesh::with_capacity(polygons.len());
    for polygon in polygons {
        // Split polygons are convex, so a fan is a valid triangulation.
        for i in 1..polygon.vertices.len().saturating_sub(1) {
            mesh.push_triangle(
                polygon.vertices[0].position,
                polygon.vertices[i].position,
                polygon.vertices[i + 1].position,
            );
        }
    }
    mesh
}

/// Whether a mesh encloses a volume.
///
/// Uses the vector area: for any closed surface `∮ n dA = 0` exactly, because opposing faces
/// cancel. An open surface leaves a residual equal to the vector area of its boundary.
///
/// This — not edge-manifoldness — is the right precondition for a boolean, and the difference
/// matters. BSP cutting subdivides a face without subdividing its neighbour, leaving
/// **T-junctions**: the result encloses exactly the right volume but has edges met by one
/// triangle on one side and two on the other. Requiring edge-manifoldness would reject the
/// output of the very operation that produced it, so `difference_many` could never apply a
/// second opening.
pub fn is_watertight(mesh: &IndexedMesh) -> bool {
    let mut vector_area = DVec3::ZERO;
    let mut total_area = 0.0;
    for (a, b, c) in mesh.triangles() {
        let cross = (b - a).cross(c - a);
        vector_area += cross;
        total_area += cross.length();
    }
    if total_area <= 0.0 {
        return false;
    }
    // Relative, so it holds for a 5 cm fixing and a 50 m slab alike.
    vector_area.length() / total_area < 1e-6
}

/// Whether every edge is shared by exactly two triangles in opposite directions.
///
/// Stricter than [`is_watertight`], and deliberately **not** the operand precondition: BSP
/// output is watertight but not edge-manifold. Kept because it is the right check for
/// *authored* geometry coming out of a sweep, where a failure means a real bug.
///
/// Positions are quantised so that vertices duplicated per-face still match.
pub fn is_edge_manifold(mesh: &IndexedMesh, epsilon: f64) -> bool {
    use std::collections::HashMap;

    let quantise = |p: DVec3| -> [i64; 3] {
        let scale = 1.0 / epsilon.max(f64::MIN_POSITIVE);
        [
            (p.x * scale).round() as i64,
            (p.y * scale).round() as i64,
            (p.z * scale).round() as i64,
        ]
    };

    let mut edges: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
    for (a, b, c) in mesh.triangles() {
        for (from, to) in [(a, b), (b, c), (c, a)] {
            let (from, to) = (quantise(from), quantise(to));
            if from == to {
                return false; // a degenerate edge cannot be matched
            }
            // Count each undirected edge, signed by direction. A closed, consistently wound
            // shell nets to zero on every edge.
            let (key, delta) = if from < to {
                ((from, to), 1)
            } else {
                ((to, from), -1)
            };
            *edges.entry(key).or_insert(0) += delta;
        }
    }
    edges.values().all(|count| *count == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{extrude, Profile};

    /// An axis-aligned box, built the way CADForge builds every solid: a swept profile.
    fn box_at(min: DVec3, size: DVec3) -> IndexedMesh {
        let profile = Profile::rectangle(size.x, size.y).unwrap();
        let mut mesh = extrude(&profile, size.z).unwrap();
        mesh.transform(glam::DMat4::from_translation(DVec3::new(
            min.x + size.x * 0.5,
            min.y + size.y * 0.5,
            min.z,
        )));
        mesh
    }

    fn volume(mesh: &IndexedMesh) -> f64 {
        mesh.signed_volume()
    }

    #[test]
    fn the_test_boxes_are_closed_and_correctly_sized() {
        let unit = box_at(DVec3::ZERO, DVec3::ONE);
        assert!(is_watertight(&unit));
        // Authored sweep output is edge-manifold too, which BSP output will not be.
        assert!(is_edge_manifold(&unit, DEFAULT_EPSILON));
        assert!((volume(&unit) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn bsp_output_is_watertight_but_not_edge_manifold() {
        // Worth pinning down rather than leaving as folklore. Cutting subdivides a face
        // without subdividing its neighbour, so the result has T-junctions: it encloses the
        // right volume, but edges are met by one triangle on one side and two on the other.
        // This is why the operand precondition is is_watertight and not is_edge_manifold.
        let wall = box_at(DVec3::new(0.0, -0.1, 0.0), DVec3::new(4.0, 0.2, 3.0));
        let void = box_at(DVec3::new(1.5, -0.15, 0.0), DVec3::new(0.92, 0.3, 2.12));
        let cut = BspCsg::new().difference(&wall, &void).unwrap();

        assert!(
            is_watertight(&cut),
            "the cut solid still encloses its volume"
        );
        assert!(
            !is_edge_manifold(&cut, DEFAULT_EPSILON),
            "if this ever passes, the backend gained T-junction repair and the docs are stale"
        );
    }

    #[test]
    fn a_door_opening_removes_exactly_its_own_volume_from_a_wall() {
        // The case that motivated the whole backend, and the same arithmetic IfcOpenShell
        // performs on the exported file: a 0.92 wide x 2.12 high void through a 0.2 thick
        // wall removes 0.92 * 0.20 * 2.12 = 0.39008 m3.
        let wall = box_at(DVec3::new(0.0, -0.1, 0.0), DVec3::new(4.0, 0.2, 3.0));
        assert!((volume(&wall) - 2.4).abs() < 1e-9);

        // The void is deeper than the wall on purpose, so it cuts cleanly through.
        let void = box_at(DVec3::new(1.5, -0.15, 0.0), DVec3::new(0.92, 0.3, 2.12));

        let cut = BspCsg::new().difference(&wall, &void).unwrap();
        let expected = 2.4 - 0.92 * 0.2 * 2.12;
        assert!(
            (volume(&cut) - expected).abs() < 1e-6,
            "expected {expected}, got {}",
            volume(&cut)
        );
        assert!(is_watertight(&cut), "the cut wall must stay watertight");
    }

    #[test]
    fn subtracting_a_disjoint_solid_changes_nothing() {
        let a = box_at(DVec3::ZERO, DVec3::ONE);
        let far = box_at(DVec3::splat(10.0), DVec3::ONE);
        let result = BspCsg::new().difference(&a, &far).unwrap();
        assert!((volume(&result) - volume(&a)).abs() < 1e-9);
    }

    #[test]
    fn subtracting_a_solid_from_itself_leaves_nothing() {
        let a = box_at(DVec3::ZERO, DVec3::ONE);
        let result = BspCsg::new().difference(&a, &a).unwrap();
        assert!(
            volume(&result).abs() < 1e-6,
            "expected an empty result, got volume {}",
            volume(&result)
        );
    }

    #[test]
    fn subtracting_an_enclosing_solid_leaves_nothing() {
        let small = box_at(DVec3::splat(0.25), DVec3::splat(0.5));
        let large = box_at(DVec3::ZERO, DVec3::ONE);
        let result = BspCsg::new().difference(&small, &large).unwrap();
        assert!(volume(&result).abs() < 1e-6, "got {}", volume(&result));
    }

    #[test]
    fn union_of_disjoint_solids_sums_their_volumes() {
        let a = box_at(DVec3::ZERO, DVec3::ONE);
        let b = box_at(DVec3::new(5.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 1.0));
        let result = BspCsg::new().union(&a, &b).unwrap();
        assert!(
            (volume(&result) - 3.0).abs() < 1e-6,
            "got {}",
            volume(&result)
        );
    }

    #[test]
    fn union_of_overlapping_solids_counts_the_overlap_once() {
        // Inclusion-exclusion: 8 + 8 - 1 = 15.
        let a = box_at(DVec3::ZERO, DVec3::splat(2.0));
        let b = box_at(DVec3::new(1.0, 1.0, 1.0), DVec3::splat(2.0));
        let result = BspCsg::new().union(&a, &b).unwrap();
        assert!(
            (volume(&result) - 15.0).abs() < 1e-6,
            "got {}",
            volume(&result)
        );
    }

    #[test]
    fn intersection_is_the_overlap() {
        let a = box_at(DVec3::ZERO, DVec3::splat(2.0));
        let b = box_at(DVec3::new(1.0, 1.0, 1.0), DVec3::splat(2.0));
        let result = BspCsg::new().intersection(&a, &b).unwrap();
        assert!(
            (volume(&result) - 1.0).abs() < 1e-6,
            "got {}",
            volume(&result)
        );
    }

    #[test]
    fn intersection_of_disjoint_solids_is_empty() {
        let a = box_at(DVec3::ZERO, DVec3::ONE);
        let b = box_at(DVec3::splat(10.0), DVec3::ONE);
        let result = BspCsg::new().intersection(&a, &b).unwrap();
        assert!(volume(&result).abs() < 1e-9);
    }

    #[test]
    fn a_wall_takes_two_openings_at_once() {
        let wall = box_at(DVec3::new(0.0, -0.1, 0.0), DVec3::new(8.0, 0.2, 3.0));
        let door = box_at(DVec3::new(1.0, -0.15, 0.0), DVec3::new(0.9, 0.3, 2.1));
        let window = box_at(DVec3::new(5.0, -0.15, 1.0), DVec3::new(1.2, 0.3, 1.4));

        let cut = BspCsg::new()
            .difference_many(&wall, &[door, window])
            .unwrap();

        let expected = 8.0 * 0.2 * 3.0 - 0.9 * 0.2 * 2.1 - 1.2 * 0.2 * 1.4;
        assert!(
            (volume(&cut) - expected).abs() < 1e-6,
            "expected {expected}, got {}",
            volume(&cut)
        );
        assert!(is_watertight(&cut));
    }

    #[test]
    fn the_result_is_deterministic() {
        // Same inputs, same bytes — which is what lets a cut mesh be cached by hash.
        let wall = box_at(DVec3::new(0.0, -0.1, 0.0), DVec3::new(4.0, 0.2, 3.0));
        let void = box_at(DVec3::new(1.5, -0.15, 0.0), DVec3::new(0.92, 0.3, 2.12));
        let backend = BspCsg::new();
        assert_eq!(
            backend.difference(&wall, &void).unwrap(),
            backend.difference(&wall, &void).unwrap()
        );
    }

    #[test]
    fn an_open_operand_is_refused_rather_than_guessed_at() {
        // A boolean between open surfaces is meaningless. Returning confident nonsense here
        // is exactly the silent corruption ADR-0004 exists to prevent.
        let solid = box_at(DVec3::ZERO, DVec3::ONE);
        let mut open = IndexedMesh::new();
        open.push_triangle(DVec3::ZERO, DVec3::X, DVec3::Y);
        assert!(!is_watertight(&open));

        let backend = BspCsg::new();
        assert_eq!(
            backend.difference(&solid, &open),
            Err(GeometryError::OpenOperand("b"))
        );
        assert_eq!(
            backend.difference(&open, &solid),
            Err(GeometryError::OpenOperand("a"))
        );
    }

    #[test]
    fn an_empty_operand_is_refused() {
        let solid = box_at(DVec3::ZERO, DVec3::ONE);
        assert_eq!(
            BspCsg::new().union(&solid, &IndexedMesh::new()),
            Err(GeometryError::EmptyOperand("b"))
        );
    }

    #[test]
    fn a_cut_solid_can_be_cut_again() {
        // The result of a boolean must be a valid operand for the next one, or openings
        // could only ever be applied one at a time.
        let wall = box_at(DVec3::new(0.0, -0.1, 0.0), DVec3::new(8.0, 0.2, 3.0));
        let first = box_at(DVec3::new(1.0, -0.15, 0.0), DVec3::new(0.9, 0.3, 2.1));
        let second = box_at(DVec3::new(5.0, -0.15, 1.0), DVec3::new(1.2, 0.3, 1.4));

        let backend = BspCsg::new();
        let once = backend.difference(&wall, &first).unwrap();
        assert!(is_watertight(&once));
        let twice = backend.difference(&once, &second).unwrap();

        let expected = 8.0 * 0.2 * 3.0 - 0.9 * 0.2 * 2.1 - 1.2 * 0.2 * 1.4;
        assert!(
            (volume(&twice) - expected).abs() < 1e-6,
            "got {}",
            volume(&twice)
        );
    }

    #[test]
    fn a_notch_cut_from_an_edge_keeps_the_shell_closed() {
        // Partial overlap rather than a clean through-cut: the void pokes out of one face.
        let block = box_at(DVec3::ZERO, DVec3::splat(2.0));
        let notch = box_at(DVec3::new(1.5, 1.5, 0.5), DVec3::new(1.0, 1.0, 1.0));
        let result = BspCsg::new().difference(&block, &notch).unwrap();

        // The removed part is only the half that was actually inside: 0.5 x 0.5 x 1.0.
        let expected = 8.0 - 0.5 * 0.5 * 1.0;
        assert!(
            (volume(&result) - expected).abs() < 1e-6,
            "expected {expected}, got {}",
            volume(&result)
        );
        assert!(is_watertight(&result));
    }

    #[test]
    fn the_epsilon_is_configurable_and_never_zero() {
        assert_eq!(BspCsg::new().epsilon(), DEFAULT_EPSILON);
        assert_eq!(BspCsg::with_epsilon(1e-3).epsilon(), 1e-3);
        assert!(BspCsg::with_epsilon(0.0).epsilon() > 0.0);
        assert!(BspCsg::with_epsilon(-1e-4).epsilon() > 0.0);
    }
}
