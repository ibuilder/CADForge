//! Spatial indexing.
//!
//! An R-tree over element bounds, used for picking, selection-by-region, section boxes, and
//! the broad phase of clash detection (`docs/ifc-semantics.md` §4.2, §11 Phase 5).
//!
//! Bulk-loaded and rebuilt rather than updated in place. A stale index silently returns wrong
//! answers, which is a far worse failure mode than the cost of a rebuild.

use crate::element::BoundingBox;
use crate::id::GlobalId;
use rstar::{RTree, RTreeObject, AABB};

/// An element as the index sees it: an identity and a box, nothing more.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedElement {
    pub global_id: GlobalId,
    pub bounds: BoundingBox,
}

impl RTreeObject for IndexedElement {
    type Envelope = AABB<[f64; 3]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(self.bounds.min.to_array(), self.bounds.max.to_array())
    }
}

/// A spatial index over elements with evaluated geometry.
#[derive(Debug, Clone, Default)]
pub struct SpatialIndex {
    tree: RTree<IndexedElement>,
}

impl SpatialIndex {
    pub fn build(elements: impl IntoIterator<Item = IndexedElement>) -> Self {
        // Bulk loading builds a far better-balanced tree than repeated insertion.
        Self {
            tree: RTree::bulk_load(elements.into_iter().collect()),
        }
    }

    pub fn len(&self) -> usize {
        self.tree.size()
    }

    pub fn is_empty(&self) -> bool {
        self.tree.size() == 0
    }

    /// Everything whose bounds intersect the query box.
    pub fn query(&self, bounds: &BoundingBox) -> Vec<&IndexedElement> {
        if bounds.is_empty() {
            return Vec::new();
        }
        let envelope = AABB::from_corners(bounds.min.to_array(), bounds.max.to_array());
        self.tree
            .locate_in_envelope_intersecting(envelope)
            .collect()
    }

    /// Identities of everything intersecting the query box, in stable order.
    ///
    /// The R-tree yields results in traversal order, which depends on tree shape. Sorting
    /// keeps selection results reproducible across rebuilds — which matters for tests and for
    /// anything user-visible.
    pub fn query_ids(&self, bounds: &BoundingBox) -> Vec<GlobalId> {
        let mut ids: Vec<GlobalId> = self
            .query(bounds)
            .into_iter()
            .map(|e| e.global_id.clone())
            .collect();
        ids.sort();
        ids
    }

    pub fn iter(&self) -> impl Iterator<Item = &IndexedElement> {
        self.tree.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    fn cube_at(x: f64) -> IndexedElement {
        IndexedElement {
            global_id: GlobalId::new(),
            bounds: BoundingBox::new(DVec3::new(x, 0.0, 0.0), DVec3::new(x + 1.0, 1.0, 1.0)),
        }
    }

    #[test]
    fn finds_only_what_overlaps() {
        let cubes: Vec<_> = (0..10).map(|i| cube_at(i as f64 * 10.0)).collect();
        let expected = cubes[3].global_id.clone();
        let index = SpatialIndex::build(cubes);
        assert_eq!(index.len(), 10);

        let hits = index.query_ids(&BoundingBox::new(
            DVec3::new(30.2, 0.2, 0.2),
            DVec3::new(30.8, 0.8, 0.8),
        ));
        assert_eq!(hits, vec![expected]);
    }

    #[test]
    fn an_empty_query_box_matches_nothing() {
        let index = SpatialIndex::build((0..5).map(|i| cube_at(i as f64)));
        assert!(index.query_ids(&BoundingBox::empty()).is_empty());
    }

    #[test]
    fn a_box_spanning_everything_matches_everything() {
        let index = SpatialIndex::build((0..64).map(|i| cube_at(i as f64 * 2.0)));
        let all = index.query_ids(&BoundingBox::new(
            DVec3::splat(-1000.0),
            DVec3::splat(1000.0),
        ));
        assert_eq!(all.len(), 64);
    }

    #[test]
    fn results_are_order_stable_across_rebuilds() {
        let cubes: Vec<_> = (0..32).map(|i| cube_at(i as f64)).collect();
        let query = BoundingBox::new(DVec3::new(4.0, 0.0, 0.0), DVec3::new(9.0, 1.0, 1.0));

        let forward = SpatialIndex::build(cubes.clone()).query_ids(&query);
        let reversed = SpatialIndex::build(cubes.into_iter().rev()).query_ids(&query);
        assert_eq!(forward, reversed);
    }

    #[test]
    fn an_empty_index_answers_without_panicking() {
        let index = SpatialIndex::default();
        assert!(index.is_empty());
        assert!(index
            .query_ids(&BoundingBox::new(DVec3::ZERO, DVec3::ONE))
            .is_empty());
    }
}
