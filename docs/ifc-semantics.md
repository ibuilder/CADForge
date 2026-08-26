# Rust-First IFC Authoring Application

## 1. Product Objective

Build an IFC-native AEC/BIM authoring application that:

- Imports, views, queries, edits, validates, and exports IFC projects.
- Preserves IFC semantics, `GlobalId` identity, placements, relationships, type assignments, property sets, quantities, classifications, and material associations.
- Uses Rust for performance-critical model handling, geometry preparation, indexing, streaming, spatial queries, conflict detection, and client/WASM execution.
- Uses IfcOpenShell as the authoritative IFC interoperability and geometric conversion layer.
- Uses Truck only for a deliberately supported subset of parametric geometry authoring—not as the universal source of imported IFC geometry.
- Renders interactively using WebGPU, with WebGL2 compatibility where needed.
- Supports a plugin system for authoring tools, family libraries, BCF issues, digital-twin overlays, construction workflows, and future 4D/5D modules.

> Principle: IFC semantic data is authoritative. Render meshes and derived B-reps are disposable caches that can be regenerated.

***

## 2. Design Principles

1. **IFC-first, not mesh-first**
   - Persist building information in IFC-compatible semantic structures.
   - Never make a viewer mesh the canonical representation of a building element.

2. **Separate semantic, parametric, B-rep, and render layers**
   - An `IfcWall` and its relationships are semantics.
   - A profile plus extrusion is a parametric authoring recipe.
   - A B-rep is editable geometric topology.
   - A triangulated mesh is a render artifact.

3. **Use the best engine per concern**
   - IfcOpenShell: IFC schemas, import/export, conversion, validation, and broad compatibility.
   - Rust: hot-path data processing, storage, spatial indexes, meshing orchestration, WASM, and concurrency.
   - Truck: controlled B-rep/NURBS experiments and a bounded parametric authoring subset.
   - OpenCascade via IfcOpenShell: robust handling of diverse imported B-reps and difficult geometry.
   - WebGPU: interactive rendering, GPU picking, visibility, and advanced post-processing.

4. **Command-based authoring**
   - Model edits are durable domain commands, not arbitrary mesh mutations.
   - Commands can be replayed, undone, audited, synchronized, validated, and mapped to IFC.

5. **Capability-driven geometry**
   - Each authoring operation declares whether it can export as a native IFC parametric representation.
   - If not, use an explicit tessellated or B-rep fallback, annotate the degradation, and preserve the original command data.

***

## 3. System Architecture

```text
┌──────────────────────────────────────────────────────────────┐
│                         Client Application                   │
│  React / TypeScript UI                                        │
│  Tool system · property inspector · outliner · issue panel   │
├──────────────────────────────────────────────────────────────┤
│ Rendering                                                     │
│  Three.js / WebGPURenderer                                    │
│  WebGPU primary · WebGL2 compatibility backend               │
│  GPU picking · render passes · LOD · instancing               │
├──────────────────────────────────────────────────────────────┤
│ Rust/WASM Core                                                │
│  Model query cache · spatial index · mesh cache · BVH         │
│  IFC fragment streaming · selection mapping · worker tasks    │
├──────────────────────────────────────────────────────────────┤
│ API / Domain Services                                         │
│  Rust service boundary or Python worker layer                 │
│  Command validation · authorization · revisioning             │
├──────────────────────────────────────────────────────────────┤
│ IFC Interoperability                                          │
│  IfcOpenShell C++ / Python                                    │
│  IFC parsing · geometry conversion · export · validation      │
├──────────────────────────────────────────────────────────────┤
│ Geometry Services                                             │
│  Truck: controlled procedural/B-rep authoring                 │
│  OpenCascade: imported B-reps, healing, advanced booleans     │
├──────────────────────────────────────────────────────────────┤
│ Persistence                                                   │
│  PostgreSQL · object storage · model/event revisions          │
│  IFC snapshots · render artifacts · geometry cache            │
└──────────────────────────────────────────────────────────────┘
```

***

## 4. Major Components

### 4.1 IFC Semantic Core

Responsibility:

- Store, inspect, edit, and export IFC object graphs.
- Preserve native entity IDs and stable `GlobalId` values.
- Manage ownership history, units, coordinate systems, placements, and schema-version metadata.
- Support IFC2X3 and IFC4 first; add IFC4.3/IFC5 only through explicit compatibility work.

Core entity coverage:

```text
IfcProject
IfcSite
IfcBuilding
IfcBuildingStorey
IfcSpace
IfcElementAssembly
IfcWall / IfcWallStandardCase
IfcSlab
IfcRoof
IfcColumn
IfcBeam
IfcDoor
IfcWindow
IfcStair
IfcCovering
IfcFurniture
IfcBuildingElementProxy
IfcTypeObject
IfcPropertySet
IfcRelDefinesByProperties
IfcRelContainedInSpatialStructure
IfcRelAggregates
IfcRelVoidsElement
IfcRelFillsElement
IfcRelAssociatesMaterial
IfcRelDefinesByType
```

Rules:

- Never use display names as keys.
- Use `GlobalId` for cross-system element identity.
- Keep `IfcRoot.Name` and `ObjectType` as editable user-facing metadata.
- Store application-specific state outside IFC where possible, or in versioned, namespaced property sets when required.

### 4.2 Rust Model Core

Responsibility:

- Provide a fast read model tailored to UI, rendering, search, selection, and spatial workflows.
- Avoid forcing every interactive request through Python object graphs or raw IFC STEP text.
- Treat IFC source files and semantic transactions as source data; derive Rust-native indexes and projections.

Suggested crates:

```toml
[dependencies]
anyhow = "1"
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["serde", "v4"] }
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
dashmap = "6"
rayon = "1"
slotmap = "1"
petgraph = "0.7"
glam = { version = "0.29", features = ["serde"] }
nalgebra = "0.33"
rstar = "0.12"
parry3d = "0.18"
```

Suggested internal records:

```rust
use glam::{DMat4, DVec3};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementId {
    pub global_id: String,
    pub ifc_step_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementRecord {
    pub id: ElementId,
    pub ifc_class: String,
    pub name: Option<String>,
    pub object_type: Option<String>,
    pub placement: DMat4,
    pub spatial_container: Option<String>,
    pub type_global_id: Option<String>,
    pub representation_revision: u64,
    pub semantic_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub min: DVec3,
    pub max: DVec3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderFragment {
    pub fragment_id: Uuid,
    pub element_global_id: String,
    pub geometry_hash: String,
    pub material_key: String,
    pub bounds: BoundingBox,
    pub vertex_count: u32,
    pub index_count: u32,
}
```

### 4.3 Geometry Authority Model

Use these explicit geometry categories:

| Category | Canonical representation | Primary engine | Typical IFC output |
|---|---|---|---|
| Existing imported IFC geometry | Original IFC representation | IfcOpenShell/OpenCascade | Preserve original whenever possible |
| New wall/slab/profile extrusion | Command + profile parameters | Rust + Truck optional | `IfcExtrudedAreaSolid` |
| New swept object | Command + path + profile | Rust + Truck optional | `IfcSweptDiskSolid` or swept area representation |
| Opening/void | Boolean command referencing host and cutter | OpenCascade preferred | `IfcRelVoidsElement` + opening representation |
| Generic complex solid | B-rep or tessellated derived representation | OpenCascade / Truck experiment | `IfcFacetedBrep`, advanced B-rep, or tessellation |
| Visualization-only geometry | Triangles / meshlets / LODs | Rust/WASM | Never authoritative |

### 4.4 IfcOpenShell Service

Expose IfcOpenShell behind a durable service boundary.

Recommended options:

- Python worker process with typed job contracts.
- C++ service with gRPC, HTTP, ZeroMQ, NATS, or local IPC.
- Rust orchestrator invoking a long-running Python worker pool.
- Native FFI only after profiling proves process boundaries are unacceptable.

Initial service responsibilities:

```text
parse_ifc
inspect_schema
extract_semantic_projection
generate_mesh_fragments
generate_native_brep
validate_ifc
write_ifc
apply_authoring_commands
run_ifcpatch_operation
convert_ifc
diff_ifc_revisions
```

Example typed request:

```json
{
  "request_id": "2c4205f2-5e9b-4f3e-874f-bff113c5dd62",
  "operation": "generate_mesh_fragments",
  "model_revision": "sha256:abc123",
  "settings": {
    "use_world_coords": false,
    "deflection_tolerance": 0.001,
    "angular_tolerance": 0.5,
    "include_normals": true,
    "include_materials": true
  }
}
```

### 4.5 Truck Geometry Adapter

Truck should not be a transparent replacement for OpenCascade.

Create a bounded adapter with explicit supported operations:

```rust
pub trait AuthoringGeometryKernel {
    fn create_extrusion(
        &self,
        profile: ClosedProfile,
        direction: glam::DVec3,
        depth: f64,
    ) -> Result<AuthoringShape, GeometryError>;

    fn create_sweep(
        &self,
        profile: ClosedProfile,
        path: Curve3d,
    ) -> Result<AuthoringShape, GeometryError>;

    fn transform(
        &self,
        shape: &AuthoringShape,
        transform: glam::DMat4,
    ) -> Result<AuthoringShape, GeometryError>;

    fn tessellate(
        &self,
        shape: &AuthoringShape,
        quality: TessellationSettings,
    ) -> Result<IndexedMesh, GeometryError>;
}
```

Support initially:

- Closed 2D profiles.
- Rectangles, circles, arcs, polylines, and simple B-spline profiles.
- Linear extrusions.
- Controlled sweeps.
- Transformations.
- Tessellation.
- Simple validation such as closed-wire checks and degenerate-area rejection.

Defer initially:

- Arbitrary topology repair.
- Unbounded imported NURBS trimming conversion.
- General B-rep healing.
- Complex booleans on externally authored solids.
- Lossless OpenCascade B-rep round trips.
- Revit-style family solver parity.

### 4.6 Render Pipeline

Rendering should use derived fragments, never raw IFC entities directly.

```text
IFC representation
  → IfcOpenShell geometry conversion
  → normalized fragment package
  → Rust geometry/cache pipeline
  → GPU vertex/index/instance buffers
  → WebGPU render passes
```

Each render fragment must retain:

```text
fragment_id
element_global_id
ifc_step_id
representation_id
geometry_hash
material_key
local_transform
bounding_box
lod_level
source_revision
```

Essential viewer features:

- GPU color-ID or integer-ID picking.
- Frustum culling.
- Hierarchical bounding boxes.
- Per-storey, per-system, per-class, and per-selection visibility.
- Instancing for repeated geometry.
- Geometry cache keyed by normalized representation hash.
- Material batching.
- LOD for high-density models.
- Selection outlines and transparency without duplicating canonical geometry.
- Section-box and clipping-plane support.

***

## 5. Domain Command Model

Every edit must be an explicit command.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelCommand {
    CreateWall(CreateWall),
    CreateSlab(CreateSlab),
    CreateOpening(CreateOpening),
    MoveElement(MoveElement),
    SetName(SetName),
    SetProperty(SetProperty),
    AssignType(AssignType),
    AssignMaterial(AssignMaterial),
    AssignSpatialContainer(AssignSpatialContainer),
    DeleteElement(DeleteElement),
}
```

Example wall command:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWall {
    pub command_id: Uuid,
    pub project_id: Uuid,
    pub parent_storey_global_id: String,
    pub global_id: Option<String>,
    pub name: String,
    pub axis: Vec<[f64; 3]>,
    pub thickness_m: f64,
    pub height_m: f64,
    pub base_elevation_m: f64,
    pub material_global_id: Option<String>,
    pub type_global_id: Option<String>,
}
```

Expected execution sequence:

```text
UI tool creates command
  → client validates basic inputs
  → API authorizes user and locks/rebases target revision
  → command validator checks IFC and geometric constraints
  → semantic transaction creates/updates IFC entities
  → geometry worker regenerates affected representations
  → Rust cache updates fragments and spatial index
  → client receives model revision and incremental patch
```

Command requirements:

- `command_id` for idempotency.
- `author_id`, model revision, timestamp, and provenance.
- Undo/redo inverse or compensating commands.
- Permission checks.
- Deterministic replay where possible.
- Structured validation results.
- Incremental invalidation of only affected geometry and relationships.

***

## 6. IFC Authoring Mapping

### 6.1 Wall Creation

For a native editable wall:

```text
IfcWall
  ├─ IfcLocalPlacement
  ├─ IfcProductDefinitionShape
  │   └─ IfcShapeRepresentation
  │       └─ IfcExtrudedAreaSolid
  │           └─ IfcArbitraryClosedProfileDef
  ├─ IfcRelContainedInSpatialStructure → IfcBuildingStorey
  ├─ IfcRelAssociatesMaterial → IfcMaterial / IfcMaterialLayerSetUsage
  ├─ IfcRelDefinesByType → IfcWallType
  └─ IfcRelDefinesByProperties → IfcPropertySet
```

For openings:

```text
IfcOpeningElement
  ├─ IfcProductDefinitionShape
  └─ IfcRelVoidsElement → host wall

IfcDoor / IfcWindow
  └─ IfcRelFillsElement → opening
```

Do not represent a door opening only by subtracting triangles from a wall mesh.

### 6.2 Geometry Fallback Rules

When native parametric output is impossible:

1. Attempt an IFC advanced B-rep representation only if validation and downstream compatibility are acceptable.
2. Otherwise generate an `IfcTriangulatedFaceSet` or another well-supported tessellated representation.
3. Preserve source command metadata in a namespaced property set or external authoring database.
4. Mark the element as a degraded geometry representation in the UI and validation report.
5. Maintain the option to regenerate a native parametric representation later.

***

## 7. Data Model and Persistence

### 7.1 Authoritative Storage

| Data type | Recommended store |
|---|---|
| Model/project metadata | PostgreSQL |
| Commands and audit events | PostgreSQL event table |
| IFC snapshots | Versioned object storage |
| Original uploaded IFC files | Immutable object storage |
| Derived render fragments | Object storage or content-addressed cache |
| Geometry/BVH/index cache | Redis, disk cache, or object storage |
| Thumbnails and drawings | Object storage |
| BCF issues and viewpoints | PostgreSQL + object storage |
| Search index | PostgreSQL FTS initially; dedicated index only when justified |

### 7.2 Revision Model

```text
Project
  └─ Model
      └─ Revision
          ├─ Parent revision
          ├─ IFC snapshot hash
          ├─ Command range
          ├─ Semantic projection hash
          ├─ Geometry fragment manifest hash
          ├─ Validation report
          └─ Author / time / change summary
```

Rules:

- IFC exports must be reproducible from a revision.
- Geometry caches can be dropped and regenerated.
- Commands must not depend on transient renderer IDs.
- Maintain content hashes for IFC snapshots, fragment manifests, and derived meshes.
- Store original source IFC separately from normalized or authored revisions.

***

## 8. API Design

### 8.1 Initial Endpoints

```text
POST   /projects
GET    /projects/{project_id}
POST   /models/import
GET    /models/{model_id}
GET    /models/{model_id}/revisions
GET    /models/{model_id}/revisions/{revision_id}/ifc
GET    /models/{model_id}/elements
GET    /models/{model_id}/elements/{global_id}
GET    /models/{model_id}/elements/{global_id}/properties
GET    /models/{model_id}/fragments
POST   /models/{model_id}/commands
POST   /models/{model_id}/validate
POST   /models/{model_id}/export
GET    /models/{model_id}/issues
POST   /models/{model_id}/issues
```

### 8.2 Command Response

```json
{
  "command_id": "05a8a24b-74ee-4098-aa31-6dd6c52a01f3",
  "status": "accepted",
  "base_revision": 41,
  "result_revision": 42,
  "changed_global_ids": [
    "2M8Y$5OyL9YeYwrNOYPHhD"
  ],
  "invalidated_fragment_ids": [
    "bef2789a-f9a0-4552-9afb-bdf7185aae38"
  ],
  "validation": {
    "status": "passed",
    "warnings": []
  }
}
```

***

## 9. Plugin Architecture

Plugins must not access IFC internals or GPU resources without controlled interfaces.

```rust
pub trait BimPlugin {
    fn manifest(&self) -> PluginManifest;
    fn register_commands(&self, registry: &mut CommandRegistry);
    fn register_panels(&self, registry: &mut UiRegistry);
    fn register_validators(&self, registry: &mut ValidationRegistry);
    fn register_render_overlays(&self, registry: &mut RenderOverlayRegistry);
}
```

Initial plugin categories:

- Element authoring tools.
- Family/type-library manager.
- IFC property-set templates.
- Classification mapper.
- BCF issue management.
- Clash detection.
- Quantity takeoff.
- Construction sequencing.
- Digital-twin and IoT overlays.
- Drone/photogrammetry alignment.
- Drawing and sheet generation.
- Rules, IDS, and compliance checking.

Plugin safety rules:

- Plugin commands must use the same transaction, authorization, and revision system as core commands.
- Plugins cannot directly mutate rendered vertex buffers as a persistent model edit.
- Plugins receive capability-scoped access to projects, models, elements, and files.
- Plugins must declare versioned schema migrations for their own data.
- Use an isolated worker/runtime for untrusted or third-party plugins.

***

## 10. Repository Layout

```text
massing/
├── apps/
│   ├── web/                      # React/TypeScript authoring UI
│   ├── api/                      # API gateway/service
│   └── worker/                   # Geometry, IFC, conversion jobs
├── crates/
│   ├── bim-domain/               # Command model and semantic projections
│   ├── bim-ifc/                  # IFC identifiers, adapters, DTOs
│   ├── bim-geometry/             # Geometry abstractions and mesh types
│   ├── bim-truck/                # Bounded Truck integration
│   ├── bim-spatial/              # BVH, R-tree, clash broad phase
│   ├── bim-fragments/            # Render-fragment cache/manifest
│   ├── bim-validation/           # Domain/IFC validation adapters
│   ├── bim-events/               # Event/revision model
│   ├── bim-plugin-api/           # Stable plugin traits and DTOs
│   └── bim-wasm/                 # Browser bindings and workers
├── services/
│   └── ifcopenshell-worker/      # Python/C++ IFC service
├── packages/
│   ├── viewer/                   # WebGPU/Three.js rendering layer
│   ├── ui/                       # Shared UI design system
│   ├── protocol/                 # OpenAPI/Protobuf/Zod contracts
│   └── plugins/                  # First-party plugins
├── infra/
│   ├── docker/
│   ├── kubernetes/
│   └── terraform/
├── docs/
│   ├── architecture/
│   ├── ifc-mapping/
│   ├── adr/
│   └── test-corpus/
└── tests/
    ├── integration/
    ├── fixtures/
    ├── interoperability/
    └── performance/
```

***

## 11. Development Phases

### Phase 0 — Technical spikes

Deliverables:

- Import three representative IFC files: small residential, mid-size commercial, and difficult consultant-exported model.
- Produce an inventory of classes, representation types, geometry failures, units, placements, and duplicate `GlobalId` problems.
- Benchmark IfcOpenShell geometry extraction.
- Benchmark Rust/WASM mesh decoding, BVH building, and GPU upload.
- Prototype Truck extrusion generation and export mapping to `IfcExtrudedAreaSolid`.
- Confirm WebGPU and WebGL2 fallback performance using the target browser/device matrix.

Exit criteria:

- A written compatibility matrix.
- A validated project coordinate/units policy.
- A representative IFC regression corpus.
- Performance budgets for initial load, time-to-first-model, memory, frame time, and command latency.

### Phase 1 — Read-only IFC viewer

Features:

- IFC upload and job-based import.
- Semantic hierarchy: project, site, building, storey, spaces, elements.
- Fragmented mesh loading.
- Class/storey/system visibility.
- Selection, isolate, hide, and property panel.
- GPU picking.
- Search by `GlobalId`, name, IFC class, and property.
- Camera controls, clipping planes, section box, and measurements.
- IFC export of original source and normalized read-only snapshot.

Exit criteria:

- Large-file progressive loading.
- Stable selection mapping from pixels to `GlobalId`.
- Reproducible fragment cache generation.
- No semantic mutation through viewer interactions.

### Phase 2 — Safe metadata authoring

Features:

- Rename elements.
- Set properties and quantities.
- Type assignments.
- Material associations.
- Classification associations.
- Spatial containment reassignment.
- BCF issue creation with viewpoints and selected element references.
- Revision history, audit trail, undo/redo, and validation report.

Exit criteria:

- Every edit produces a valid command and revision.
- Exported IFC is re-importable by IfcOpenShell.
- Round-trip tests preserve edited metadata and `GlobalId` identity.

### Phase 3 — Native parametric elements

Start with a small, reliable element set:

- Wall.
- Slab.
- Column.
- Beam.
- Opening.
- Door/window placement in an opening.
- Generic profile extrusion.

Features:

- Level-aware placement.
- Snapping and constraints.
- Profile editing.
- Length, height, thickness, elevation, and material/type editing.
- IFC-native parametric representation output.
- Opening and host relationship management.
- Incremental mesh regeneration.

Exit criteria:

- New elements export as interoperable IFC-native parametric forms.
- Re-import produces equivalent semantics and visually equivalent geometry.
- Deletions and moves do not orphan IFC relationships.

### Phase 4 — Controlled Truck integration

Features:

- Truck-backed preview for supported profile and sweep tools.
- Rust tessellation pipeline.
- Geometry-command validation.
- Controlled boolean experiments.
- Translation of supported shapes to IFC parametric forms.
- Explicit fallback behavior for unsupported operations.

Exit criteria:

- Truck is used only where an operation has verified IFC mapping.
- Every supported Truck operation has golden-file and round-trip test coverage.
- Unsupported geometry fails clearly rather than silently corrupting model data.

### Phase 5 — Advanced BIM workflows

Features:

- Family/type authoring and reusable libraries.
- Rules and IDS validation.
- Clash detection with broad-phase Rust spatial index.
- Quantities and cost links.
- Construction tasks and 4D sequences.
- BCF API support.
- Federated models and links.
- Digital-twin sensor overlays.
- Point clouds, photogrammetry, Gaussian splats, and surveyed-model alignment.

***

## 12. Testing Strategy

### 12.1 IFC Interoperability

Maintain a versioned corpus containing:

- IFC2X3 and IFC4 files.
- Native exports from Revit, Archicad, Tekla, Vectorworks, Allplan, FreeCAD, Blender/Bonsai, Solibri, and consultant coordination models.
- Models with extrusions, sweeps, CSG, mapped items, B-reps, tessellation, openings, profiles, materials, properties, classifications, and georeferencing.
- Intentionally malformed IFC files.
- Large federated models.

Required checks:

```text
parse
semantic projection
geometry generation
native B-rep generation where applicable
command application
IFC export
re-import
property/relationship comparison
GlobalId preservation
geometry bounds comparison
validation report
```

### 12.2 Golden Tests

For every authoring command:

```text
input model + command
  → expected IFC entities / relationships
  → expected normalized semantic projection
  → expected fragment manifest
  → expected validation result
```

### 12.3 Geometry Tests

Test:

- Profile closure.
- Winding direction.
- Unit conversion.
- Local and world placement composition.
- Floating-point tolerance behavior.
- Zero-area faces.
- Non-manifold results.
- Openings crossing host boundaries.
- Self-intersections.
- Degenerate or inverted transforms.
- Tessellation consistency and normal orientation.

### 12.4 Performance Tests

Track:

| Metric | Initial target |
|---|---:|
| Time to show a coarse model preview | Under 5 seconds for a normal project |
| First interactive selection | Under 10 seconds for a normal project |
| Camera frame budget | 16.7 ms at 60 FPS for active viewport workloads |
| Selection/picking response | Under 100 ms |
| Property edit acknowledgement | Under 250 ms before background geometry work |
| Incremental element mesh rebuild | Under 1 second for simple authored elements |
| Model command mutation | Under 500 ms excluding long geometry tasks |

Use representative hardware tiers, not only development workstations.

***

## 13. Security and Reliability

- Authorize every project, model, revision, issue, and element mutation.
- Keep uploads in isolated object storage and scan/validate files before worker processing.
- Run IfcOpenShell and geometry conversion in resource-limited workers.
- Enforce file-size, entity-count, recursion-depth, geometry-count, and processing-time limits.
- Treat IFC files as untrusted input.
- Record provenance for uploaded source, converted model, and user-authored changes.
- Sign or hash immutable snapshots and exported deliverables.
- Use idempotency keys for all mutation requests.
- Add job queues, retries, dead-letter handling, cancellation, and worker health checks.
- Capture tracing across API, command execution, IFC worker, and geometry worker.
- Back up PostgreSQL, object storage manifests, and command/event records.
- Test restore procedures, not only backup completion.

***

## 14. Key Architectural Decisions

### ADR-001: IFC Is the Exchange and Semantic Authority

**Decision:** Preserve IFC entity semantics and relationships as canonical building data.

**Consequences:**

- Viewer meshes are caches.
- Export is a first-class system behavior.
- Semantic edits occur through an IFC-aware command pipeline.
- Imported models retain their original source data and provenance.

### ADR-002: Rust Owns Performance-Critical Derived Data

**Decision:** Rust manages fragment processing, spatial indexing, cache generation, concurrency, and WASM/browser computation.

**Consequences:**

- High-throughput geometry work does not require Python in the interactive path.
- Semantic correctness still depends on IfcOpenShell-backed workflows.
- Rust projections must be versioned and reproducible from canonical revisions.

### ADR-003: Truck Is a Controlled Geometry Kernel

**Decision:** Use Truck for a supported authoring subset, not arbitrary IFC B-rep round trips.

**Consequences:**

- New authored geometry can benefit from Rust-native workflows.
- Unsupported shapes use IfcOpenShell/OpenCascade or explicit tessellation fallback.
- Every Truck operation requires a tested IFC representation mapping.

### ADR-004: WebGPU Is Preferred, WebGL2 Is a Compatibility Path

**Decision:** Build a rendering abstraction that supports WebGPU where available and WebGL2-compatible operation where necessary.

**Consequences:**

- Avoid permanently coupling semantic state to a specific graphics backend.
- Compute-dependent features require capability detection and fallback behavior.
- The renderer is replaceable without changing the IFC domain core.

***

## 15. First 90-Day Backlog

### Weeks 1–2

- Create IFC interoperability corpus and importer benchmark harness.
- Define model revision, command envelope, and element projection schemas.
- Add geometry-worker contracts for parse, extract, tessellate, validate, and export.
- Implement initial Rust `ElementRecord`, `RenderFragment`, and spatial-index structures.
- Establish coordinate, unit, precision, and georeferencing policy.

### Weeks 3–4

- Build IFC upload, import job, semantic hierarchy, and fragment-manifest pipeline.
- Build WebGPU/WebGL2 viewer shell with selection and `GlobalId` lookup.
- Implement property inspector and element search.
- Add observability for import time, entity counts, geometry failures, mesh sizes, GPU upload time, and memory use.

### Weeks 5–6

- Implement revisions, command log, audit history, and safe property-set edits.
- Add export and re-import golden tests.
- Add BCF issue records, selected-element links, and camera viewpoints.
- Implement spatial containment, type, material, and classification assignments.

### Weeks 7–8

- Implement native wall and slab commands with IFC-compatible parametric output.
- Implement element transforms, storey assignment, and incremental fragment invalidation.
- Implement simple opening creation using correct host/void relationships.
- Establish wall/slab/opening round-trip fixtures.

### Weeks 9–10

- Prototype Truck-backed profile extrusion as a preview/tessellation component.
- Compare Truck output with IfcOpenShell/OpenCascade output for supported primitives.
- Define supported-geometry capability flags and explicit fallback paths.
- Add regression tests for closed profiles, transformed extrusions, and unit conversion.

### Weeks 11–12

- Implement family/type prototype, permissions, job reliability, backups, and restore checks.
- Add automated interoperability testing in CI.
- Publish architecture decision records and plugin SDK draft.
- Review performance budgets against the representative IFC corpus.

***

## 16. Definition of Done

An authoring feature is complete only when it:

- Has a typed command and permission rule.
- Produces an auditable model revision.
- Has unit and integration tests.
- Has an IFC mapping document.
- Preserves or deliberately creates stable `GlobalId` identity.
- Regenerates only necessary derived geometry.
- Exports a valid IFC file.
- Re-imports with expected semantics intact.
- Handles unsupported geometry explicitly.
- Has observability for duration, failure, and output size.
- Works with WebGPU and documented WebGL2 fallback behavior where applicable.
