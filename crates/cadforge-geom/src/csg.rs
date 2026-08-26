//! The CSG boundary.
//!
//! Booleans are the one operation CADForge does not implement itself. Robust mesh booleans
//! are a research-grade problem — floating-point boolean kernels are where CAD software goes
//! to produce silently wrong geometry — so the operation sits behind a trait and is delegated
//! (ADR-0004).
//!
//! The intended default backend is the exact-arithmetic kernel in `ifc-lite`, which is
//! verified element-by-element against IfcOpenShell at 99.9%+ agreement. It is not wired up
//! until it has been measured against the Phase 2 corpus (PLAN.md §10), so the default today
//! is [`UnavailableCsg`], which **fails loudly**.
//!
//! Failing loudly is the point. A boolean that silently returns one of its operands, or a
//! self-intersecting shell, corrupts a model file — and that is discovered weeks later by
//! somebody else.

use crate::mesh::IndexedMesh;
use crate::GeometryError;

/// A mesh boolean provider.
pub trait CsgBackend {
    /// Human-readable backend name, for the degraded-geometry report.
    fn name(&self) -> &'static str;

    /// `a ∪ b`
    fn union(&self, a: &IndexedMesh, b: &IndexedMesh) -> Result<IndexedMesh, GeometryError>;

    /// `a − b`. The opening cut: a wall minus its openings.
    fn difference(&self, a: &IndexedMesh, b: &IndexedMesh) -> Result<IndexedMesh, GeometryError>;

    /// `a ∩ b`. The clash-detection primitive.
    fn intersection(&self, a: &IndexedMesh, b: &IndexedMesh) -> Result<IndexedMesh, GeometryError>;

    /// Subtract many operands. Cutting a wall with all of its openings at once lets a real
    /// kernel batch the work instead of rebuilding intermediate shells.
    fn difference_many(
        &self,
        a: &IndexedMesh,
        others: &[IndexedMesh],
    ) -> Result<IndexedMesh, GeometryError> {
        others
            .iter()
            .try_fold(a.clone(), |acc, b| self.difference(&acc, b))
    }
}

/// The default backend: refuses every operation.
///
/// Present so the boundary is exercised and the failure path is tested from day one, rather
/// than being discovered when the first real backend is swapped in.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableCsg;

impl CsgBackend for UnavailableCsg {
    fn name(&self) -> &'static str {
        "unavailable"
    }

    fn union(&self, _a: &IndexedMesh, _b: &IndexedMesh) -> Result<IndexedMesh, GeometryError> {
        Err(GeometryError::CsgUnavailable)
    }

    fn difference(&self, _a: &IndexedMesh, _b: &IndexedMesh) -> Result<IndexedMesh, GeometryError> {
        Err(GeometryError::CsgUnavailable)
    }

    fn intersection(
        &self,
        _a: &IndexedMesh,
        _b: &IndexedMesh,
    ) -> Result<IndexedMesh, GeometryError> {
        Err(GeometryError::CsgUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{extrude, Profile};

    fn box_mesh(size: f64) -> IndexedMesh {
        extrude(&Profile::rectangle(size, size).unwrap(), size).unwrap()
    }

    #[test]
    fn the_default_backend_refuses_rather_than_approximating() {
        let backend = UnavailableCsg;
        let a = box_mesh(2.0);
        let b = box_mesh(1.0);

        assert_eq!(backend.union(&a, &b), Err(GeometryError::CsgUnavailable));
        assert_eq!(
            backend.difference(&a, &b),
            Err(GeometryError::CsgUnavailable)
        );
        assert_eq!(
            backend.intersection(&a, &b),
            Err(GeometryError::CsgUnavailable)
        );
        assert_eq!(backend.name(), "unavailable");
    }

    #[test]
    fn subtracting_nothing_is_the_identity_even_on_a_dead_backend() {
        // `difference_many` with an empty operand list must not invent a failure — the
        // uncut wall is the correct answer.
        let a = box_mesh(2.0);
        let out = UnavailableCsg.difference_many(&a, &[]).unwrap();
        assert_eq!(out, a);
    }

    #[test]
    fn subtracting_anything_propagates_the_failure() {
        let a = box_mesh(2.0);
        assert_eq!(
            UnavailableCsg.difference_many(&a, &[box_mesh(1.0)]),
            Err(GeometryError::CsgUnavailable)
        );
    }
}
