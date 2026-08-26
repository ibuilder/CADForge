# Security Policy

## Supported versions

CADForge is pre-1.0. Only the latest release receives security fixes.

| Version | Supported |
|---|---|
| 0.1.x | ✅ |
| < 0.1 | ❌ |

## Reporting a vulnerability

Report privately through
[GitHub Security Advisories](https://github.com/ibuilder/CADForge/security/advisories/new).
Please do not open a public issue for a vulnerability.

Include what you did, what happened, and — if you have one — a file or input that reproduces
it. A malformed IFC file attached to the advisory is worth more than a paragraph describing
one.

Expect an acknowledgement within a week. Once a fix ships, credit goes in the advisory and the
changelog unless you would rather it did not.

## The threat model, stated plainly

**IFC files are untrusted input.** They arrive by email from people you have never met,
produced by exporters you cannot audit, and they are large, deeply nested, and full of
cross-references. A parser is an attack surface, and CADForge treats it as one.

Things that are in scope and worth reporting:

- A crafted IFC file that causes a panic, an unbounded allocation, or a hang.
- Path traversal or arbitrary writes through a file path taken from model content.
- Anything that escapes the geometry pipeline into the filesystem or the network.
- Integer overflow or slice indexing reachable from file content.

Things that are known and not vulnerabilities:

- **A malformed file producing a wrong model.** CADForge validates aggressively and reports
  what it could not understand, but garbage in can still mean garbage out. That is a
  correctness bug — please still report it as an issue.
- **Resource use proportional to a legitimately large file.** A 500 MB federated model is
  supposed to be expensive.
- **`BspCsg` producing degenerate geometry on pathological input.** Documented in
  [ADR-0008](docs/adr/0008-bsp-csg-backend.md).

## What the code already does about it

- `GlobalId` is validated on parse, never trusted from a file.
- Representations are structurally checked before export, so malformed geometry cannot reach
  a file.
- Commands validate before they mutate; a rejected command leaves the model untouched.
- No `unsafe` blocks in the workspace at present.

## Dependencies

Dependency updates are automated through Dependabot. `cargo deny` is not yet wired into CI —
that is a known gap, tracked rather than hidden.
