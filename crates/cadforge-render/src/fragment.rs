//! Render fragments.
//!
//! A fragment is one draw-able piece of one element: bounds, a material key, a geometry hash,
//! and enough identity to map a pixel back to a `GlobalId`. It is a cache entry, never truth
//! (`docs/ifc-semantics.md` §4.6).

use cadforge_core::{BoundingBox, GlobalId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A fragment handle.
///
/// Sequential and small so it can be written straight into a GPU pick buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FragmentId(pub u32);

impl FragmentId {
    /// Reserved value meaning "nothing was hit". Real ids start at 1 so a cleared pick buffer
    /// reads as a miss rather than as fragment zero.
    pub const NONE: FragmentId = FragmentId(0);

    /// Encode into an RGBA8 pick colour.
    pub fn to_pick_color(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    /// Decode a pixel read back from the pick buffer.
    pub fn from_pick_color(rgba: [u8; 4]) -> Self {
        FragmentId(u32::from_le_bytes(rgba))
    }

    pub fn is_none(self) -> bool {
        self == Self::NONE
    }
}

/// The GPU-side facts about one piece of geometry.
///
/// Grouped rather than passed as loose arguments, because `hash`, `vertex_count`, and
/// `index_count` always travel together and three bare integers in a row is an easy call site
/// to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometrySource {
    /// Hash of the geometry **in local space**. Two elements with the same hash really are
    /// the same mesh and can share one GPU buffer — hashing world-space geometry instead
    /// silently defeats instancing.
    pub hash: u64,
    pub vertex_count: u32,
    pub index_count: u32,
}

/// One drawable piece of an element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderFragment {
    pub id: FragmentId,
    /// The element this belongs to. The only identity that means anything outside the
    /// renderer.
    pub element: GlobalId,
    /// Hash of the normalised geometry. Equal hashes may share GPU buffers — which is what
    /// makes 400 identical doors cost one upload.
    pub geometry_hash: u64,
    pub material_key: String,
    pub bounds: BoundingBox,
    pub lod: u8,
    /// The element's `representation_revision` when this fragment was built. If the element
    /// has moved past it, the fragment is stale.
    pub source_revision: u64,
    pub vertex_count: u32,
    pub index_count: u32,
}

impl RenderFragment {
    pub fn triangle_count(&self) -> u32 {
        self.index_count / 3
    }
}

/// The fragments currently held for a model.
#[derive(Debug, Clone, Default)]
pub struct FragmentSet {
    fragments: BTreeMap<FragmentId, RenderFragment>,
    by_element: BTreeMap<GlobalId, Vec<FragmentId>>,
    next_id: u32,
}

impl FragmentSet {
    pub fn new() -> Self {
        Self {
            next_id: 1, // 0 is reserved for "no hit"
            ..Default::default()
        }
    }

    /// Insert a fragment, assigning it an id.
    pub fn insert(
        &mut self,
        element: GlobalId,
        geometry: GeometrySource,
        material_key: impl Into<String>,
        bounds: BoundingBox,
        source_revision: u64,
    ) -> FragmentId {
        let id = FragmentId(self.next_id.max(1));
        self.next_id = self.next_id.max(1) + 1;

        self.fragments.insert(
            id,
            RenderFragment {
                id,
                element: element.clone(),
                geometry_hash: geometry.hash,
                material_key: material_key.into(),
                bounds,
                lod: 0,
                source_revision,
                vertex_count: geometry.vertex_count,
                index_count: geometry.index_count,
            },
        );
        self.by_element.entry(element).or_default().push(id);
        id
    }

    pub fn get(&self, id: FragmentId) -> Option<&RenderFragment> {
        self.fragments.get(&id)
    }

    /// Map a pick-buffer read back to the element that was clicked.
    ///
    /// This is the whole point of the fragment layer: pixels in, `GlobalId` out.
    pub fn element_at(&self, id: FragmentId) -> Option<&GlobalId> {
        self.fragments.get(&id).map(|f| &f.element)
    }

    pub fn fragments_of(&self, element: &GlobalId) -> &[FragmentId] {
        self.by_element
            .get(element)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Drop every fragment for an element, because its geometry changed.
    ///
    /// Returns how many were removed.
    pub fn invalidate(&mut self, element: &GlobalId) -> usize {
        let Some(ids) = self.by_element.remove(element) else {
            return 0;
        };
        for id in &ids {
            self.fragments.remove(id);
        }
        ids.len()
    }

    /// Fragments built against an older revision than the element now has.
    pub fn stale(&self, current: &dyn Fn(&GlobalId) -> Option<u64>) -> Vec<FragmentId> {
        self.fragments
            .values()
            .filter(|f| match current(&f.element) {
                // The element is gone, so its fragments are stale too.
                None => true,
                Some(revision) => revision != f.source_revision,
            })
            .map(|f| f.id)
            .collect()
    }

    /// Fragments passing a visibility test, in stable order.
    pub fn visible<F>(&self, mut test: F) -> Vec<&RenderFragment>
    where
        F: FnMut(&BoundingBox) -> bool,
    {
        self.fragments
            .values()
            .filter(|f| test(&f.bounds))
            .collect()
    }

    /// Distinct geometry hashes. The gap between this and [`FragmentSet::len`] is how much
    /// instancing can save.
    pub fn distinct_geometries(&self) -> usize {
        let unique: std::collections::BTreeSet<u64> =
            self.fragments.values().map(|f| f.geometry_hash).collect();
        unique.len()
    }

    pub fn total_triangles(&self) -> u64 {
        self.fragments
            .values()
            .map(|f| f.triangle_count() as u64)
            .sum()
    }

    pub fn bounds(&self) -> BoundingBox {
        self.fragments
            .values()
            .fold(BoundingBox::empty(), |b, f| b.union(f.bounds))
    }

    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &RenderFragment> {
        self.fragments.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    fn set_with(count: usize, geometry_hash: u64) -> (FragmentSet, Vec<GlobalId>) {
        let mut set = FragmentSet::new();
        let mut ids = Vec::new();
        for i in 0..count {
            let element = GlobalId::new();
            let origin = DVec3::new(i as f64 * 5.0, 0.0, 0.0);
            set.insert(
                element.clone(),
                GeometrySource {
                    hash: geometry_hash,
                    vertex_count: 24,
                    index_count: 36,
                },
                "concrete",
                BoundingBox::new(origin, origin + DVec3::splat(2.0)),
                0,
            );
            ids.push(element);
        }
        (set, ids)
    }

    #[test]
    fn pick_ids_round_trip_through_a_colour() {
        for raw in [1u32, 2, 255, 256, 65_535, 16_777_216, u32::MAX] {
            let id = FragmentId(raw);
            assert_eq!(FragmentId::from_pick_color(id.to_pick_color()), id);
        }
    }

    #[test]
    fn a_cleared_pick_buffer_reads_as_a_miss() {
        // Zeroed memory must not resolve to a real fragment.
        assert!(FragmentId::from_pick_color([0, 0, 0, 0]).is_none());
        let (set, _) = set_with(3, 1);
        assert_eq!(set.element_at(FragmentId::NONE), None);
    }

    #[test]
    fn ids_never_collide_with_the_miss_sentinel() {
        let (set, _) = set_with(100, 1);
        assert!(set.iter().all(|f| !f.id.is_none()));
    }

    #[test]
    fn a_pick_maps_back_to_its_element() {
        let mut set = FragmentSet::new();
        let element = GlobalId::new();
        let id = set.insert(
            element.clone(),
            GeometrySource {
                hash: 42,
                vertex_count: 8,
                index_count: 36,
            },
            "glass",
            BoundingBox::new(DVec3::ZERO, DVec3::ONE),
            3,
        );
        let picked = FragmentId::from_pick_color(id.to_pick_color());
        assert_eq!(set.element_at(picked), Some(&element));
    }

    #[test]
    fn invalidating_an_element_removes_all_of_its_fragments() {
        let mut set = FragmentSet::new();
        let element = GlobalId::new();
        for _ in 0..3 {
            set.insert(
                element.clone(),
                GeometrySource {
                    hash: 1,
                    vertex_count: 8,
                    index_count: 36,
                },
                "concrete",
                BoundingBox::new(DVec3::ZERO, DVec3::ONE),
                0,
            );
        }
        set.insert(
            GlobalId::new(),
            GeometrySource {
                hash: 2,
                vertex_count: 8,
                index_count: 36,
            },
            "steel",
            BoundingBox::new(DVec3::ZERO, DVec3::ONE),
            0,
        );
        assert_eq!(set.len(), 4);

        assert_eq!(set.invalidate(&element), 3);
        assert_eq!(set.len(), 1);
        assert!(set.fragments_of(&element).is_empty());
        assert_eq!(
            set.invalidate(&element),
            0,
            "invalidating twice is harmless"
        );
    }

    #[test]
    fn staleness_is_detected_by_revision_and_by_deletion() {
        let mut set = FragmentSet::new();
        let live = GlobalId::new();
        let moved = GlobalId::new();
        let deleted = GlobalId::new();
        for element in [&live, &moved, &deleted] {
            set.insert(
                element.clone(),
                GeometrySource {
                    hash: 1,
                    vertex_count: 8,
                    index_count: 36,
                },
                "concrete",
                BoundingBox::new(DVec3::ZERO, DVec3::ONE),
                7,
            );
        }

        let current = |id: &GlobalId| -> Option<u64> {
            if id == &live {
                Some(7) // unchanged
            } else if id == &moved {
                Some(8) // geometry changed
            } else {
                None // deleted
            }
        };

        let stale = set.stale(&current);
        assert_eq!(stale.len(), 2);
        let stale_elements: Vec<&GlobalId> =
            stale.iter().filter_map(|id| set.element_at(*id)).collect();
        assert!(stale_elements.contains(&&moved));
        assert!(stale_elements.contains(&&deleted));
        assert!(!stale_elements.contains(&&live));
    }

    #[test]
    fn repeated_geometry_is_visible_as_an_instancing_opportunity() {
        let (set, _) = set_with(400, 0xD00D);
        assert_eq!(set.len(), 400);
        assert_eq!(
            set.distinct_geometries(),
            1,
            "400 identical doors, one upload"
        );
        assert_eq!(set.total_triangles(), 400 * 12);
    }

    #[test]
    fn visibility_filtering_uses_the_supplied_test() {
        let (set, _) = set_with(10, 1);
        let near_origin = set.visible(|b| b.min.x < 12.0);
        assert_eq!(near_origin.len(), 3);
        assert!(set.visible(|_| false).is_empty());
        assert_eq!(set.visible(|_| true).len(), 10);
    }

    #[test]
    fn set_bounds_cover_every_fragment() {
        let (set, _) = set_with(5, 1);
        let bounds = set.bounds();
        assert_eq!(bounds.min, DVec3::ZERO);
        assert_eq!(bounds.max, DVec3::new(22.0, 2.0, 2.0));
        assert!(FragmentSet::new().bounds().is_empty());
    }
}
