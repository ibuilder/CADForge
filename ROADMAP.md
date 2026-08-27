# Roadmap

Where CADForge is, where it is going, and what would change the plan.

This is the public roadmap. The reasoning behind each decision lives in
[docs/adr/](docs/adr/), the full build plan in [docs/PLAN.md](docs/PLAN.md), and the dated
evidence in [docs/research/LANDSCAPE.md](docs/research/LANDSCAPE.md).

## Status

**Early development.** CADForge reads and writes IFC, authors a building semantically,
evaluates parametric families, cuts openings, renders on the GPU, and **opens a window you can
drop an IFC file into**. It has no authoring tools — everything is still driven from code.

| Phase | State | What it means |
|---|---|---|
| **0 — Foundation** | ✅ Complete | Research, decisions, a compiling and tested workspace |
| **1 — Core model** | ✅ Complete | Elements, commands with exact inverses, revisions, undo, spatial index |
| **2a — IFC out** | ✅ Complete | Native IFC4 writer, validated against IfcOpenShell |
| **2b — IFC in** | ✅ Complete | Reads IFC4 and IFC4X3; all 23 corpus files round-trip intact |
| **3a — Renderer** | ✅ Complete | Headless wgpu: pipeline, depth, culling, readback |
| **3b — Viewport** | Partial | Window, orbit/pan/zoom, and IFC display work; GPU picking, section planes, and instancing do not |
| **4 — Families** | Partial | Definition, flexing, hosting, and IFC type export all work; library management does not |
| **5 — Authoring** | Planned | Wall, slab, column, beam, and opening tools with native parametric output |
| **6 — Mobile** | Planned | Android, then iOS |
| **7 — Ingest** | Planned | `.rfa` parameters, Archicad GDL, glTF meshes |

## Next

Three things gate progress, in this order.

### 1. An IFC test corpus ✅ *partly*

buildingSMART's 23 certification sample models are now fetchable and surveyed:

```bash
python tools/fetch_corpus.py
cargo run --release -p cadforge-ifc --example corpus_survey
```

They immediately overturned an architectural assumption — **710 of 719 shape representations
are tessellated, not swept** ([ADR-0010](docs/adr/0010-tessellation-is-a-primary-import-path.md)) —
and showed that only 43% of product instances map to a native `IfcClass`, with the rest
largely infrastructure.

**Still missing: vendor-exported architectural models.** Revit, Archicad, and Tekla output, plus
at least one coordination model that is genuinely broken. The certification corpus is clean and
skews to infrastructure; it cannot tell us how the importer behaves against duplicate
`GlobalId`s, mis-nested placements, and geometry that fails to generate.

### 2. IFC import (Phase 2b) ✅

Done. All 23 corpus files import — 984 elements, one geometry failure, zero dangling
references — and **all 23 survive import → export → import with every element intact**.

```bash
cargo run --release -p cadforge-ifc --example corpus_import
```

Pointing it at real files immediately found two bugs no test over our own output could reach:
the exporter was dropping every `IfcSpace` and every site past the first, and elements inside
IFC4X3 infrastructure spatial parts were being stranded. Both are fixed; see the
[changelog](CHANGELOG.md).

### 3. A window (Phase 3b) — mostly done

```bash
cargo run -p cadforge-shell --features viewport --bin cadforge-viewport -- model.ifc
```

Opens a `winit` window on a `wgpu` surface, imports the file, and gives you orbit, pan, and
zoom. `--png out.png` does the same headless.

Putting a real file on screen immediately found a bug that every unit test, the whole corpus
round trip, and IfcOpenShell validation had missed: **imported lengths ignored the file's
units**, so a millimetre model came in a thousand times too large. Export and import shared the
same wrong assumption, so nothing that compared them could see it.

Still to do here: GPU picking, section planes, and instancing.

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
