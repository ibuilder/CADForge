# Roadmap

Where CADForge is, where it is going, and what would change the plan.

This is the public roadmap. The reasoning behind each decision lives in
[docs/adr/](docs/adr/), the full build plan in [docs/PLAN.md](docs/PLAN.md), and the dated
evidence in [docs/research/LANDSCAPE.md](docs/research/LANDSCAPE.md).

## Status

**Early development.** Three of eight phases are complete. CADForge can author a building
semantically, evaluate parametric families, cut openings, render on the GPU, and write valid
IFC4. It cannot yet read IFC, and it has no window.

| Phase | State | What it means |
|---|---|---|
| **0 — Foundation** | ✅ Complete | Research, decisions, a compiling and tested workspace |
| **1 — Core model** | ✅ Complete | Elements, commands with exact inverses, revisions, undo, spatial index |
| **2a — IFC out** | ✅ Complete | Native IFC4 writer, validated against IfcOpenShell |
| **2b — IFC in** | Next | Read IFC4 and project to `ElementRecord` |
| **3a — Renderer** | ✅ Complete | Headless wgpu: pipeline, depth, culling, readback |
| **3b — Viewport** | Planned | `winit` window, GPU picking, section planes, instancing |
| **4 — Families** | Partial | Definition, flexing, hosting, and IFC type export all work; library management does not |
| **5 — Authoring** | Planned | Wall, slab, column, beam, and opening tools with native parametric output |
| **6 — Mobile** | Planned | Android, then iOS |
| **7 — Ingest** | Planned | `.rfa` parameters, Archicad GDL, glTF meshes |

## Next

Three things gate progress, in this order.

### 1. An IFC test corpus

Everything round-tripped so far was written by CADForge itself. That proves the format is
self-consistent; it says nothing about a consultant's Revit export with duplicate `GlobalId`s,
mis-nested placements, and geometry that fails to generate.

The corpus needs IFC2X3 and IFC4 files from Revit, Archicad, Tekla, and at least one
coordination model that is genuinely broken. This is the gate on Phase 2b — not the parser.

### 2. IFC import (Phase 2b)

The backend is chosen on measurement rather than reputation
([ADR-0009](docs/adr/0009-ifc-lite-as-the-read-backend.md)): `ifc-lite-core` scans at
520 MB/s and recovers the full parametric recipe — profile points and extrusion depth, not
triangles — with every `GlobalId` byte-for-byte.

What remains is the projection layer from IFC entities to `ElementRecord`, plus honest
handling of everything real files do wrong.

### 3. A window (Phase 3b)

The render pipeline is proven on hardware. What is unproven is the event loop, resize, and
swapchain handling — and that is precisely the part that differs per platform.

## Further out

**Authoring tools (Phase 5).** Wall, slab, column, beam, and opening commands with snapping,
constraints, and level-aware placement. The commands and geometry already exist; what is
missing is the interaction model.

**Mobile (Phase 6).** Android first, because `android-activity` with `winit` is better trodden
than UIKit. Parity is in the *model*, not the toolbar — see
[ADR-0006](docs/adr/0006-mobile-parity-in-model-not-toolbar.md). You do not draft on a phone;
you review, redline, measure, and place components.

**Family ingest (Phase 7).** Stated honestly, because one of these cannot deliver what people
assume ([ADR-0005](docs/adr/0005-family-system-is-the-differentiator.md)):

| Source | What is actually possible |
|---|---|
| Archicad `.gsm` | The best parametric path. `LP_XMLConverter` → HSF/XML → a GDL subset interpreter. |
| Blender | Mesh families via glTF; parametric via Bonsai's IFC output. No `.blend` parser. |
| Revit `.rfa` | Parameters, types, and metadata via `rvt-rs`. **Not parametric geometry** — Revit's geometry and constraint solver are closed and have not been reverse-engineered. Geometry comes from Revit's IFC export. |

**IFC5 / IFCX.** Alpha as of 2026, and a much better fit for a component system than IFC4's
monolithic file. Treated as a second serialiser over the same semantic core, so it can become
primary without a rewrite.

## What would change this plan

Kept explicit, because a roadmap that cannot be falsified is marketing.

- **A production-grade pure-Rust B-rep kernel appears.** [ADR-0004](docs/adr/0004-no-brep-kernel.md)
  exists because there isn't one — Fornjot ended without reaching its goals and `truck`'s
  crates.io releases are two years stale. If that changes, freeform authoring comes back into
  scope.
- **WebGPU ships in WKWebView and Android WebView.** [ADR-0001](docs/adr/0001-native-shell-over-webview.md)
  rejects a webview viewport because WebGPU is absent from every mobile webview, capping 3D at
  WebGL2 on three of four target platforms. If that changes, a Tauri shell becomes viable — and
  [ADR-0002](docs/adr/0002-shell-boundary-keeps-tauri-possible.md) keeps that door open at the
  cost of one crate.
- **`ifc-lite` stalls.** It is young and single-organisation. That is why it sits behind
  `IfcBackend` and why the writer does not depend on it at all.
- **Real-world files defeat the round trip.** If the corpus shows the semantic model cannot
  hold what real projects contain, the model changes — not the files.

## Non-goals

- **A DWG/DXF drafting tool.** [Open CAD Studio](https://open-aec.com/open-cad-studio/) already
  does that, in Rust, well. Interoperate rather than duplicate.
- **Another IFC viewer.** Several exist and more are being funded.
- **A browser app.** That lane is crowded — Motif, Arcol, Qonic, Snaptrude, Forma — and all of
  it is closed. Native, offline-capable, openBIM authoring including tablets is the empty one.
- **A B-rep kernel project.** See above.
