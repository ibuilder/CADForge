//! Elements and their placement.

use crate::id::GlobalId;
use crate::property::PropertySets;
use crate::representation::Representation;
use glam::{DMat4, DVec3};
use serde::{Deserialize, Serialize};

/// The IFC class of an element.
///
/// Only classes CADForge authors natively are named; everything else round-trips through
/// [`IfcClass::Other`] so that importing an unfamiliar model never loses its typing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IfcClass {
    // Spatial structure
    Project,
    Site,
    Building,
    BuildingStorey,
    Space,
    // Physical elements CADForge authors natively (ADR-0004: all profile sweeps)
    Wall,
    Slab,
    Roof,
    Column,
    Beam,
    Door,
    Window,
    Stair,
    Covering,
    Furniture,
    OpeningElement,
    BuildingElementProxy,
    /// Anything imported that CADForge does not author natively. Preserved verbatim.
    Other(String),
}

impl IfcClass {
    /// The IFC entity name, for export and for display.
    pub fn ifc_name(&self) -> &str {
        match self {
            Self::Project => "IfcProject",
            Self::Site => "IfcSite",
            Self::Building => "IfcBuilding",
            Self::BuildingStorey => "IfcBuildingStorey",
            Self::Space => "IfcSpace",
            Self::Wall => "IfcWall",
            Self::Slab => "IfcSlab",
            Self::Roof => "IfcRoof",
            Self::Column => "IfcColumn",
            Self::Beam => "IfcBeam",
            Self::Door => "IfcDoor",
            Self::Window => "IfcWindow",
            Self::Stair => "IfcStair",
            Self::Covering => "IfcCovering",
            Self::Furniture => "IfcFurniture",
            Self::OpeningElement => "IfcOpeningElement",
            Self::BuildingElementProxy => "IfcBuildingElementProxy",
            Self::Other(name) => name,
        }
    }

    /// True for the spatial structure hierarchy, which contains elements rather than being
    /// contained by them.
    ///
    /// `Other` participates: IFC4X3 introduced a whole spatial hierarchy for infrastructure —
    /// `IfcRoadPart`, `IfcBridgePart`, `IfcFacilityPart` — and CADForge does not author any of
    /// it, so those arrive as `Other`. Treating them as non-spatial strands every element
    /// contained in one. Found by importing the corpus: a single road model produced 26 of
    /// them and 38 dangling-reference warnings.
    pub fn is_spatial(&self) -> bool {
        match self {
            Self::Project | Self::Site | Self::Building | Self::BuildingStorey | Self::Space => {
                true
            }
            Self::Other(name) => is_spatial_entity(name),
            _ => false,
        }
    }

    /// True for the four classes that form the containment spine and are written by the
    /// exporter as structure rather than as products.
    ///
    /// Distinct from [`IfcClass::is_spatial`]: a space and a road part are spatial — things
    /// can be contained in them — but they are written as products, because the exporter
    /// composes exactly one project/site/building/storey chain.
    pub fn is_structure_spine(&self) -> bool {
        matches!(
            self,
            Self::Project | Self::Site | Self::Building | Self::BuildingStorey
        )
    }

    /// True if an element of this class may host openings (`IfcRelVoidsElement`).
    pub fn can_host_openings(&self) -> bool {
        matches!(
            self,
            Self::Wall | Self::Slab | Self::Roof | Self::Column | Self::Beam
        )
    }
}

/// Spatial entities CADForge does not model natively but must still accept as containers.
///
/// Mostly the IFC4X3 infrastructure hierarchy. Not exhaustive, and deliberately a list rather
/// than a prefix rule — `IfcSpatialZone` is spatial and `IfcSpaceHeater` is not.
fn is_spatial_entity(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "IFCSPATIALELEMENT"
            | "IFCSPATIALSTRUCTUREELEMENT"
            | "IFCSPATIALZONE"
            | "IFCEXTERNALSPATIALELEMENT"
            | "IFCEXTERNALSPATIALSTRUCTUREELEMENT"
            | "IFCFACILITY"
            | "IFCFACILITYPART"
            | "IFCFACILITYPARTCOMMON"
            | "IFCBRIDGE"
            | "IFCBRIDGEPART"
            | "IFCROAD"
            | "IFCROADPART"
            | "IFCRAILWAY"
            | "IFCRAILWAYPART"
            | "IFCMARINEFACILITY"
            | "IFCMARINEPART"
            | "IFCTUNNEL"
            | "IFCTUNNELPART"
    )
}

/// An object placement, modelled as IFC models it.
///
/// Stored as `IfcAxis2Placement3D` does — a location plus a Z axis and a reference X axis —
/// rather than as a raw 4×4. Keeping the IFC shape avoids a lossy decomposition on every
/// export, and it makes a non-orthogonal or mirrored transform impossible to represent by
/// accident.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub location: DVec3,
    /// Local Z. Normalised on construction.
    pub axis: DVec3,
    /// Local X. Orthogonalised against `axis` on construction.
    pub ref_direction: DVec3,
}

impl Placement {
    /// Identity placement at the origin.
    pub fn identity() -> Self {
        Self {
            location: DVec3::ZERO,
            axis: DVec3::Z,
            ref_direction: DVec3::X,
        }
    }

    pub fn at(location: DVec3) -> Self {
        Self {
            location,
            ..Self::identity()
        }
    }

    /// Build a placement, normalising `axis` and orthogonalising `ref_direction` against it.
    ///
    /// Degenerate input (zero-length or parallel axes) falls back to the identity basis rather
    /// than producing a silently broken transform.
    pub fn new(location: DVec3, axis: DVec3, ref_direction: DVec3) -> Self {
        let z = axis.try_normalize().unwrap_or(DVec3::Z);
        let x = (ref_direction - z * z.dot(ref_direction))
            .try_normalize()
            .unwrap_or_else(|| {
                // Any direction orthogonal to z will do; pick the more stable of two.
                let candidate = if z.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
                (candidate - z * z.dot(candidate))
                    .try_normalize()
                    .unwrap_or(DVec3::X)
            });
        Self {
            location,
            axis: z,
            ref_direction: x,
        }
    }

    /// The local-to-parent transform.
    pub fn to_matrix(self) -> DMat4 {
        let z = self.axis;
        let x = self.ref_direction;
        let y = z.cross(x);
        DMat4::from_cols(
            x.extend(0.0),
            y.extend(0.0),
            z.extend(0.0),
            self.location.extend(1.0),
        )
    }

    /// Recover a placement from a rigid transform.
    ///
    /// Import needs this: IFC nests placements, so an element's transform is the composition
    /// of its whole `IfcLocalPlacement` chain, and getting back to a storey-relative placement
    /// means composing, inverting, and decomposing.
    ///
    /// Only rotation and translation survive. Scale and shear are discarded by
    /// re-orthogonalising, because `Placement` cannot represent them and silently keeping a
    /// scaled basis would corrupt every downstream length.
    pub fn from_matrix(matrix: DMat4) -> Self {
        Self::new(
            matrix.w_axis.truncate(),
            matrix.z_axis.truncate(),
            matrix.x_axis.truncate(),
        )
    }

    /// Translate without touching orientation.
    pub fn translated(self, delta: DVec3) -> Self {
        Self {
            location: self.location + delta,
            ..self
        }
    }
}

impl Default for Placement {
    fn default() -> Self {
        Self::identity()
    }
}

/// An axis-aligned bounding box in world space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub min: DVec3,
    pub max: DVec3,
}

impl BoundingBox {
    pub fn new(min: DVec3, max: DVec3) -> Self {
        Self {
            min: min.min(max),
            max: min.max(max),
        }
    }

    /// The box containing no points. Union with anything returns that thing.
    pub fn empty() -> Self {
        Self {
            min: DVec3::splat(f64::INFINITY),
            max: DVec3::splat(f64::NEG_INFINITY),
        }
    }

    pub fn from_points(points: impl IntoIterator<Item = DVec3>) -> Self {
        points.into_iter().fold(Self::empty(), |b, p| b.extended(p))
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn extended(self, p: DVec3) -> Self {
        Self {
            min: self.min.min(p),
            max: self.max.max(p),
        }
    }

    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    pub fn intersects(&self, other: &Self) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
            && self.min.z <= other.max.z
            && self.max.z >= other.min.z
    }

    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> DVec3 {
        (self.max - self.min).max(DVec3::ZERO)
    }
}

/// One element in the model.
///
/// The two revision counters are what make incremental work possible: a rename bumps only
/// `semantic_revision`, so the renderer never rebuilds a mesh for it
/// (`docs/ifc-semantics.md` §5, "incremental invalidation").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementRecord {
    pub global_id: GlobalId,
    pub class: IfcClass,
    pub name: Option<String>,
    pub object_type: Option<String>,
    pub placement: Placement,
    /// `IfcRelContainedInSpatialStructure` — the storey or space holding this element.
    pub container: Option<GlobalId>,
    /// `IfcRelDefinesByType` — the family type this instance is defined by.
    pub type_ref: Option<GlobalId>,
    pub properties: PropertySets,
    /// Geometry in local space, evaluated from the family recipe. `None` for spatial
    /// structure elements and for anything not yet flexed.
    pub representation: Option<Representation>,
    /// World-space bounds, once geometry has been evaluated.
    pub bounds: Option<BoundingBox>,
    /// Bumped when geometry must be rebuilt.
    pub representation_revision: u64,
    /// Bumped when metadata changes but geometry does not.
    pub semantic_revision: u64,
}

impl ElementRecord {
    pub fn new(global_id: GlobalId, class: IfcClass) -> Self {
        Self {
            global_id,
            class,
            name: None,
            object_type: None,
            placement: Placement::identity(),
            container: None,
            type_ref: None,
            properties: PropertySets::default(),
            representation: None,
            bounds: None,
            representation_revision: 0,
            semantic_revision: 0,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    pub fn with_container(mut self, container: GlobalId) -> Self {
        self.container = Some(container);
        self
    }

    pub fn with_representation(mut self, representation: Representation) -> Self {
        self.representation = Some(representation);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_orthogonalises_a_skewed_basis() {
        let p = Placement::new(DVec3::ZERO, DVec3::Z * 3.0, DVec3::new(1.0, 0.0, 0.7));
        assert!((p.axis.length() - 1.0).abs() < 1e-12);
        assert!((p.ref_direction.length() - 1.0).abs() < 1e-12);
        assert!(
            p.axis.dot(p.ref_direction).abs() < 1e-12,
            "axes must be orthogonal"
        );
    }

    #[test]
    fn placement_survives_degenerate_input() {
        // Zero axis and a ref_direction parallel to it: both degenerate.
        let p = Placement::new(DVec3::ONE, DVec3::ZERO, DVec3::ZERO);
        assert!(p.axis.is_finite() && p.ref_direction.is_finite());
        assert!(p.axis.dot(p.ref_direction).abs() < 1e-12);
    }

    #[test]
    fn placement_matrix_is_right_handed() {
        let p = Placement::new(DVec3::new(1.0, 2.0, 3.0), DVec3::Z, DVec3::X);
        let m = p.to_matrix();
        assert!((m.determinant() - 1.0).abs() < 1e-12);
        assert_eq!(m.transform_point3(DVec3::ZERO), DVec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn a_placement_survives_a_matrix_round_trip() {
        let original = Placement::new(
            DVec3::new(3.0, -7.5, 2.25),
            DVec3::new(0.0, 0.3, 1.0),
            DVec3::new(1.0, 0.4, 0.0),
        );
        let recovered = Placement::from_matrix(original.to_matrix());

        assert!((recovered.location - original.location).length() < 1e-12);
        assert!((recovered.axis - original.axis).length() < 1e-12);
        assert!((recovered.ref_direction - original.ref_direction).length() < 1e-12);
    }

    #[test]
    fn composing_and_decomposing_recovers_a_relative_placement() {
        // Exactly what import does: an element's world transform is its whole IfcLocalPlacement
        // chain composed, and the storey-relative placement has to be recovered from it.
        let storey = Placement::new(DVec3::new(0.0, 0.0, 3.5), DVec3::Z, DVec3::X);
        let relative = Placement::new(DVec3::new(2.0, 1.0, 0.0), DVec3::Z, DVec3::Y);

        let world = storey.to_matrix() * relative.to_matrix();
        let recovered = Placement::from_matrix(storey.to_matrix().inverse() * world);

        assert!((recovered.location - relative.location).length() < 1e-12);
        assert!((recovered.ref_direction - relative.ref_direction).length() < 1e-12);
    }

    #[test]
    fn scale_is_discarded_rather_than_silently_kept() {
        // A scaled basis would corrupt every length downstream. Dropping it is wrong too, but
        // it is wrong loudly and consistently rather than subtly.
        let scaled = DMat4::from_scale(DVec3::new(3.0, 3.0, 3.0));
        let recovered = Placement::from_matrix(scaled);
        assert!((recovered.axis.length() - 1.0).abs() < 1e-12);
        assert!((recovered.ref_direction.length() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn empty_box_is_the_union_identity() {
        let b = BoundingBox::new(DVec3::ZERO, DVec3::ONE);
        assert!(BoundingBox::empty().is_empty());
        assert_eq!(BoundingBox::empty().union(b), b);
        assert_eq!(b.union(BoundingBox::empty()), b);
    }

    #[test]
    fn boxes_touching_at_a_face_intersect() {
        let a = BoundingBox::new(DVec3::ZERO, DVec3::ONE);
        let b = BoundingBox::new(DVec3::X, DVec3::new(2.0, 1.0, 1.0));
        let far = BoundingBox::new(DVec3::splat(5.0), DVec3::splat(6.0));
        assert!(a.intersects(&b));
        assert!(!a.intersects(&far));
        assert!(!a.intersects(&BoundingBox::empty()));
    }

    #[test]
    fn new_normalises_inverted_corners() {
        let b = BoundingBox::new(DVec3::ONE, DVec3::ZERO);
        assert_eq!(b.min, DVec3::ZERO);
        assert_eq!(b.size(), DVec3::ONE);
    }

    #[test]
    fn infrastructure_spatial_parts_count_as_containers() {
        // IFC4X3 spatial structure arrives as Other, because CADForge does not author roads.
        // Refusing it as a container strands every element inside one.
        assert!(IfcClass::Other("IfcRoadPart".into()).is_spatial());
        assert!(IfcClass::Other("IFCBRIDGEPART".into()).is_spatial());
        assert!(IfcClass::Other("IfcFacilityPart".into()).is_spatial());
        assert!(IfcClass::Space.is_spatial());

        // A prefix rule would get this wrong: a space heater is equipment, not a space.
        assert!(!IfcClass::Other("IfcSpaceHeater".into()).is_spatial());
        assert!(!IfcClass::Other("IfcPipeSegment".into()).is_spatial());
        assert!(!IfcClass::Wall.is_spatial());
    }

    #[test]
    fn the_structure_spine_is_narrower_than_spatial() {
        // A space can contain things but is written as a product; only these four form the
        // chain the exporter composes.
        for class in [
            IfcClass::Project,
            IfcClass::Site,
            IfcClass::Building,
            IfcClass::BuildingStorey,
        ] {
            assert!(class.is_structure_spine() && class.is_spatial());
        }
        assert!(IfcClass::Space.is_spatial());
        assert!(!IfcClass::Space.is_structure_spine());
        assert!(!IfcClass::Other("IfcRoadPart".into()).is_structure_spine());
    }

    #[test]
    fn unknown_classes_keep_their_name() {
        let c = IfcClass::Other("IfcDistributionElement".into());
        assert_eq!(c.ifc_name(), "IfcDistributionElement");
        assert!(!c.can_host_openings());
        assert!(IfcClass::Wall.can_host_openings());
        assert!(IfcClass::BuildingStorey.is_spatial());
    }
}
