# ADR-0009: `ifc-lite-core` is the read backend, on measurement

Date: 2026-08-26 · Status: **Accepted** · Settles the open question in [ADR-0003](0003-ifc-backend-behind-a-trait.md)

## Context

ADR-0003 chose the *shape* of the IFC dependency — everything behind `trait IfcBackend` — and explicitly deferred the choice of implementation: *"Before `ifc-lite` becomes the committed default it must be measured against the Phase 2 corpus. This ADR selects the shape, not yet the winner."*

The concern was real. `ifc-lite-core` is 6.0.1, MPL-2.0, roughly 1,157 downloads a month, five dependent crates, one organisation. Excellent work, but young and barely adopted — exactly the profile that should not be adopted on a README claim.

## The measurement

`crates/cadforge-ifc/examples/ifclite_spike.rs`. It authors a synthetic model, exports it with `SpfBackend`, then tries to recover the semantics with `ifc-lite-core` alone. Release build, AMD Radeon workstation, 20,000 walls with a door in every fourth — 30,001 elements, 310,031 IFC entities, 17.5 MB.

| Stage | Result |
|---|---|
| Scan (entity discovery) | 33.7 ms — **520 MB/s** |
| Index (id → byte range) | 105 ms — **166 MB/s** |
| Decode 25,000 entities with full attributes | 113 ms |

Every check passed at both 2,000 and 20,000 walls:

- Wall, door, `IfcRelVoidsElement`, and `IfcRelFillsElement` counts all match what was authored.
- **Every authored `GlobalId` comes back byte-for-byte.**
- Every `IfcRelVoidsElement` resolves to a wall we know.
- The full geometry walk works: `IfcWall` → `IfcProductDefinitionShape` → `IfcShapeRepresentation` → `IfcExtrudedAreaSolid` → `IfcArbitraryClosedProfileDef` → `IfcPolyline` → points, recovering `[[0,0],[4,0],[4,0.2],[0,0.2]]` and depth 3.0 exactly.

That last one is the decisive result. It recovers the **recipe** — profile and depth — not triangles, which is precisely what [ADR-0004](0004-no-brep-kernel.md) needs to reconstruct an editable `Representation` rather than a dead mesh.

## Decision

`ifc-lite-core` becomes the **read** backend for Phase 2b. Writing stays ours ([ADR-0007](0007-own-the-ifc-writer.md)).

It stays a **dev-dependency until Phase 2b actually lands** — the spike proves it works; nothing shipped depends on it yet.

## What tipped it beyond the numbers

`IfcType::attribute_index(name)` and `attribute_names()`. The projection layer looks attributes up **by name**, never by position, so it never hardcodes "GlobalId is attribute 0" — the assumption that quietly breaks across schema versions. A library that makes the careful thing the easy thing is worth more than one that is merely fast.

## Consequences

- Phase 2b is de-risked: the hard part is a projection layer, not a parser.
- The `IfcBackend` boundary stays. Adoption here is a decision about the *default*, and IfcOpenShell-as-subprocess remains the desktop escape hatch for files this cannot handle.
- **Still unmeasured: real-world files.** Every byte in this spike was written by CADForge. That proves the round trip, not robustness against a consultant's Revit export with duplicate GlobalIds and malformed geometry. The corpus (ifc-semantics.md §12.1) is still the gate before Phase 2b ships.
- MPL-2.0 is file-level copyleft and compatible with CADForge's own MPL-2.0.

## The bug it found in *our* code

The spike was pointed at `ifc-lite`; it caught a defect in CADForge instead, which is the argument for running spikes at scale rather than on toy inputs.

Export throughput **fell** from 19.1 MB/s at 2,000 walls to 3.0 MB/s at 20,000 — superlinear, so a real algorithmic fault. `SpfBackend::write_voids_and_fills` asked each element for its openings, and `Model::openings_of` **scans the whole relationship set**. Elements × voids: 30,000 × 5,000.

Fixed by walking the relationship sets directly, via new `Model::voids()` and `Model::fills()` accessors. Export of the 20k model went **5.86 s → 0.77 s**, and per-megabyte throughput is now *better* at 20k than it was at 2k, which is how you can tell the quadratic term is actually gone rather than merely smaller.

`openings_of` is still a scan, and is now documented as such — correct for asking about one element, wrong inside a loop. If it ever shows up in a profile for single-element queries, the fix is a secondary index keyed by host, not a change at every call site.
