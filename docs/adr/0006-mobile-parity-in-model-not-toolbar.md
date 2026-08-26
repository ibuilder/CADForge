# ADR-0006: Mobile parity is in the model, not the toolbar

Date: 2026-08-25 · Status: **Accepted**

## Context

All four platforms are targets. But full BIM authoring on a phone is an unsolved UX problem, and pretending otherwise produces an application that is mediocre everywhere instead of excellent somewhere.

Shapr3D is the counter-example worth studying: native on Windows, macOS, and iPad with real feature parity. But it is mechanical CAD, with a fundamentally simpler interaction model than BIM authoring, and it had Parasolid and D-Cubed from day one.

## Decision

| Platform | Priority | Role |
|---|---|---|
| Windows | P0 | Full authoring |
| macOS | P1 | Full authoring |
| Android | P2 | Review, redline, measure, place components |
| iOS | P2 | Review, redline, measure, place components |

**The same IFC file, families, commands, and semantic model on every platform.** The toolbar differs; the model does not.

Desktop-only by necessity: `.rfa` and `.gsm` ingest (both need vendor tooling), the IfcOpenShell subprocess backend, and 500 MB+ federated models.

## Consequences

- Mobile v1 ships as an excellent review-and-place client — already better than anything openBIM currently offers on a tablet.
- No core crate may assume a keyboard, a mouse, or a large viewport.
- Tablet authoring can expand later with no change to the model or the file format.
- Android precedes iOS: `android-activity` with `winit` is better-trodden than UIKit.
