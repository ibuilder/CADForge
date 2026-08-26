# Landscape & Technology Research

Research date: **2026-08-25**. Every claim below is dated because this field moves fast — re-verify before Phase 1 starts.

***

## 1. Direct reference: Open CAD Studio

The project the user pointed at, and the closest existing thing to "a Rust CAD app for AEC".

| Aspect | Finding |
|---|---|
| What | Independent open-source 2D/3D CAD app with **native DWG/DXF** read/write (R13–R2018) |
| Author | Hakan Seven, solo developer, started March 2026 |
| Language | **Rust** |
| UI | [iced](https://iced.rs) — *not* a webview |
| Rendering | **wgpu**, GPU-accelerated |
| Geometry | ACIS solids tessellation |
| Other formats | STL, STEP AP203, OBJ, PDF |
| License | **GPL-3.0** (stricter than OpenAEC's usual LGPL-3.0) |
| Platforms | Windows (.exe/.msi), macOS (Apple Silicon), Linux (AppImage), browser demo |
| **Mobile** | **None** — no iOS, no Android |
| Version | v0.8.8, 66 releases, 1,322 commits |
| **IFC/BIM** | **No IFC support, no BIM semantics** |
| Repo | `github.com/HakanSeven12/OpenCADStudio` |

**Read on it:** Open CAD Studio validates the *technical* thesis (Rust + wgpu CAD is viable and shippable by one person in months) but it is a **drafting tool, not a BIM authoring tool**, and it has **no mobile story**. It is a reference implementation to learn from, not a competitor to clone. The two things CADForge would add — IFC semantics and mobile — are exactly the two things it does not have.

Its stack choice is also a data point *against* Tauri: a Rust CAD developer starting fresh in 2026 chose native iced + wgpu over a webview shell.

## 2. OpenAEC Foundation ecosystem

Open CAD Studio sits inside the OpenAEC Foundation catalog, which is publicly targeting **full production-readiness by end of 2026**.

Named applications: Open PDF Studio, Open 2D Studio, **Open CAD Studio**, Open Geotechniek Studio, **Monty IFC Viewer**, **BIM Validator**, Open Energy Studio, Open Field Studio, **BCF Manager Studio**.

Their stated shared architecture: **"Rust, Tauri, WebAssembly, browser and desktop delivery, and IFCX as an open data format."**

Scope covers IfcRoad, IfcRail, IfcBridge, IfcTunnel, IfcMarineFacility.

**Read on it:** This is both the strongest validation of the CADForge stack thesis and the clearest competitive warning. An organized foundation is building the adjacent tools on nearly the identical stack. Two implications:

1. **Do not rebuild what they ship.** A BCF manager, an IFC validator, and an IFC viewer are already being built by a funded group. CADForge's defensible ground is **authoring with a real family system**, which nothing in that catalog covers.
2. **IFCX is a live target, not a research topic.** Design the persistence layer so IFC-SPF (2X3/4/4X3) and IFCX are two serializers over one semantic core, not one format with the other bolted on.

## 3. The competitive field for BIM authoring

The "BIM 2.0" cohort discussed together at NXT BLD 2026 — **Motif, Arcol, Autodesk Forma, Hypar, Snaptrude, Qonic**:

- **Snaptrude** — pivoted to concentrate AI on conceptual/schematic phases.
- **Arcol** — real-time collaborative design and presentation, feasibility → boards, browser, any device.
- **Qonic** — went the *opposite* direction from the rest: started at **production documentation** accuracy, browser-based BIM modeling and coordination, federation of consultant models.
- **Motif** — Amar Hanspal (ex-Autodesk CTO), "agent-native" platform built from scratch.

Every one of these is **cloud/browser-first and closed-source**. None is a native offline-capable app; none targets iPad/Android as a first-class authoring surface.

**Shapr3D** is the proof that the *other* posture works: native on Windows, macOS, **and iPad** with full feature parity, Siemens **Parasolid** kernel, D-Cubed sketch engine, files sync across devices. It is not BIM — it is mechanical CAD — but it is the single best existence proof that a serious cross-platform native CAD app including tablets is achievable and commercially viable.

**Read on it:** The open lane is **native, offline-capable, cross-platform (including tablet) IFC authoring**. Browser-first is crowded and well-funded; native+mobile+openBIM is empty.

## 4. IFC5 / IFCX status

- IFC5 is in **alpha** as of 2026.
- New file format **`.ifcx`**; explores workflows from the **USD ecosystem**.
- Moves away from a single monolithic file to a **component-based / layered** model where objects decompose into smaller independent parts.
- Schema source defined in **TypeSpec**; JSON schema to be published at `ifcx.dev`.
- buildingSMART is explicit that **USD and IFC5 are related concepts but distinct, autonomous standards**.
- Dev repo: `github.com/buildingSMART/IFC5-development`; viewer at `ifc5.technical.buildingsmart.org/viewer/`.

**Read on it:** IFC5's layered composition model is a *much* better fit for a family/component system than IFC4's monolithic STEP file. But it is **alpha** — building the core on it today is a schedule risk. Correct posture: **author against an internal semantic model, ship IFC4 first, keep IFCX as a second serializer** that can become primary without a rewrite.

## 5. Rust IFC libraries

| Crate | Version (2026-08-25) | Verdict |
|---|---|---|
| **ifc-lite-core** | 6.0.1, updated **today**, MPL-2.0 | Most promising. IFC2X3/4/4X3 **and IFC5 (IFCX)**. 100% of IFC4 (776 entities) and IFC4X3 (876 entities). ~50 MB/s parse, ~1.2 GB/s STEP tokenization, 1.2 MB gzipped WASM. Exact-arithmetic CSG kernel, **verified element-by-element against IfcOpenShell at 99.9%+ agreement**. Sibling crates: `-geometry`, `-export`, `-clash`, `-processing`, `-ffi`, `-wasm`. Repo `ltplus-ag/ifc-lite`, 345 stars, 2,418 commits, pushed today. Supports authoring (`@ifc-lite/create`) and export to IFC-SPF, glTF, Parquet, IFC5. |
| **ifc_rs** | — | `MetabuildDev/ifc_rs`. Self-described work-in-progress, API expected to change a lot. |

**Caveat that matters:** `ifc-lite-core` has **~1,157 downloads/month and is used by 5 crates**. It is excellent work but it is *young and barely adopted*. Depending on it for the semantic core is a single-maintainer risk. Its own docs note the exact-arithmetic kernel trades speed for correctness, and 500 MB+ files want the native path.

**Decision that follows:** use `ifc-lite-*` behind a **CADForge-owned trait boundary** (`IfcBackend`), never let its types leak into the domain model, and keep an IfcOpenShell-subprocess backend as the escape hatch on desktop. See ADR-0003.

## 6. Rust geometry kernels — the hard part

| Kernel | Status (verified 2026-08-25) |
|---|---|
| **truck** (`ricosjp/truck`) | **1,533 stars. Repo actively maintained — last push 2026-08-24.** B-rep + NURBS, boolean operations, defeaturing, topological healing. **But: crates.io releases are stale — `truck-modeling` 0.6.0 last published 2024-09-20, only 66k lifetime downloads.** Recent commits are mostly `cargo upgrade` maintenance. |
| **Fornjot** | **Dead.** The author has ended the project: "No longer in development… its goals were not reached." |
| **opencascade-rs** | Rust bindings to OpenCASCADE. "Major work in progress." Needs a C++ toolchain + CMake to build. |
| **ifc-lite** CSG | Exact-arithmetic boolean kernel, IfcOpenShell-verified. Mesh/CSG level, not full B-rep. |

**This is the single biggest technical risk in the project.** There is **no production-grade pure-Rust B-rep kernel in 2026**. Fornjot's death is the cautionary tale — a talented developer worked for years and publicly concluded the goal was not reached.

Three consequences, all reflected in the plan:

1. **Do not build the product on a B-rep kernel.** Architecturally, a *parametric recipe* (profile + extrusion + boolean) plus a *tessellated result* covers the overwhelming majority of real building elements. Walls, slabs, columns, beams, openings, doors, windows, railings, ducts, and pipes are all sweeps of profiles.
2. **Where a kernel is unavoidable, keep it behind a trait** and pick per-platform: truck (git dependency, not the stale crates.io release) where it works; a subprocess kernel on desktop; explicit failure elsewhere.
3. **Correction to the earlier IFC roadmap** (now `docs/ifc-semantics.md`): its §4.2 crate pins are already wrong — it lists `glam = "0.29"` (actual 0.33.5) and `parry3d = "0.18"` (actual 0.30.2). Its §4.5 Truck bet is directionally right but needs the git-vs-crates.io nuance above.

## 7. The decisive finding: WebGPU is not available in webviews

This is what changes the shell architecture.

| Webview | WebGPU (2026-08-25) |
|---|---|
| **WKWebView** (macOS **and** iOS) | **Not supported** |
| **Android System WebView** | **Not supported** |
| **WebView2** (Windows) | Conflicting sources — `caniwebview.com` lists unsupported; Edge/Chromium ships WebGPU by default since late 2025 and WebView2 is evergreen Chromium, so it likely works. **Unverified — treat as unavailable until measured.** |
| Safari 26 / iOS 26 **browser** | Supported — *but the browser is not the webview* |
| Chrome 121+ Android **browser** | Supported — *again, not the system webview* |

Sources are consistent on the key point: *"iOS WKWebView does not ship WebGPU by default… Android WebView does not ship WebGPU by default, so hybrid apps still need a fallback to WebGL or native code."*

**Therefore: a Tauri app that renders 3D inside its webview is capped at WebGL2 on macOS, iOS, and Android.** Not "degraded on old devices" — capped, on current flagship hardware, on three of the four target platforms.

For a viewer, WebGL2 is survivable. For a CAD *authoring* app carrying full building models with GPU picking, instancing, and compute-driven culling, it forfeits the core reason to write the thing in Rust.

## 8. Tauri mobile status

- Tauri 2.0 stable since 2024-10-02; that release added iOS and Android.
- Current: **2.11.5** (tauri crate, 2026-07-01). Wikipedia lists 2.10.1 stable line March 2026; core runtime 2.11.5 by July 2026.
- Minimums: **iOS 9, Android 8 (API 26)**.
- The Tauri team's own framing: *"we don't want to raise expectations that Tauri 2.0 will be the 'mobile as a first class citizen' release, but you can develop production ready mobile applications with Tauri NOW."* Not all desktop features and plugins are ported to mobile.
- Plugins touching filesystem/camera need Info.plist / AndroidManifest entries; CLI scaffolds them, you approve them.
- **Native GPU surface composited with the webview**: multiple open discussions (`tauri#8246`, `tauri-apps/discussions/10964`, `#11944`). Tauri v2 can host multiple surfaces in a window on desktop. **Click-through overlays are explicitly not supported on mobile or Wayland.** Community consensus: combining native wgpu rendering under a webview UI on iOS/Android is *still evolving*, i.e. not a foundation to build on.

## 9. Family system — source format reality check

The "families like Revit, Blender, Archicad" requirement, assessed honestly per source:

| Source | Path | Realistic outcome |
|---|---|---|
| **Revit `.rfa`** | **`rvt-rs`** (`DrunkOnJava/rvt-rs`, **Apache-2.0**, Rust, tested across 11 Revit releases 2016–2026). Opens the OLE/CFB container, decodes truncated-gzip streams, extracts metadata + previews, parses the embedded `Formats/Latest` schema. Also `CodeCavePro/revitless-toolkit` (.NET) for metadata, shared parameters, type catalogs. | **Metadata, parameters, and type catalogs — yes. Parametric geometry — no.** Revit's geometry is a closed format; nobody has reverse-engineered the constraint solver. Geometry must come via IFC export from Revit. **`rvt-rs` has 12 stars and one author — treat as an experiment, vendor it, do not depend on it in the critical path.** |
| **Archicad `.gsm`** | **`LP_XMLConverter`**, shipped next to `Archicad.exe`, converts library parts ↔ XML/HSF: `LP_XMLConverter.exe l2x <src> <dst>`. GDL is documented and Graphisoft explicitly permits building GDL libraries with free tools. | **Best parametric path of the three.** GDL is a real scripting language with parameters — a GDL *subset* interpreter is tractable. Requires an Archicad install to run the converter, so it is an offline/desktop ingest step, not a runtime feature. |
| **Blender** | `.blend` has a documented DNA/SDNA structure; glTF export is the pragmatic route. **Bonsai** (fka BlenderBIM, on IfcOpenShell) already does native IFC authoring in Blender and **stores parametric definitions in the IFC data itself**, exchangeable with other apps supporting parametric IFC. | **Mesh-level families via glTF — easy. Parametric — go through Bonsai's IFC output**, which is the same semantic channel CADForge already speaks. Do not write a `.blend` parser. |
| **IFC itself** | `IfcTypeProduct` + `IfcRelDefinesByType` + representation maps + property sets | **The native family format.** Everything else normalizes into this. |

**Read on it:** "Import Revit families" cannot mean "read `.rfa` and get parametric geometry" — that is not possible without Revit. It can mean: read `.rfa` **metadata/parameters/types** with `rvt-rs`, take **geometry from IFC export**, and rebuild the parametric recipe in CADForge's own family format. The plan says this plainly rather than promising the impossible.

## 10. Local toolchain (verified on this machine)

```
rustc    1.98.0 (2026-08-18)   stable-x86_64-pc-windows-msvc
cargo    1.98.0
node     v24.18.0
npm      11.16.0
git      2.45.2.windows.1
python   3.10.6
cmake    NOT INSTALLED
pnpm     NOT INSTALLED
targets  x86_64-pc-windows-msvc only
```

**No CMake** is itself an argument for pure-Rust dependencies: `opencascade-rs` and an IfcOpenShell native build both need a C++ toolchain. Every crate selected in the plan builds with cargo alone.

***

## Sources

- [Open CAD Studio — open-aec.com](https://open-aec.com/open-cad-studio/)
- [Open CAD Studio — project site](https://hakanseven12.github.io/OpenCADStudio)
- [OpenAEC outlines open source AEC ecosystem and 2026 roadmap — OSArch](https://osarch.org/2026/06/05/openaec-open-source-aec-ecosystem-roadmap/)
- [Open CAD Studio (with native DWG/DXF support) — OSArch community](https://community.osarch.org/discussion/3472/open-cad-studio-with-native-dwg-dxf-support)
- [Open AEC Foundation](https://open-aec.com/)
- [buildingSMART IFC5-development](https://github.com/buildingSMART/IFC5-development)
- [Learn IFC 5 with Us! — buildingSMART](https://ifc5.technical.buildingsmart.org/)
- [IFC Schema Specifications — buildingSMART Technical](https://technical.buildingsmart.org/standards/ifc/ifc-schema-specifications/)
- [ifc-lite-core — lib.rs](https://lib.rs/crates/ifc-lite-core)
- [ifc-lite — GitHub (ltplus-ag)](https://github.com/ltplus-ag/ifc-lite)
- [ifc_rs — GitHub (MetabuildDev)](https://github.com/MetabuildDev/ifc_rs)
- [truck — GitHub (ricosjp)](https://github.com/ricosjp/truck)
- [Fornjot — GitHub (no longer in development)](https://github.com/hannobraun/fornjot)
- [opencascade-rs — GitHub](https://github.com/bschwind/opencascade-rs)
- [IfcOpenShell](https://ifcopenshell.org/)
- [Can I WebView… WebGPU](https://caniwebview.com/features/web-feature-webgpu/)
- [WebGPU in iOS 26 — App Developer Magazine](https://appdevelopermagazine.com/webgpu-in-ios-26/)
- [WebGPU Browser Support in 2026](https://webo360solutions.com/blog/webgpu-browser-support/)
- [Tauri 2.0 Stable Release](https://v2.tauri.app/blog/tauri-20/)
- [Announcing the Tauri Mobile Alpha Release](https://v2.tauri.app/blog/tauri-mobile-alpha/)
- [Tauri — Native renderer? (Discussion #10964)](https://github.com/orgs/tauri-apps/discussions/10964)
- [Tauri — Render WebView on Top of Native GPU Rendered Content (#8246)](https://github.com/tauri-apps/tauri/issues/8246)
- [Tauri (software framework) — Wikipedia](https://en.wikipedia.org/wiki/Tauri_(software_framework))
- [rvt-rs — open reader for Revit files](https://github.com/DrunkOnJava/rvt-rs)
- [revitless-toolkit — CodeCavePro](https://github.com/CodeCavePro/revitless-toolkit)
- [How to use the LP_XMLConverter tool — GRAPHISOFT GDL Center](https://gdl.graphisoft.com/tips-and-tricks/how-to-use-the-lp_xmlconverter-tool/)
- [HSF source format — GDL Center](https://gdl.graphisoft.com/tips-and-tricks/hsf-source-format/)
- [Geometric Description Language — Wikipedia](https://en.wikipedia.org/wiki/Geometric_Description_Language)
- [Parametric Geometry — Bonsai documentation](https://docs.bonsaibim.org/guides/authoring/advanced_modeling/parametric_geometry.html)
- [Bonsai — IfcOpenShell documentation](https://docs.ifcopenshell.org/bonsai.html)
- [NXT BLD 2026: a decade of looking around corners — AEC Magazine](https://aecmag.com/nxt-bld/nxt-bld-2026-a-decade-of-looking-around-corners)
- [Agentic BIM — The Startups Challenging Revit — archBIM.cloud](https://archbim.cloud/en/blog/agentic-bim-startups-challenging-revit-2026)
- [Shapr3D](https://www.shapr3d.com/)
- [Shapr3D — Wikipedia](https://en.wikipedia.org/wiki/Shapr3D)
