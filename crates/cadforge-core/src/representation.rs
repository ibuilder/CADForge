//! Element geometry, in the form IFC stores it.
//!
//! This is the bridge between "the recipe is canonical" (ADR-0004) and "IFC is the exchange
//! authority" (`docs/ifc-semantics.md` ADR-001). A `GeometryRecipe` is how a family *authors*
//! geometry; a `Representation` is what that evaluates to and what gets written to a file.
//!
//! Deliberately pure data with no dependency on `cadforge-geom` or `cadforge-family`: core
//! must stay standalone (ADR-0002), and the exporter must not have to understand recipes to
//! write a file.
//!
//! The two variants are not equivalent, and the difference is the whole point of ADR-0004:
//!
//! - [`Representation::ExtrudedAreaSolid`] exports as `IfcExtrudedAreaSolid` over
//!   `IfcArbitraryClosedProfileDef`. It stays **parametric and editable** in Revit, Archicad,
//!   and Bonsai after a round trip.
//! - [`Representation::TriangulatedFaceSet`] exports as `IfcTriangulatedFaceSet`. It is
//!   geometrically correct and semantically poorer — a receiving application can display it
//!   but not edit it.
//!
//! An element that degrades from the first to the second has lost something real, so the
//! degradation is visible in the type rather than buried in a log.

use serde::{Deserialize, Serialize};

/// The geometry of one element, in its own local coordinate system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Representation {
    /// A closed profile swept along a direction.
    ///
    /// Covers walls, slabs, columns, beams, openings, ducts, and pipes — the large majority
    /// of real building elements.
    ExtrudedAreaSolid {
        /// The profile in the local XY plane: counter-clockwise, with no repeated closing
        /// point. Invariants come from `cadforge_geom::Profile`.
        profile: Vec<[f64; 2]>,
        /// Sweep direction in local space.
        direction: [f64; 3],
        /// Sweep distance. Always positive — a negative depth is normalised into the
        /// direction when the representation is built, because IFC requires
        /// `IfcExtrudedAreaSolid.Depth` to be positive.
        depth: f64,
    },
    /// Explicit triangles, for geometry no recipe can express.
    TriangulatedFaceSet {
        vertices: Vec<[f64; 3]>,
        faces: Vec<[u32; 3]>,
    },
}

impl Representation {
    /// Build a swept solid, normalising a negative depth into the direction.
    pub fn extrusion(profile: Vec<[f64; 2]>, direction: [f64; 3], depth: f64) -> Self {
        if depth < 0.0 {
            Self::ExtrudedAreaSolid {
                profile,
                direction: [-direction[0], -direction[1], -direction[2]],
                depth: -depth,
            }
        } else {
            Self::ExtrudedAreaSolid {
                profile,
                direction,
                depth,
            }
        }
    }

    /// Whether this survives export as editable parametric geometry.
    pub fn is_native_parametric(&self) -> bool {
        matches!(self, Self::ExtrudedAreaSolid { .. })
    }

    /// The `IfcShapeRepresentation.RepresentationType` this maps to.
    pub fn ifc_representation_type(&self) -> &'static str {
        match self {
            Self::ExtrudedAreaSolid { .. } => "SweptSolid",
            Self::TriangulatedFaceSet { .. } => "Tessellation",
        }
    }

    /// The IFC entity name of the representation item.
    pub fn ifc_item(&self) -> &'static str {
        match self {
            Self::ExtrudedAreaSolid { .. } => "IfcExtrudedAreaSolid",
            Self::TriangulatedFaceSet { .. } => "IfcTriangulatedFaceSet",
        }
    }

    /// Structural check before export. A malformed representation must never reach a file.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::ExtrudedAreaSolid {
                profile,
                direction,
                depth,
            } => {
                profile.len() >= 3
                    && profile.iter().flatten().all(|v| v.is_finite())
                    && direction.iter().all(|v| v.is_finite())
                    && direction.iter().any(|v| *v != 0.0)
                    && depth.is_finite()
                    && *depth > 0.0
            }
            Self::TriangulatedFaceSet { vertices, faces } => {
                !vertices.is_empty()
                    && !faces.is_empty()
                    && vertices.iter().flatten().all(|v| v.is_finite())
                    && faces
                        .iter()
                        .flatten()
                        .all(|i| (*i as usize) < vertices.len())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Vec<[f64; 2]> {
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
    }

    #[test]
    fn a_negative_depth_is_folded_into_the_direction() {
        // IFC requires a positive Depth, so the flip has to happen somewhere. Doing it here
        // means the exporter never has to think about it.
        let r = Representation::extrusion(square(), [0.0, 0.0, 1.0], -3.0);
        let Representation::ExtrudedAreaSolid {
            direction, depth, ..
        } = &r
        else {
            panic!("expected a swept solid");
        };
        assert_eq!(*direction, [0.0, 0.0, -1.0]);
        assert_eq!(*depth, 3.0);
        assert!(r.is_valid());
    }

    #[test]
    fn a_positive_depth_is_left_alone() {
        let r = Representation::extrusion(square(), [0.0, 0.0, 1.0], 3.0);
        assert_eq!(
            r,
            Representation::ExtrudedAreaSolid {
                profile: square(),
                direction: [0.0, 0.0, 1.0],
                depth: 3.0,
            }
        );
    }

    #[test]
    fn parametric_and_tessellated_map_to_different_ifc() {
        let swept = Representation::extrusion(square(), [0.0, 0.0, 1.0], 1.0);
        assert!(swept.is_native_parametric());
        assert_eq!(swept.ifc_representation_type(), "SweptSolid");
        assert_eq!(swept.ifc_item(), "IfcExtrudedAreaSolid");

        let mesh = Representation::TriangulatedFaceSet {
            vertices: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            faces: vec![[0, 1, 2]],
        };
        assert!(!mesh.is_native_parametric());
        assert_eq!(mesh.ifc_representation_type(), "Tessellation");
        assert!(mesh.is_valid());
    }

    #[test]
    fn malformed_geometry_is_caught_before_it_reaches_a_file() {
        assert!(
            !Representation::extrusion(vec![[0.0, 0.0], [1.0, 0.0]], [0.0, 0.0, 1.0], 1.0)
                .is_valid()
        );
        assert!(!Representation::extrusion(square(), [0.0, 0.0, 0.0], 1.0).is_valid());
        assert!(!Representation::extrusion(square(), [0.0, 0.0, 1.0], 0.0).is_valid());
        assert!(!Representation::extrusion(square(), [0.0, 0.0, 1.0], f64::NAN).is_valid());
        assert!(!Representation::ExtrudedAreaSolid {
            profile: vec![[0.0, 0.0], [1.0, 0.0], [f64::INFINITY, 1.0]],
            direction: [0.0, 0.0, 1.0],
            depth: 1.0,
        }
        .is_valid());
    }

    #[test]
    fn an_out_of_range_face_index_is_invalid() {
        // The failure that writes a file no other application can open.
        assert!(!Representation::TriangulatedFaceSet {
            vertices: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            faces: vec![[0, 1, 7]],
        }
        .is_valid());
        assert!(!Representation::TriangulatedFaceSet {
            vertices: Vec::new(),
            faces: vec![[0, 1, 2]],
        }
        .is_valid());
    }
}
