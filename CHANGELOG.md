# Changelog

All notable changes to CADForge are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the major version
is `0`, the public API may change in any release.

## [Unreleased]

Nothing yet.

## [0.1.0] — 2026-08-26

First public release. Phases 0, 2a, and 3a of the [roadmap](ROADMAP.md) are complete: the
semantic core, the geometry pipeline, the family system, IFC **export**, and a GPU renderer
all work and are tested. IFC **import** and a windowed viewport are not built yet.

### Added

#### Semantic core (`cadforge-core`)

- `GlobalId` — `IfcGloballyUniqueId` compression and expansion, validated on parse, with
  deterministic derivation from raw bytes for reproducible export.
- `ElementRecord` with IFC-shaped `Placement` (location + axis + reference direction, as
  `IfcAxis2Placement3D` stores it) rather than a raw 4×4 matrix.
- `ModelCommand` — every mutation is a command, and **every variant has an exact inverse**.
  Nothing else may touch the store, which is what buys undo, audit, and replay.
- `Model` with revisions, undo/redo, an append-only history, and validation before mutation:
  a rejected command leaves the model untouched.
- `Representation` — the bridge between an authored recipe and what a file carries.
  `ExtrudedAreaSolid` stays parametric; `TriangulatedFaceSet` is the explicit degradation.
- `PropertySets` — `BTreeMap`-backed so export is reproducible; measures keep their IFC type,
  so a length and a bare real are not interchangeable.
- `SpatialIndex` — bulk-loaded R-tree with order-stable queries.

#### Geometry (`cadforge-geom`)

- `Profile` — closed 2D profiles with validated winding, ear-clipping triangulation, and
  tolerance-driven circle segmentation.
- `extrude` / `extrude_along` — profile sweeps with outward-facing normals, the operation
  that covers walls, slabs, columns, beams, openings, ducts, and pipes.
- `IndexedMesh` with surface area, signed volume, and structural validity checks.
- `TessellationSettings` — segment counts derived from a chord tolerance, never hard-coded.
- **`BspCsg`** — binary-space-partition mesh booleans, pure Rust, deterministic. Openings now
  actually cut their hosts.
- `is_watertight` and `is_edge_manifold` as separate, public, and deliberately different
  checks.

#### Families (`cadforge-family`)

- `FamilyDefinition` — typed parameters, named types, a deterministic geometry recipe, host
  behaviour, and an IFC type mapping.
- `ParamDef` with type/instance scoping enforced, not merely conventional.
- `Expr` — arithmetic over parameters via real operator traits, so a formula reads the way it
  is written on paper.
- `GeometryRecipe` — an acyclic op list where a step may only reference earlier steps, so the
  graph is acyclic by construction.
- `place()` — placing a hosted family expands into the full IFC relationship set: a real
  `IfcOpeningElement`, an `IfcRelVoidsElement`, and an `IfcRelFillsElement`.

#### IFC (`cadforge-ifc`)

- `IfcBackend` — the swappable boundary. No third-party IFC type reaches the core.
- **`SpfBackend`** — a native IFC4 STEP writer. Byte-reproducible, with interned primitives
  and deterministic synthesised identities.
- `IfcSchema::detect` — header parsing that does not mistake IFC4X3 for IFC4.

#### Rendering (`cadforge-render`)

- `Camera` — orbit camera in `f64`, narrowing to `f32` only at upload.
- `Frustum` — Gribb–Hartmann plane extraction for a 0..1 depth range.
- `FragmentSet` — geometry hashing for instancing, staleness detection, and pick encoding.
- **Headless `wgpu` renderer** behind the `gpu` feature: full pipeline, depth, back-face
  culling, framebuffer readback, PNG output.

#### Tooling and documentation

- `tools/validate_ifc.py` — validates exported IFC against IfcOpenShell.
- `crates/cadforge-ifc/examples/ifclite_spike.rs` — the measurement behind ADR-0009.
- Nine architecture decision records, and dated landscape research with sources.

### Verified

- **179 tests** (186 with `--features gpu`), zero clippy warnings, `cargo fmt` clean.
- Exported IFC4 validated against **IfcOpenShell 0.8.5**: no schema or EXPRESS rule
  violations, every relationship resolves, geometry generates for every element from the
  swept solids alone, and every `GlobalId` survives a round trip.
- The GPU path runs on real hardware (AMD Radeon via Vulkan).
- `ifc-lite-core` measured at 520 MB/s scan over a 17.5 MB file, recovering the full
  parametric recipe.

### Fixed

- **Quadratic IFC export.** `write_voids_and_fills` asked each element for its openings, and
  `Model::openings_of` scans the whole relationship set. Exporting 20,000 walls took 5.86 s;
  it now takes 0.77 s, and per-megabyte throughput is better at 20k than it was at 2k.
- **Non-reproducible export.** Synthesised identities were randomly minted, so every export
  of an unchanged model produced a different file. Now derived by FNV-1a from the project
  name, entity kind, and sequence.
- **Instancing defeated by world-space hashing.** Geometry hashes are taken in local space, so
  two identical walls in different positions share one GPU buffer.

### Known limitations

- **No IFC import.** Export works and is validated; reading is Phase 2b.
- **No window.** The renderer is headless; a `winit` viewport is Phase 3b.
- **iOS and Android are unrun.** The code targets them and wgpu supports Metal and Vulkan
  there, but compiling for a target is not running on one.
- **`BspCsg` is `f64`, not exact.** T-junctions, coplanar faces, and superlinear cost are
  documented in [ADR-0008](docs/adr/0008-bsp-csg-backend.md) and pinned by tests.
- **The IFC writer covers a subset.** No `IfcOwnerHistory`, materials, georeferencing, or
  voided profiles. An unmodelled class degrades to `IfcBuildingElementProxy` keeping its
  original entity name in `ObjectType`.
- **Only CADForge-authored files have been round-tripped.** Robustness against real-world
  consultant exports is unproven.

[Unreleased]: https://github.com/ibuilder/CADForge/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ibuilder/CADForge/releases/tag/v0.1.0
