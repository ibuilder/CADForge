# ADR-0003: IFC libraries live behind `trait IfcBackend`

Date: 2026-08-25 · Status: **Accepted** · Refines ifc-semantics.md ADR-001

## Context

`ifc-lite` (crate `ifc-lite-core` 6.0.1, MPL-2.0) is the strongest Rust IFC option found: IFC2X3/4/4X3 and IFC5/IFCX, 100% of IFC4 (776 entities) and IFC4X3 (876 entities), ~50 MB/s parse, an exact-arithmetic CSG kernel verified element-by-element against IfcOpenShell at 99.9%+ agreement, a 1.2 MB gzipped WASM build, and active daily development.

It is also **young and barely adopted**: ~1,157 downloads/month, used by 5 crates, one organization behind it. The alternative, `ifc_rs`, self-describes as work-in-progress with an API expected to change substantially. IfcOpenShell is the mature option but is C++/Python, needs a toolchain this machine lacks (no CMake), and cannot easily ship to mobile.

## Decision

`cadforge-ifc` exposes **`trait IfcBackend`**. `ifc-lite` is the default implementation. An IfcOpenShell-subprocess implementation is the desktop escape hatch.

**No third-party IFC type may appear in a `cadforge-core` signature.** Core speaks `ElementRecord`, `GlobalId`, and `ModelCommand` only.

## Consequences

- Replacing the IFC library is a one-crate job, not a refactor of the product.
- Mobile gets a pure-Rust path; desktop can escalate to IfcOpenShell for hard files.
- Cost: a projection layer must be written and maintained between backend types and `ElementRecord`. That is the price of not being married to a young dependency.
- Before `ifc-lite` becomes the committed default it must be measured against the Phase 2 corpus (PLAN.md §10, step 4). This ADR selects the *shape* of the dependency, not yet the winner.
