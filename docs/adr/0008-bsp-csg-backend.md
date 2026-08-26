# ADR-0008: A BSP mesh boolean, with its limits written down

Date: 2026-08-26 · Status: **Accepted** · Fills the gap left by [ADR-0004](0004-no-brep-kernel.md)

## Context

ADR-0004 put booleans behind `trait CsgBackend` and shipped `UnavailableCsg`, which refuses every operation. That was the right default while nothing was proven, but it left CADForge in an incoherent state that IfcOpenShell validation made obvious:

- The **exported IFC was correct** — a real `IfcOpenElement` with `IfcRelVoidsElement`, and every consumer subtracted the opening (15.2099 m³ across the demo's four walls).
- The **viewer was wrong** — with no boolean, our own meshes stayed uncut and showed a solid wall (15.60 m³).

For a tool whose viewport is the primary working surface, that is backwards. The file being right does not help someone looking at a wall with no door in it.

The intended kernel is still `ifc-lite`'s exact-arithmetic implementation, verified against IfcOpenShell at 99.9%+ agreement. But it has not been measured against a corpus yet, and adopting an unmeasured dependency for something this load-bearing is the mistake ADR-0003 exists to avoid.

## Decision

Implement **`BspCsg`** in `cadforge-geom`: the classic binary-space-partition boolean (Naylor, Amanatides & Thibault 1990 — the `csg.js` construction). Pure Rust, no dependency, deterministic, ~450 lines.

It becomes the working backend. When `ifc-lite`'s kernel is measured and adopted, it plugs in behind the same trait and this becomes the fallback.

## Consequences

- **The viewer and the file now agree**, exactly: 15.21 m³ on both sides in the demo.
- **Openings work end to end** without a kernel dependency, on every target platform.
- **Deterministic.** No randomised plane selection, no shuffling — the same operands always produce the same output mesh, so a cut mesh stays cacheable by hash.
- **Exported IFC is unaffected**, which is the correct behaviour and worth stating: the `Representation` is canonical and stays an `IfcExtrudedAreaSolid`. Cutting is a *viewer* concern. The demo file kept the same size, the same 107 entities, and the same six swept solids across the change. (Not literally byte-identical between runs, because the demo mints fresh `GlobalId`s each time it authors a model — export of a *fixed* model is byte-reproducible, and that is what the test asserts.)

### What it is not

It is not an exact-arithmetic kernel. It works in `f64` against a 1 µm epsilon, and it inherits the known weaknesses of the approach. All of these are documented in the module and pinned by tests rather than left as folklore:

- **T-junctions.** Cutting subdivides a face without subdividing its neighbour, so results are **watertight but not edge-manifold**.
- **Coplanar faces** are decided by an epsilon comparison. A wall flush against a slab is the awkward case.
- **Cost is superlinear**, and both tree construction and clipping recurse. Intended for element-scale operands — a wall and its openings — not whole models.

### The T-junction finding, because it changed the design

The first implementation used edge-manifoldness as the operand precondition: every edge shared by exactly two triangles in opposite directions. That is the textbook test for a closed shell, and it is the right test for *authored* geometry coming out of a sweep.

It is the wrong test here, and four tests failed to say so. Volumes were all correct; the shells were rejected anyway. BSP output has T-junctions by construction, so requiring edge-manifoldness **rejects the output of the very operation that produced it** — meaning `difference_many` could never apply a second opening to a wall.

The precondition is now the **vector area**: `∮ n dA = 0` holds exactly for any closed surface because opposing faces cancel, and it is entirely insensitive to how faces are subdivided. An open surface leaves a residual equal to its boundary's vector area.

Both checks are public API — `is_watertight` for boolean preconditions, `is_edge_manifold` for validating authored geometry, where a failure means a real bug. A test asserts that BSP output is watertight and **not** edge-manifold, so that if the backend ever gains T-junction repair, the test fails and these docs get corrected instead of quietly rotting.
