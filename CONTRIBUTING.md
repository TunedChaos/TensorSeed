# Contributing to Tensorseed

Thanks for your interest. Tensorseed is pre-alpha (milestone M0), so the design in
[docs/SPEC.md](docs/SPEC.md) can still change. Opening an issue to discuss a change
before writing a large patch saves everyone time.

## Licensing of contributions

Tensorseed is licensed under **AGPL-3.0-or-later**. By submitting a contribution you
agree that it is licensed under AGPL-3.0-or-later, and you confirm that you have the
right to license it that way. New source files start with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos
```

New dependencies must use a license compatible with AGPL-3.0-or-later; `cargo deny
check licenses` enforces the list in `deny.toml`. GPL-2.0-only code cannot be used.

## Before you open a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check licenses bans sources   # optional locally, required in CI
```

## Guidelines

- Keep the catalog format chain agnostic. Chain specific code belongs behind the
  `CatalogBackend` trait in `tensorseed-catalog`.
- Never commit secrets. Configuration reads secrets only from environment variables.
- Hash and verify everything: no code path may accept bytes that were not checked
  against a manifest.
- Write plain text without em or en dashes in docs and comments.
