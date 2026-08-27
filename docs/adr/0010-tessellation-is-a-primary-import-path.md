# ADR-0010: Tessellation is a primary import path, not a fallback

Date: 2026-08-26 · Status: **Accepted** · Qualifies [ADR-0004](0004-no-brep-kernel.md)

## Context

[ADR-0004](0004-no-brep-kernel.md) built the geometry strategy on a claim:

> Walls, slabs, columns, beams, openings, doors, windows, railings, ducts, and pipes are all
> profile sweeps. This covers the large majority of real building elements.

That reasoning has been load-bearing since. It justified having no B-rep kernel, it shaped
`Representation`, and it is why `TriangulatedFaceSet` was framed as an explicit *degradation*
rather than an ordinary case.

The corpus was fetched (`tools/fetch_corpus.py`) and surveyed
(`cargo run --release -p cadforge-ifc --example corpus_survey`) over buildingSMART's 23
certification sample models — 16.7 MB, IFC4 and IFC4X3, covering architecture, structural,
HVAC, plumbing, road, rail, bridge, and landscaping.

**719 shape representations. 710 of them — 98.7% — are `Tessellation`. Seven are `SweptSolid`.**

## The claim was true about the wrong thing

ADR-0004 is correct about **authoring**. When CADForge creates a wall it is a swept profile,
that exports as `IfcExtrudedAreaSolid`, and it stays editable downstream. Nothing here changes
that.

It was wrong to extend the claim to **files that arrive**. What an exporter emits is not what
an author drew. By the time geometry has crossed a vendor boundary it has very often been
baked to triangles, and no amount of profile arithmetic gets it back.

## What the numbers do and do not support

Read honestly, because this corpus is not a uniform sample:

- **It over-represents tessellation.** Four of the 23 files exist specifically to test
  tessellation conformance, and eight more are infrastructure and landscape models where mesh
  geometry is the norm.
- **The architectural files are less extreme.** `building-architecture.ifc` is 14% swept
  solids, and the purpose-built `wall-with-opening-and-window.ifc` is 75%.
- **Real Revit and Archicad architectural exports are expected to be swept-heavy**, well above
  what this corpus shows. That class of file has still not been sourced and remains the open
  gap.

So the defensible statement is not "IFC is 99% tessellated". It is: **tessellation is common
enough, across enough file types, that treating it as a degraded edge case would make the
importer useless on most of the corpus we have.** Even if real architectural exports turn out
to be 60% swept, the remaining 40% is a primary path by any reasonable definition.

## Decision

For **import**, `Representation::TriangulatedFaceSet` is a first-class case:

1. It gets the same care as `ExtrudedAreaSolid` — correct normals, bounds, and identity, not a
   best-effort conversion.
2. It **round-trips losslessly**. A file that arrives tessellated leaves tessellated, with the
   same vertices. CADForge must never silently re-mesh somebody else's geometry.
3. It is **not reported as a degradation on import**. `RepresentationKind::is_degraded()`
   describes what happened to geometry CADForge *authored* and could not express natively.
   Applying that label to a file that was always tessellated tells the user something false.

For **authoring**, ADR-0004 stands unchanged.

## Also found

**`IfcClass::Other` is doing more work than expected.** The corpus contains 37 distinct product
classes; only **43% of product instances** map to a native `IfcClass` variant. The rest are
largely infrastructure — `IfcGeographicElement` (160), `IfcTrackElement` (66),
`IfcPipeSegment` (48), `IfcElementAssembly` (47), `IfcRoadPart`, `IfcBridgePart`,
`IfcEarthworksFill`.

That vindicates the `Other(String)` fallback, and raises its importance: it is not a rare
escape hatch, it is the path for the majority of what arrives. Preserving the original entity
name and round-tripping it exactly is a correctness requirement, not a nicety.

Expanding `IfcClass` to cover infrastructure is explicitly **not** the answer. CADForge does
not author roads or rail, and adding variants it cannot author would imply a capability that
does not exist.

**Two pieces of CADForge were validated on files it did not write.** Schema detection
identified all 23 correctly, including the IFC4X3 files that a naive `starts_with("IFC4")`
would have mis-read. `ifc-lite-core` parsed every file, at 47–2,650 MB/s.

## Consequences

- The Phase 2b importer is a **tessellation-first** design with swept solids as the
  recognised-and-preserved case, which is the opposite emphasis from the writer.
- `IfcClass::Other` needs round-trip tests as thorough as the native variants get.
- The "degraded geometry" reporting in the UI must distinguish *we could not express this* from
  *it arrived this way*. Same representation, entirely different meaning.
- **The gap that remains is unchanged and now sharper**: no messy real-world consultant export
  has been tested. Vendor-exported architectural models are the next thing to source, and this
  ADR should be revisited when they exist — it may well move the numbers again.
