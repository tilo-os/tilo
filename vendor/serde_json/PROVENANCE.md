# serde_json provenance

- Package: `serde_json` 1.0.151 (crates.io)
- Registry checksum: `c841b55ecdae098c80dcae9cf767f6f8a0c2cdb3416bbef72181df4d0fe73f14`
- Repository: https://github.com/serde-rs/json
- Upstream revision: `de8500740cdcabffb9734f503e4889def823cf10` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: the repository root
- License: MIT OR Apache-2.0; `LICENSE-MIT` and `LICENSE-APACHE` are the
  published archive's files, byte for byte.
- Vendored: 2026-10-24

Local changes:

- `src/value/de.rs`, `src/value/mod.rs` and `src/map.rs` move code every
  crate compiled for itself into serde_json (`TILO-PATCH.md`);
- `Cargo.toml` drops the archive's `[[test]]` targets and
  `[dev-dependencies]`, and the archive's `tests/`, `.github/`, `.gitignore`,
  `CONTRIBUTING.md`, `Cargo.lock`, `.cargo_vcs_info.json` and `.cargo-ok` are
  omitted. Everything else is the published source.

To update, unpack the new release over this directory, apply the same
manifest edits and re-apply `TILO-PATCH.md`; Cargo's `[patch.crates-io]`
entry only replaces the version this manifest declares.
