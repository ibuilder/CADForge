# CADForge — Build Plan

**A native, cross-platform, IFC-native BIM authoring application in Rust, with a first-class family system.**

Targets: **Windows, macOS, Android, iOS.** Plan written 2026-08-25 against the findings in [LANDSCAPE.md](research/LANDSCAPE.md). Read that first — this document assumes it.

***

## 0. The one-paragraph version

Every serious new BIM authoring tool in 2026 (Motif, Arcol, Qonic, Snaptrude, Forma) is **cloud/browser-first and closed**. Every open one (Bonsai, FreeCAD, the OpenAEC catalog) is **desktop-only**. Nobody ships a native, offline-capable, openBIM authoring app that runs on a phone or tablet as well as a workstation. Shapr3D proved that posture works commercially in mechanical CAD; nobody has done it for AEC. That is the lane. The hard technical constraint discovered in research — **WebGPU does not exist in any mobile webview** — decides the architecture: the viewport is a **native wgpu surface**, not a webview canvas. The differentiator is the **family system**, because a BIM authoring tool without reusable parametric components is a mesh editor with extra steps.

## 1. Scope decision, stated plainly

You asked for Tauri. **I am recommending against Tauri for the 3D viewport, and the reason is a hard external constraint, not a preference.**

WebGPU is not available in WKWebView (macOS *and* iOS) or Android System WebView. A Tauri app that draws 3D in its webview is therefore **capped at WebGL2 on three of your four target platforms** — not on old hardware, on current hardware. And compositing a native GPU surface underneath a Tauri webview, which would be the escape hatch, is explicitly unsupported on mobile (`tauri#8246`, discussions #10964/#11944).

Writing a CAD kernel in Rust and then rendering it through WebGL2 on an iPad forfeits the point of the exercise.

**What I am building instead:** a single Rust binary — `winit` + `wgpu` + `egui` — that runs natively on all four platforms with Metal on Apple, Vulkan on Android, DX12 on Windows. Same renderer everywhere. No webview in the hot path.

**What this does not close off:** the shell is one crate (`cadforge-shell`) behind a trait. A Tauri desktop shell over the identical core crates remains a ~2-week add whenever there is a reason (rich document/report panels, web deployment, reusing a web design system). ADR-0002 records this so the decision can be revisited without re-litigating it. If you want Tauri anyway — say so and I will build it; the core work is unchanged either way, and only `cadforge-shell` differs.

## 2. What CADForge is, and what it is not

**Is:**
- Native IFC authoring — the semantic model is authoritative, meshes are caches (this carries over unchanged from [ifc-semantics.md](ifc-semantics.md) §2 and ADR-001, which remain correct).
- Cross-platform *including tablets and phones*, with parity in the model and honest asymmetry in the tooling (you do not draft on a phone; you review, redline, measure, and place components).
- **Family-centric.** Parametric, versioned, reusable components are the core object, not an afterthought.
- Offline-capable. Sync is a feature, not a dependency.

**Is not:**
- Not a DWG/DXF drafting tool — Open CAD Studio already does that, in Rust, well. Interop with it rather than duplicating it.
- Not another IFC viewer — Monty, ifc-lite, and a dozen others exist.
- Not a browser app. That lane is full and funded.
- Not a B-rep kernel project. **Fornjot died trying**; see §4.

## 3. Architecture

```text
┌───────────────────────────────────────────────────────────────┐
│  cadforge-shell            per-platform entry points          │
│  winit window · egui UI · input · file dialogs · lifecycle    │
│  win/mac: desktop binary   android: NativeActivity  ios: UIKit│
├───────────────────────────────────────────────────────────────┤
│  cadforge-render           wgpu 30 · Metal/Vulkan/DX12        │
│  fragment buffers · instancing · GPU pick · culling · LOD     │
│  section planes · outlines · transparency · camera            │
├───────────────────────────────────────────────────────────────┤
│  cadforge-family           THE DIFFERENTIATOR                 │
│  ParamDef · FamilyType · GeometryRecipe DAG · HostBehavior    │
│  flexing/solve · 2D symbol · IFC type mapping · versioning    │
├───────────────────────────────────────────────────────────────┤
│  cadforge-geom             pure-Rust, no C++ toolchain        │
│  profiles · sweeps · extrusions · transforms · tessellation   │
│  CSG boolean via trait → ifc-lite exact kernel / truck / stub │
├───────────────────────────────────────────────────────────────┤
│  cadforge-core             semantic authority                 │
│  ElementRecord · GlobalId · ModelCommand · Revision · undo    │
│  spatial index (rstar) · relationships · property sets        │
├───────────────────────────────────────────────────────────────┤
│  cadforge-ifc              trait IfcBackend — no type leakage │
│  ifc-lite (default) │ IfcOpenShell subprocess (desktop only)  │
│  IFC2X3 · IFC4 · IFC4X3 readers/writers · IFCX serializer     │
├───────────────────────────────────────────────────────────────┤
│  cadforge-import           offline ingest, desktop-side       │
│  .rfa via rvt-rs │ .gsm via LP_XMLConverter+GDL │ glTF │ IFC  │
└───────────────────────────────────────────────────────────────┘
```

Non-negotiable boundaries:

1. **`cadforge-core` depends on nothing platform-specific and nothing IFC-library-specific.** It is pure domain. It compiles for every target and for tests without a GPU.
2. **No third-party IFC type ever appears in a `cadforge-core` signature.** `ifc-lite` is young (1,157 downloads/month, 5 dependent crates); the trait boundary is what makes replacing it a one-crate job.
3. **The renderer never owns truth.** It consumes `RenderFragment`s and returns picks. Delete every fragment and the model is intact.
4. **Every edit is a `ModelCommand`.** No path mutates the store directly. This is what makes undo, audit, replay, and eventual multi-user sync possible rather than retrofitted.

## 4. The geometry strategy — and why there is no kernel here

Research finding, restated because it governs everything downstream: **there is no production-grade pure-Rust B-rep kernel in 2026.** Fornjot is formally dead ("no longer in development… its goals were not reached"). `opencascade-rs` is a WIP C++ binding needing CMake, which is not even installed on this machine. `truck` is genuinely maintained (1,533 stars, pushed 2026-08-24) but its **crates.io releases are two years stale** — `truck-modeling` 0.6.0 was published 2024-09-20 — so consuming it means a git dependency and accepting an unpinned surface.

**So CADForge does not build on a B-rep kernel.** It builds on a **parametric recipe + tessellation** model:

- A wall is a closed profile swept along an axis. A slab is a profile extruded down. A column, a beam, a duct, a pipe, a railing — all sweeps.
- The **recipe is canonical and exports as native IFC parametric geometry** (`IfcExtrudedAreaSolid` + `IfcArbitraryClosedProfileDef`). The mesh is derived and disposable.
- Booleans (openings, joins) go through `trait CsgBackend`. A pure-Rust BSP implementation ships today ([ADR-0008](adr/0008-bsp-csg-backend.md)); `ifc-lite`'s exact-arithmetic kernel — verified against IfcOpenShell at 99.9%+ agreement — replaces it behind the same trait once measured.
- When an operation cannot be expressed as a recipe, it degrades **explicitly**: tessellated representation, flagged in the UI and validation report, original command data preserved so it can be regenerated parametrically later. (ifc-semantics.md §6.2 already specifies this correctly.)

This covers the overwhelming majority of real building elements and takes the project's largest technical risk off the critical path.

## 5. The family system — the actual differentiator

This is the part nothing else in the open AEC space has, and it is where the effort should concentrate.

### 5.1 Model

```rust
FamilyDefinition {
    id, name, version,
    category:   IfcClass,                  // IfcDoor, IfcWindow, IfcColumn…
    parameters: Vec<ParamDef>,             // typed, constrained, type-or-instance scoped
    types:      Vec<FamilyType>,           // Revit-style named types = param overrides
    recipe:     GeometryRecipe,            // deterministic op DAG
    host:       HostBehavior,              // Free | LevelHosted | WallHosted | FaceHosted
    symbol2d:   Option<Symbol2d>,          // plan/elevation representation
    ifc_mapping: IfcTypeMapping,           // entity + predefined type + psets
}
```

`GeometryRecipe` is a small deterministic DAG — `Profile`, `Extrude`, `Revolve`, `Sweep`, `Boolean`, `Transform`, `Array`, `Mirror`. Deterministic evaluation is what buys reproducible export, cacheable meshes, and golden-file testing. Ops that map 1:1 onto IFC representation types export natively; the rest fall back per §4.

`HostBehavior` is what makes a door a door rather than a box: a wall-hosted family cuts its host via `IfcRelVoidsElement` and fills the opening via `IfcRelFillsElement`, and it moves when its host moves.

### 5.2 Ingest — honest capability per source

The requirement was "families like Revit, Blender, Archicad". Here is what each actually yields:

| Source | Mechanism | What you truly get |
|---|---|---|
| **Revit `.rfa`** | `rvt-rs` (Apache-2.0, Rust, 11 Revit releases) + IFC export | **Parameters, types, type catalogs, metadata, preview — yes. Parametric geometry — no.** Revit's geometry and constraint solver are closed and unreverse-engineered. Geometry arrives via IFC export from Revit; CADForge rebuilds the recipe. **`rvt-rs` has 12 stars and one author — vendor it, keep it off the critical path.** |
| **Archicad `.gsm`** | `LP_XMLConverter l2x` → HSF/XML → GDL subset interpreter | **The best parametric path of the three.** GDL is a documented scripting language with real parameters, and Graphisoft permits third-party tooling. Needs an Archicad install to run the converter → desktop-side offline ingest, not a runtime feature. |
| **Blender** | glTF, or **Bonsai**'s IFC output | Mesh families via glTF trivially. For parametric, go through Bonsai — it already stores parametric definitions in IFC data. **Do not write a `.blend` parser.** |
| **IFC** | `IfcTypeProduct` + `IfcRelDefinesByType` + representation maps | **The native family format.** Everything above normalizes into this. |

Stating this plainly up front matters: "import Revit families" cannot mean "get parametric geometry from `.rfa`". It means parameters and types from `.rfa`, geometry from IFC, recipe rebuilt in CADForge.

## 6. Platform strategy

| | Windows | macOS | Android | iOS |
|---|---|---|---|---|
| Backend | DX12 | Metal | Vulkan | Metal |
| Shell | winit desktop | winit desktop | `android-activity` | UIKit + winit |
| Priority | **P0** | **P1** | P2 | P2 |
| Role | Full authoring | Full authoring | Review, redline, measure, place | Review, redline, measure, place |

**Parity is in the model, not the toolbar.** The same IFC, same families, same commands, same file. Phone-sized authoring UI is a research problem nobody has solved; do not pretend otherwise in v1. Mobile ships as a genuinely excellent *review and light-placement* client, which is already better than anything openBIM has.

Desktop-only, by necessity: `.rfa`/`.gsm` ingest (needs vendor tooling), IfcOpenShell subprocess backend, 500 MB+ federated models.

## 7. Phases

Phase 0 is complete — this document and [LANDSCAPE.md](research/LANDSCAPE.md) are its output, alongside the workspace scaffold in §8.

| Phase | Goal | Exit criteria |
|---|---|---|
| **0 — Foundation** ✅ | Research, decisions, compiling workspace | Workspace builds, tests pass, ADRs recorded |
| **1 — Core model** | `cadforge-core` real: elements, commands, revisions, undo, spatial index | Round-trip property test: N random commands → apply → invert → original state. 10k elements indexed and queried under 10 ms |
| **2a — IFC out** ✅ | Native IFC4 STEP writer ([ADR-0007](adr/0007-own-the-ifc-writer.md)) | **Validated against IfcOpenShell 0.8.5** — no EXPRESS violations, all relationships resolve, geometry generates, GlobalIds survive a round trip |
| **2b — IFC in** ✅ | `IfcBackend` over `ifc-lite` ([ADR-0009](adr/0009-ifc-lite-as-the-read-backend.md)); tessellation-first projection ([ADR-0010](adr/0010-tessellation-is-a-primary-import-path.md)) | **23/23 corpus files round-trip intact**; still to do: vendor exports, and opening our output in Bonsai and Revit |
| **3a — Renderer** ✅ | Headless wgpu: pipeline, depth, culling, readback, PNG | Runs on real hardware (AMD Radeon via Vulkan); renders the demo model; 6 GPU tests |
| **3b — Viewport** | `winit` window, GPU picking, section planes, instancing, LOD | 60 fps on a 100k-element model, desktop; pick < 100 ms; runs on Windows + macOS |
| **4 — Families** | `cadforge-family` end-to-end: define, flex, place, host, export to IFC types | Author a parametric door family, place it in a wall, export IFC, re-import into Bonsai with the void and fill intact |
| **5 — Authoring** | Wall, slab, column, beam, opening commands with native parametric IFC output | Everything in ifc-semantics.md §11 Phase 3 exit criteria |
| **6 — Mobile** | Android then iOS shells over the same core | Same model file opens and renders on all four platforms |
| **7 — Ingest** | `.rfa` params via `rvt-rs`; `.gsm` via GDL subset; glTF meshes | 20-family library imported from each source with parameters intact |

Phases 1–3 are the spine and are sequential. Phase 4 is the differentiator and should get disproportionate effort. Phases 6 and 7 are parallelizable once 1–3 land.

## 8. Deliverable of this pass

A compiling Cargo workspace, not slideware:

```text
CADForge/
├── Cargo.toml                  workspace, pinned to versions verified 2026-08-25
├── crates/
│   ├── cadforge-core/          ElementRecord, GlobalId, ModelCommand, Revision, store
│   ├── cadforge-geom/          profiles, sweeps, tessellation, CsgBackend trait
│   ├── cadforge-family/        FamilyDefinition, params, types, GeometryRecipe, hosting
│   ├── cadforge-ifc/           IfcBackend trait + projection types
│   ├── cadforge-render/        RenderFragment, camera, wgpu backend
│   └── cadforge-shell/         winit + egui entry points per platform
├── docs/
│   ├── PLAN.md                 this file
│   ├── ifc-semantics.md        the IFC mapping reference — still correct, still authoritative
│   ├── adr/                    decisions with their reasoning
│   └── research/LANDSCAPE.md   dated evidence
└── tests/                      integration + golden fixtures
```

Dependency versions are pinned from a live crates.io query on 2026-08-25 (`wgpu` 30.0.1, `glam` 0.33.5, `parry3d` 0.30.2, `tauri` 2.11.5, `ifc-lite-core` 6.0.1) — **not** from the stale pins in ifc-semantics.md §4.2, which list `glam = "0.29"` and `parry3d = "0.18"`.

**Delivered:** ~7,900 lines of Rust across six crates, **162 tests passing**, zero clippy warnings, `cargo fmt` clean.

`cargo run -p cadforge-shell` drives the full stack: four walls authored through commands, a parametric door family placed into a wall as a real `IfcOpeningElement` + `IfcRelVoidsElement` + `IfcRelFillsElement`, meshes evaluated, fragments hashed in local space (4 distinct geometries from 6 fragments — instancing works), camera framed, frustum culled, a simulated GPU pick resolved back to a `GlobalId`, all 19 commands undone to an empty model and redone, **a valid IFC4 file exported** (107 entities, 6 native parametric solids, `IfcDoorType` bound to its instance), and an OBJ written.

Phase 2a landed with it: `cadforge-ifc` now carries a native IFC4 STEP writer ([ADR-0007](adr/0007-own-the-ifc-writer.md)), and `cadforge-core` gained a `Representation` — the bridge between "the recipe is canonical" and what actually goes into a file.

The export is **validated against IfcOpenShell 0.8.5** by `tools/validate_ifc.py` — it parses with no schema or EXPRESS rule violations, every relationship navigates, and IfcOpenShell generates geometry for every element from the swept solids alone.

Phase 3a landed too: a headless wgpu renderer behind a `gpu` feature, running on real hardware (**AMD Radeon via Vulkan**) and writing `out/demo.png`. Deliberately *not* here yet: a `winit` window (Phase 3b) and IFC **reading** (Phase 2b). Both boundaries exist, are exercised by tests, and fail loudly rather than pretending.

That validation surfaced a real incoherence — the exported file carried a correct `IfcRelVoidsElement` while our own uncut meshes showed a solid wall — which the `BspCsg` backend ([ADR-0008](adr/0008-bsp-csg-backend.md)) has since closed. The demo now reports **15.21 m³ on both sides**, and W-01 goes 4.800 → 4.410 m³, matching what IfcOpenShell computes from the exported file to four decimal places.

Worth noting what did *not* change: the exported IFC — same size, same 107 entities, same six swept solids. The `Representation` is canonical and stays an `IfcExtrudedAreaSolid`; cutting is a viewer concern.

## 9. Risks, ranked

| Risk | Severity | Mitigation |
|---|---|---|
| **No Rust B-rep kernel exists** | **High** | Recipe+tessellation model (§4); never put a kernel on the critical path. Fornjot is the precedent for what happens otherwise. |
| BSP booleans are `f64`, not exact | Medium | Limits documented and pinned by tests ([ADR-0008](adr/0008-bsp-csg-backend.md)); element-scale operands only; `ifc-lite`'s exact kernel replaces it behind the same trait once measured. |
| ~~`ifc-lite` is young and single-org~~ | ~~High~~ → Medium | Measured, not assumed ([ADR-0009](adr/0009-ifc-lite-as-the-read-backend.md)). Still behind `IfcBackend`; still unproven against real-world files; writing does not depend on it at all. |
| **`ifc-lite` is young and single-org** | **High** | `IfcBackend` trait; IfcOpenShell subprocess as desktop escape hatch; no type leakage into core. |
| iOS shell maturity (winit/egui on UIKit) | Medium | iOS is P2 and lands in Phase 6, after the core is proven. Android first — better-trodden path. |
| OpenAEC ships an authoring tool | Medium | They have no family system and no mobile. Concentrate there; interoperate rather than compete on viewers and validators. |
| IFC5/IFCX churn (still alpha) | Medium | Author against the internal semantic model; IFC4 first; IFCX as a second serializer. |
| `.rfa` parametric geometry is impossible | **Certain** | Say so up front (§5.2). Params from `rvt-rs`, geometry from IFC export. Never promise more. |
| Mobile authoring UX is unsolved | Medium | Mobile = review/redline/place in v1. Parity in the model, not the toolbar. |

## 10. Immediate next steps

1. ~~Scaffold the workspace, make it build~~ — done in this pass.
2. ~~Phase 1: `cadforge-core` command application and inversion~~ — done, with an exact-inversion sweep over every command variant.
3. ~~Phase 2a: get a model *out* as valid IFC4~~ — done.
4. ~~Validate the exported IFC against IfcOpenShell~~ — done, `tools/validate_ifc.py`, all checks passing.
5. ~~Wire a CSG backend so viewer meshes are cut by their openings~~ — done, [ADR-0008](adr/0008-bsp-csg-backend.md).
6. **Open `out/demo.ifc` in Bonsai and in Revit by hand.** IfcOpenShell acceptance is strong evidence — Bonsai is built on it — but a commercial importer is a different bar. Needs Blender running with Bonsai installed.
7. **Stand up the IFC test corpus** (ifc-semantics.md §12.1) — small residential, mid commercial, and one ugly consultant export. This is now the gate on Phase 2b: the spike proved the round trip on files CADForge itself wrote, which says nothing about a consultant's Revit export with duplicate GlobalIds and broken geometry.
8. ~~Spike `ifc-lite-core` and measure before committing~~ — done, [ADR-0009](adr/0009-ifc-lite-as-the-read-backend.md). 520 MB/s scan, full recipe recovery, every `GlobalId` byte-for-byte. Adopted as the read backend; still a dev-dependency until 2b lands.
9. ~~Spike wgpu before writing renderer code against an unproven shell~~ — done, headless, [ADR-0001](adr/0001-native-shell-over-webview.md#validated-on-hardware).
10. **Put the renderer in a `winit` window** and drive the camera from real input. The pipeline is proven; what is unproven is the event loop, resize, and swapchain handling — and that is the part that differs per platform.
11. **Get a device in hand for Android, then iOS.** wgpu targets Metal and Vulkan there and the API is identical, but compiling for a target is not running on one.
