# ADR-0002: The shell is a swappable crate

Date: 2026-08-25 · Status: **Accepted**

## Context

ADR-0001 rejects the webview for the viewport. That rejection rests on a fact about 2026 webviews, not on a dislike of Tauri, and the fact may change. A user preference for Tauri also remains legitimate for reasons ADR-0001 does not address: rich document and report panels, web deployment, reusing an existing web design system.

## Decision

All platform and UI concerns live in **`cadforge-shell`**. Every other crate — `core`, `geom`, `family`, `ifc`, `render` — is shell-agnostic and compiles and tests without a window, a GPU, or an event loop.

A Tauri desktop shell over the identical core is therefore an additive crate, not a rewrite.

## Consequences

- Adding a Tauri shell is estimated at ~2 weeks and touches exactly one crate.
- Both shells could ship simultaneously: Tauri on desktop where WebView2 has WebGPU, native on mobile where no webview does.
- Cost: the shell boundary must stay honest. Any core crate that reaches for `winit`, `egui`, or `tauri` has broken this ADR and should fail review.
- `cargo test -p cadforge-core` must never need a display. That is the enforcement test.
