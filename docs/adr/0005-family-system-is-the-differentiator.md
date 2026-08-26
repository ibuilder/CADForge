# ADR-0005: The family system is the product, and its ingest limits are stated up front

Date: 2026-08-25 · Status: **Accepted**

## Context

The brief asked for "families like Revit, Blender, and Archicad". Research established what each source can actually yield — and one of them cannot yield what the phrase implies.

Separately, the open AEC landscape has viewers (Monty, ifc-lite), validators (BIM Validator), BCF managers, and one Rust CAD application (Open CAD Studio, which has no IFC support at all). **None has a family system.** The closed BIM 2.0 cohort — Motif, Arcol, Qonic, Snaptrude, Forma — is browser-first with no native mobile authoring.

## Decision

`cadforge-family` is the differentiating crate and receives disproportionate effort. A family is a versioned `FamilyDefinition`: typed parameters, named types, a deterministic `GeometryRecipe` DAG, a `HostBehavior`, a 2D symbol, and an IFC type mapping.

Ingest capability is documented **honestly per source**:

| Source | Yields |
|---|---|
| Revit `.rfa` | Parameters, types, type catalogs, and metadata via `rvt-rs`. **Not parametric geometry** — Revit's geometry and constraint solver are closed and have not been reverse-engineered. Geometry comes from Revit's IFC export; the recipe is rebuilt in CADForge. |
| Archicad `.gsm` | The best parametric path of the three. `LP_XMLConverter l2x` → HSF/XML → a GDL subset interpreter. Requires an Archicad install, so it is desktop-side offline ingest, not a runtime feature. |
| Blender | Mesh families via glTF. For parametric, go through Bonsai's IFC output, which already stores parametric definitions in IFC data. **No `.blend` parser.** |
| IFC | `IfcTypeProduct` + `IfcRelDefinesByType` + representation maps — the native family format everything else normalizes into. |

## Consequences

- "Import Revit families" means parameters and types from `.rfa`, geometry from IFC. Recording this in an ADR prevents it being promised in a roadmap and discovered mid-sprint.
- `rvt-rs` (Apache-2.0, 12 stars, one author) is **vendored and kept off the critical path**. If it breaks, `.rfa` metadata ingest degrades and nothing else does.
- The GDL interpreter is a bounded subset. Unsupported statements fail loudly rather than producing silently wrong geometry.
