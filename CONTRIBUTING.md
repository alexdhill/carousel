# Contributing to Carousel

Thanks for contributing! Carousel is a `wry`/`tao` desktop app built in Rust.

## Workflow

1. Branch from `main` — direct pushes to `main` are blocked.
2. Make your change.
3. Open a pull request. CI (fmt, clippy, tests) must pass and the branch must
   be up to date with `main` before it can merge.

## Before you open a PR

Run these locally and make sure they are clean:

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings   # zero warnings
cargo test
```

## Code expectations

- No compiler or clippy warnings.
- Functions stay focused — aim for ≤ 80 lines, one statement per line.
- Validate inputs; assert on critical/abnormal data.
- Match the style of surrounding code.

## Versioning & releases

See [docs/SEMVER.md](docs/SEMVER.md). Maintainers cut releases by bumping
`Cargo.toml` and pushing a `vX.Y.Z` tag.

## Local build

Linux needs `libwebkit2gtk-4.1-dev` installed. macOS and Windows build with the
default toolchain.
