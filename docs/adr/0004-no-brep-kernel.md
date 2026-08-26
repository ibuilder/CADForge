# ADR-0004: Parametric recipes and tessellation, not a B-rep kernel

Date: 2026-08-25 · Status: **Accepted** · Supersedes ifc-semantics.md ADR-003

## Context

ifc-semantics.md ADR-003 scoped Truck as a controlled authoring kernel. Research on 2026-08-25 found the Rust B-rep landscape worse than that ADR assumed:

- **Fornjot is dead.** The author ended it: "No longer in development… its goals were not reached." Years of skilled, sustained work did not produce a usable kernel.
- **`truck`** is genuinely maintained (1,533 stars, pushed 2026-08-24) but its **crates.io releases are two years stale** — `truck-modeling` 0.6.0 published 2024-09-20, 66k lifetime downloads — and recent commits are largely `cargo upgrade`. Using it means a git dependency against an unpinned surface.
- **`opencascade-rs`** is a work-in-progress C++ binding requiring CMake, which is not installed here and does not ship easily to iOS or Android.

There is no production-grade pure-Rust B-rep kernel in 2026.

## Decision

CADForge is **not built on a B-rep kernel.** The canonical geometry of an authored element is a **deterministic parametric recipe** — profile, sweep, extrude, boolean, transform — and the mesh is a derived, disposable cache.

Booleans go through `trait CsgBackend`, defaulting to the exact-arithmetic kernel in `ifc-lite`.

Where an operation cannot be expressed as a recipe, it degrades **explicitly**: a tessellated representation, flagged in the UI and the validation report, with the source command preserved so a parametric representation can be regenerated later. ifc-semantics.md §6.2 already specifies this correctly and stands.

## Consequences

- Walls, slabs, columns, beams, openings, doors, windows, railings, ducts, and pipes are all profile sweeps. This covers the large majority of real building elements.
- Recipes export directly as `IfcExtrudedAreaSolid` + `IfcArbitraryClosedProfileDef` — native parametric IFC, not tessellated mush.
- Determinism buys reproducible export, cacheable meshes, and golden-file testability.
- Cost: freeform and organic geometry is out of scope for *authoring*. Imported freeform geometry is still viewed and preserved — it simply is not editable as a recipe.
- The largest technical risk in the project moves off the critical path.
