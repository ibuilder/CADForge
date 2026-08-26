# ADR-0001: Native wgpu viewport, not a webview canvas

Date: 2026-08-25 · Status: **Accepted** · Supersedes nothing

## Context

The brief proposed Tauri for a Windows/macOS/Android/iOS app. Tauri 2 (2.11.5) genuinely
ships all four platforms, and the OpenAEC Foundation uses Tauri across its catalog, so the
proposal was well-founded on its face.

Research on 2026-08-25 found a hard blocker: **WebGPU is not available in any mobile webview.**

| Webview | WebGPU |
|---|---|
| WKWebView (macOS and iOS) | Not supported |
| Android System WebView | Not supported |
| WebView2 (Windows) | Conflicting sources; unverified |

Safari 26 and Chrome 121+ ship WebGPU, but the *browser* is not the *webview*. Compositing a
native GPU surface under a Tauri webview — the obvious escape hatch — is explicitly
unsupported on mobile (tauri#8246, discussions #10964 and #11944).

## Decision

The 3D viewport is a **native `wgpu` surface** driven by `winit`, with `egui` for UI. One
renderer on all four platforms: DX12, Metal, Vulkan, Metal.

## Consequences

- 3D is not capped at WebGL2 on macOS, iOS, and Android.
- Compute shaders, GPU picking, and instancing are available everywhere.
- Cost: `egui` is less expressive than HTML/CSS for dense document and report UI.
- Cost: iOS `winit`/`egui` is the least-trodden path of the four. Mitigated by making iOS P2
  and landing it in Phase 6, after the core is proven.
- Open CAD Studio — the closest comparable, Rust, 2026, solo dev — independently chose
  native `iced` + `wgpu` over a webview. Corroborating, not decisive.

## Validated on hardware

2026-08-26. The claim was that a native wgpu path works; that is now measured rather than
asserted. `cadforge-render` gained a `gpu` feature and a headless renderer, and on this machine
it comes up as:

    AMD Radeon(TM) Graphics via Vulkan

It renders the demo model — four walls, a hosted door, a boolean-cut opening — to a 1600x900
texture and writes `out/demo.png`. Six GPU tests cover device creation, an empty scene, a cube
that actually reaches the framebuffer, determinism across frames, back-face culling, and a
readback width that needs row padding.

Headless, not windowed, on purpose: a window needs a display server and a human, a texture
needs neither, so the whole pipeline runs in CI. `winit` remains a shell concern (ADR-0002) and
nothing in the renderer changes when the target becomes a swapchain or a `CAMetalLayer`.

One test had to be rewritten because its premise was wrong. It asserted that a cube with
reversed winding renders as an empty frame. It does not — flipping the winding culls the near
faces and reveals the far ones, so an inside-out cube is perfectly visible, and you are looking
at the inside of its back wall. The property worth testing is the single-triangle one: wound
toward the camera it draws, wound away it is culled exactly. Both statements are now tests.

Still unproven: iOS and Android. The API is identical and wgpu targets Metal and Vulkan there,
but "compiles for the target" is not "runs on the device", and ADR-0006 keeps both at P2 for
exactly that reason.

## Revisit when

WKWebView or Android WebView ships WebGPU by default, or Tauri supports native GPU surface
composition on mobile. See ADR-0002 for how the door is held open.
