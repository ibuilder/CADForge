# Contributing to CADForge

Thanks for considering it. CADForge is early, which means contributions can still shape the
architecture rather than just fill it in.

## Before you start

Read [docs/PLAN.md](docs/PLAN.md) and skim [docs/adr/](docs/adr/). CADForge has made several
decisions that look unusual until you know why — no B-rep kernel, no webview, a hand-written
IFC writer. Each has an ADR with the evidence behind it.

For anything larger than a bug fix, open an issue first. It is cheaper to disagree about an
approach in an issue than in a pull request.

## Ground rules the codebase enforces

These are not style preferences. Breaking one usually means something is architecturally
wrong, and reviews will push back.

1. **Every mutation is a `ModelCommand`, and every command has an exact inverse.** Nothing
   else may touch the model store. This is what makes undo, audit, and replay possible rather
   than retrofitted.
2. **`cadforge-core` has no platform and no IFC-library dependencies.** `cargo test -p
   cadforge-core` must pass on a headless machine with no GPU. That is the enforcement test.
3. **No third-party IFC type appears in a `cadforge-core` signature.** The IFC library is
   swappable precisely because nothing in the core knows which one is in use.
4. **The renderer never owns truth.** Delete every fragment and the model is intact.
5. **Geometry fails loudly.** A boolean that silently returns one of its operands corrupts a
   model file, and it is discovered weeks later by somebody else. Return an error.
6. **Iteration order is stable.** `BTreeMap` and `BTreeSet`, not hash containers, anywhere
   that feeds export or hashing. Export must be byte-reproducible.

## Tests

Tests are how this project knows anything. A few things that make them useful here:

- **Assert on physical quantities where you can.** A boolean test that checks volume
  (`4 × 0.2 × 3 − 0.92 × 0.2 × 2.12`) catches errors that a triangle count never will.
- **Test the failure path.** Half the value of a trait boundary is that the failure is
  exercised from day one, not discovered when a real backend arrives.
- **When a test fails, check the test before you change the code.** Two of the more useful
  findings in this repo came from tests whose *premise* was wrong — an inside-out cube does not
  vanish under back-face culling, and BSP output is watertight but not edge-manifold. Both are
  recorded in ADRs so nobody re-derives them.

Before opening a pull request:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If you touched the IFC writer, also run the reference-implementation check:

```bash
pip install ifcopenshell
cargo run -p cadforge-shell
python tools/validate_ifc.py out/demo.ifc
```

If you touched the renderer:

```bash
cargo test -p cadforge-render --features gpu
```

GPU tests skip rather than fail where no adapter exists, so they are safe in CI and on
headless machines.

## Architecture decisions

Changing a decision recorded in an ADR means writing a new ADR that supersedes it — not
editing the old one. The record of *why* something was decided is worth more than the tidiness
of the folder. See [ADR-0004](docs/adr/0004-no-brep-kernel.md) superseding an earlier position
for the shape of it.

An ADR should say what was decided, what the alternatives were, what it costs, and what would
make it wrong.

## Commit messages

Present tense, imperative, and specific about the *why* when it is not obvious:

```
Fix quadratic IFC export

write_voids_and_fills asked each element for its openings, and openings_of
scans the whole relationship set. Exporting 20k walls took 5.86 s; walking
the relationship sets directly takes 0.77 s.
```

## Licensing

CADForge is MPL-2.0. By contributing you agree your contributions are licensed under it.

MPL-2.0 is file-level copyleft: modifications to existing files stay open, but the license
does not reach into code that merely links against it. That combination is deliberate — it
keeps the openBIM core open while letting people build proprietary tools on top.
