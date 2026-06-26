# Versioning Policy

Carousel follows [Semantic Versioning](https://semver.org/).

## Current phase: pre-1.0

While the version is `0.x.y`:

- `0.MINOR.x` — breaking changes **or** new features bump MINOR.
- `0.x.PATCH` — bug fixes only bump PATCH.

This is standard Cargo 0.x semantics: a `0.MINOR` bump may break compatibility.

## Post-1.0

Once `1.0.0` ships:

- **MAJOR** — incompatible / breaking changes.
- **MINOR** — backwards-compatible features.
- **PATCH** — backwards-compatible bug fixes.

## Release mechanics

- Tags use the form `vX.Y.Z` (e.g. `v0.2.0`).
- Bump the `version` field in `Cargo.toml` in the same PR that prepares a
  release, **before** tagging. The release workflow's `verify` job fails the
  build if the tag does not match `Cargo.toml`.
- One tag = one release. Never re-tag or move a version that has been published.
- Releases are published as GitHub **drafts** and reviewed before going public.
