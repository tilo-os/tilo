# regex-syntax provenance

- Package: `regex-syntax` 0.8.11 (crates.io)
- Registry checksum: `d6f6ff9a378485b298a5286656da665ba74413d36db0979633275d2e708145d4`
- Repository: https://github.com/rust-lang/regex
- Upstream revision: `140167995737fa11dfe11b8af8b9aa143b790b4e` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `regex-syntax`
- License: MIT OR Apache-2.0; `LICENSE-MIT`, `LICENSE-APACHE` and
  `src/unicode_tables/LICENSE-UNICODE` are the published archive's files, byte
  for byte.
- Vendored: 2026-10-19

The whole directory is `tools/regex-syntax-tables.py`'s output for the
published archive: rerun it on that archive to reproduce it, or on a newer
release to update. Local changes (`TILO-PATCH.md`):

- the Unicode name tables (`BY_NAME` of each property table,
  `PROPERTY_NAMES`, `PROPERTY_VALUES`) and the simple case folding table are
  laid out as string literals and integer offsets;
- `src/unicode.rs` and the `arbitrary` impl in `src/ast/mod.rs` read them;
- the archive's `Cargo.lock`, `.cargo_vcs_info.json` and `.cargo-ok` are
  omitted.
