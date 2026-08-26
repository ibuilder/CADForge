# ADR-0007: Own the IFC writer, delegate the IFC reader

Date: 2026-08-25 · Status: **Accepted** · Refines [ADR-0003](0003-ifc-backend-behind-a-trait.md)

## Context

ADR-0003 put every IFC library behind `trait IfcBackend` and named `ifc-lite` as the intended default, pending measurement. That decision treated reading and writing as one problem. They are not.

**Reading arbitrary IFC is genuinely hard.** Files arrive from Revit, Archicad, Tekla, and a long tail of consultant exporters, each with its own interpretation of the schema, and many of them malformed. IFC4 has 776 entities and IFC4X3 has 876. Handling that surface is exactly what justifies a library, and doing it badly means silently mis-importing somebody's building.

**Writing the subset CADForge authors is bounded.** The exporter has to emit what the model can hold: spatial structure, swept solids, tessellated fallbacks, four relationship types, property sets, and type objects. That is a closed set defined by CADForge itself, not by the ecosystem.

## Decision

`cadforge-ifc` **implements its own IFC4 STEP writer** (`SpfBackend`). Reading stays delegated to a third-party backend behind `IfcBackend`.

## Consequences

- **Export works on every platform from day one** — no C++ toolchain, no CMake, no subprocess, and nothing that cannot ship to iOS or Android.
- **Export does not depend on a young library.** `ifc-lite` may or may not survive; the ability to get a user's model out of CADForge does not depend on the answer.
- **Parametric geometry survives the crossing.** A single extrusion is written as `IfcExtrudedAreaSolid` over `IfcArbitraryClosedProfileDef` — profile and depth, not triangles — so a wall stays editable in Revit, Archicad, or Bonsai after a round trip. That is the point of [ADR-0004](0004-no-brep-kernel.md), and delegating export would have put it in someone else's hands.
- **Export is byte-reproducible.** Synthesised identities are derived by FNV-1a from the project name, entity kind, and sequence rather than randomly minted, and the header timestamp is caller-supplied. Same model plus same `ExportContext` gives the same bytes, which is what makes content-addressed revisions and golden-file tests possible. This was a real bug caught by its own test: the first implementation minted a fresh `GlobalId` per synthesised entity and produced a different file every run.
- **Cost: the writer is ours to maintain.** Every entity's attribute list is hand-written and every one is a chance to be wrong in a way that only shows up in another application. Mitigated by tests that assert on the structural failure modes rather than on formatting — every `#N` resolves, no id repeats, closed polylines repeat their first point, `IfcTriangulatedFaceSet` indices are 1-based, openings are not listed in `IfcRelContainedInSpatialStructure`.
- **Cost: the subset is a subset.** No `IfcOwnerHistory`, no materials, no georeferencing, no `IfcArbitraryProfileDefWithVoids`, and an unmodelled class degrades to `IfcBuildingElementProxy` with its original entity name kept in `ObjectType`. Each of these is a known gap rather than a surprise, and each is additive.

## Validated

The structural tests prove the file is internally consistent; they cannot prove anyone else accepts it. So the output was run through **IfcOpenShell 0.8.5**, the reference implementation (`tools/validate_ifc.py`):

- Parses as IFC4, **no schema or EXPRESS rule violations**.
- Units, representation context, and spatial hierarchy resolve.
- `IfcRelVoidsElement`, `IfcRelFillsElement`, `IfcRelContainedInSpatialStructure`, `IfcRelDefinesByType`, and `IfcRelDefinesByProperties` all navigate correctly; the door resolves to its `IfcDoorType` and `IsExternal` survives as a boolean.
- **Geometry generates for every element** from the swept solids alone - no tessellated fallbacks in the file.
- Every `GlobalId` survives a round trip through the IfcOpenShell writer.

Two initial failures were both defects in the *checks*, not in the file, and both are worth recording because they are easy to get backwards:

1. `ifcopenshell.util.element.get_container()` resolves the container of an opening **through its host**, so it returns a storey even for a correctly-authored file. The attribute that actually matters is the raw `ContainedInStructure`, which is empty as intended.
2. IfcOpenShell reported **less** wall volume than CADForge did - 15.2099 m³ against 15.60 m³. The difference is exactly 0.92 × 0.20 × 2.12 = 0.39008 m³, the door opening. The reference implementation was applying the void, which is the behaviour we wanted; the expectation had simply been written against the uncut number.

That second one surfaced a genuine inconsistency in CADForge, now stated plainly in the demo output: with no CSG backend (ADR-0004) our own meshes are **uncut**, so the viewer shows a solid wall while every IFC consumer subtracts the opening. The file is right and the viewer is behind it.

Still not done: opening the output in Bonsai and Revit by hand. IfcOpenShell acceptance is strong evidence - Bonsai is built on it - but it is not the same bar as a commercial importer.
