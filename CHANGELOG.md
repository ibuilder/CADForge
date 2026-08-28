# Changelog

All notable changes to CADForge are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the major version
is `0`, the public API may change in any release.

## [Unreleased]

### Added

- **Authoring tools.** Phase 5. `cadforge-tools` turns clicks into elements: a wall from two
  points, a column from one, a slab from a closed outline. Every tool emits ordinary
  `ModelCommand`s, so a hand-drawn wall is indistinguishable from an authored one and undo,
  redo, revisions, and export needed no changes at all.
  - **All output is native parametric.** Walls, slabs, and columns export as
    `IfcExtrudedAreaSolid` over `IfcArbitraryClosedProfileDef`, never as triangles. A tool
    that emitted a mesh would look identical on screen and arrive in Revit or Archicad as
    something nobody can edit.
  - **Snapping** (`cadforge_geom::snap`): candidates beat the grid, the grid leaves elevation
    alone, and the tolerance is derived from a fixed reach in *pixels*, because "near where I
    clicked" is a screen-space idea and a metric tolerance is grabby zoomed out and
    unreachable zoomed in.
  - Snap candidates include profile **edge midpoints**, not just corners. For a wall — whose
    profile is the centreline spread half a thickness either side — the midpoint of an end
    edge *is* the centreline endpoint. Without it, a wall drawn onto another lands on a face
    corner half a thickness off: joined to the eye, wrong to a tape measure.
  - A live **preview** of what the next click would build, produced by running the real
    construction against a hypothetical point rather than by a second implementation of the
    same geometry.
  - In the viewport: `1`/`2`/`3`/`4` select the tool, `Enter` closes a slab outline,
    `Backspace` takes a point back, `Esc` cancels, `Ctrl+Z`/`Ctrl+Y` undo and redo. Drawn
    elements are filed in the lowest storey.
- `Camera::ray_at` and `Ray::intersect_plane`/`intersect_ground` — screen pixel to world
  point, which is the whole of what an authoring tool needs from a camera and is pure enough
  to test without a window.
- `examples/drawn_room.rs` draws a room with the tools from clicks that are every one of them
  29 mm off, asserts the eight wall ends meet exactly, exports, and renders it. The drawing
  and export need no GPU, so **CI now validates a tool-drawn file against IfcOpenShell**
  alongside the code-authored demo.

- **A window.** `cadforge-viewport` (feature `viewport`) opens a real `winit` window on a
  `wgpu` surface with orbit, pan, and zoom. Phase 3b.
  - Give it an `.ifc` path and it imports and displays the file — the first place the reader,
    the geometry pipeline, and the renderer are all load-bearing at once.
  - `--frames N` renders N frames and exits, so the viewport is testable without a human.
  - `--png <path>` renders headless and never opens a window, for thumbnails and for machines
    with no display.
- `Renderer::for_surface` and `render_to_view` — the renderer now draws to a swapchain or an
  offscreen texture through one code path, which is the claim ADR-0001 rests on. The surface
  format is taken from the surface's own capabilities rather than guessed.
- **The CADForge mark**, authored rather than drawn: a 13-point closed profile swept into a
  solid and rendered by CADForge (`examples/logo.rs`). An SVG twin carries the same points for
  the favicon and site header.
- **API documentation** published at <https://ibuilder.github.io/CADForge/api/>, built from
  rustdoc on every push and generated at deploy time rather than committed.

- **GPU picking.** `Renderer::pick` renders the scene a second time writing identities
  instead of colour and reads back the pixel under the cursor. It shares the shading pass
  depth test, so what you click can never disagree with what you see — which ray casting
  against the model cannot guarantee. `MeshData` carries a `FragmentId`; zero means drawn but
  not selectable, for grids and gizmos.
- Clicking in the viewport selects an element and highlights it, distinguishing a click from
  the end of an orbit drag by how far the cursor moved.
- **Section planes.** `SectionPlane` is GPU-free view state — the model never learns a section
  exists — and the shader evaluates the identical `dot(normal, point) + offset > 0` test, so
  the CPU and GPU answers agree by construction rather than by agreement. Up to four at once.
  In the viewport: `X`/`Y`/`Z` cut through the model centre, `[`/`]` slide the cut, `C`
  clears, `F` re-frames. Headless: `--section z --png out.png`.
  - The pick pass clips too, so a click passes through sectioned-away geometry to whatever is
    behind it. Selecting something you cannot see would be worse than not selecting at all.
  - **Uncapped.** Back faces are culled, so a cut solid reads as a hole rather than a filled
    section. Capping needs a stencil pass and is not done.

### Fixed

- **`extrude_along` rotated the profile instead of shearing it.** It built an orthonormal
  basis around the sweep direction and re-based the profile onto it. That is not what
  `IfcExtrudedAreaSolid` means: the profile lies in the XY plane of `Position` and
  `ExtrudedDirection` is a vector in that same system, so an off-axis sweep gives an oblique
  prism and the profile never moves.

  For `+Z` — everything written before Phase 5 — the two are identical, which is why it
  survived. The existing test compared volume and surface area against a `+Z` sweep and
  called the difference "a rigid rotation", and both quantities are invariant under exactly
  the error it was meant to catch. It became visible the moment a slab swept downward: the
  basis completing a right-handed frame with `-Z` mirrors the profile in Y, and a 7 × 5 m
  slab landed alongside the room it was drawn in rather than underneath it. A sweep lying in
  the profile's own plane is now refused rather than returned as a zero-volume shell.
- `tools/validate_ifc.py` failed any file without a door. The structural, geometric, and
  round-trip checks apply to any CADForge export; the ones naming a door, an opening, or a
  volume describe the demo model, and now run only when the file contains them. Pointed at a
  hand-drawn room it used to report two failures and then crash.

- **Imported lengths ignored the file's units.** Real IFC is overwhelmingly in millimetres, and
  CADForge is metric-metres throughout, so a 3 m wall was being read as 3 km. Placements,
  profiles, extrusion depths, tessellated vertices, and `Length`/`Area`/`Volume` properties are
  now all scaled by the project's `IfcUnitAssignment` — areas by the square of the scale and
  volumes by the cube.

  This survived every unit test, the whole corpus round trip, and IfcOpenShell validation,
  because export and import shared the same wrong assumption and the corpus checks compared
  counts rather than sizes. It took ten seconds to spot once a real file was on screen and the
  scene reported *3000 metres across*. Now pinned by a test with an inline millimetre file.
- Two rustdoc links that broke a `-D warnings` documentation build, one of which was
  `cadforge-core` linking to a type in `cadforge-family` — a crate it deliberately does not
  depend on. The doc build now enforces that boundary.

## [0.2.0] — 2026-08-27

**CADForge can read IFC.** Phase 2b is complete, which closes the half of the product
objective that was missing: import, not just export.

### Added

- **`IfcLiteBackend`** — a full IFC reader over `ifc-lite-core`, behind the `read` feature
  (on by default). Products become `ElementRecord`s; `IfcRelVoidsElement`,
  `IfcRelFillsElement`, `IfcRelContainedInSpatialStructure`, `IfcRelDefinesByType`, and
  `IfcRelDefinesByProperties` are all rebuilt.
- Geometry import: `IfcExtrudedAreaSolid` (with `IfcArbitraryClosedProfileDef` and
  `IfcRectangleProfileDef`) stays **parametric**; `IfcTriangulatedFaceSet` and
  `IfcPolygonalFaceSet` come back as themselves. Nothing is silently re-meshed.
- `IfcLocalPlacement` chains are composed and re-expressed relative to the element's
  container, which is the only approach correct for both a two-deep file we wrote and a real
  file with assemblies nested inside storeys.
- Property import keeps measure types: `IFCLENGTHMEASURE(2.4)` comes back as `Length`, not as
  a bare real, because `ifc-lite` puts the type name in the typed-value list.
- `Placement::from_matrix` — recovering a rigid placement from a composed transform.
- `IfcClass::is_structure_spine()` — the four classes the exporter writes as structure, which
  is deliberately narrower than `is_spatial()`.
- **`tools/fetch_corpus.py`** — downloads buildingSMART's 23 certification models with
  checksums. Files are fetched, never committed.
- **`examples/corpus_survey`** and **`examples/corpus_import`** — what real files contain, and
  what happens when we read them.

### Verified

- **195 tests**, zero clippy warnings, `cargo fmt` clean.
- All 23 corpus files import: 984 elements, one geometry failure, zero dangling references.
- **All 23 survive import → export → import with every element intact.**
- The exporter still passes IfcOpenShell 0.8.5 validation after the spatial-structure rewrite.

### Fixed

Both found by pointing the importer at files CADForge did not write. Neither was reachable
from any test over its own output.

- **The exporter dropped `IfcSpace`, and every site or building past the first.** Spatial
  classes were skipped wholesale in the product loop while only `sites.first()` and
  `buildings.first()` were written as structure. `is_spatial()` and `is_structure_spine()` are
  now different questions: a space is spatial — things live in it — but it is written as a
  product.
- **Elements inside IFC4X3 infrastructure spatial parts were stranded.** `IfcRoadPart`,
  `IfcBridgePart`, and `IfcFacilityPart` arrive as `IfcClass::Other`, which was never spatial,
  so every `AssignContainer` against one was rejected. One road model alone produced 38
  dangling-reference warnings. `Other` is now spatial for a known list of entity names — a
  list rather than a prefix rule, because `IfcSpatialZone` is spatial and `IfcSpaceHeater`
  is not.

Together: **151 dangling references → 0**, and files surviving a full round trip went from
**5 of 23 to 23 of 23**.

### Known limitations

- Only buildingSMART's clean certification models have been tested. Vendor-exported
  architectural models — Revit, Archicad, Tekla — and genuinely broken coordination files are
  still unsourced, and remain the honest gap.
- An element contained in an `IfcSpace` re-exports into its storey, not the space. No element
  is lost; the containment is coarsened.
- `IfcAdvancedBrep`, `IfcFacetedBrep`, CSG, and mapped representations are not read. The
  element keeps its semantics and loses its shape, with a warning.

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

[Unreleased]: https://github.com/ibuilder/CADForge/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/ibuilder/CADForge/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ibuilder/CADForge/releases/tag/v0.1.0
