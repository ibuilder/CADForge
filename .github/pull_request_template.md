## What this changes

<!-- One paragraph. If it fixes an issue, link it. -->

## Why

<!-- The reasoning, not the diff. If it is not obvious from the code, it belongs here. -->

## Checks

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Touched the IFC writer? Ran `python tools/validate_ifc.py out/demo.ifc`
- [ ] Touched the renderer? Ran `cargo test -p cadforge-render --features gpu`

## Architecture

- [ ] No new dependency in `cadforge-core` on a platform or an IFC library
- [ ] No third-party IFC type in a `cadforge-core` signature
- [ ] Any new `ModelCommand` variant has an exact inverse, with a test
- [ ] Contradicts an ADR? A new ADR superseding it is included
