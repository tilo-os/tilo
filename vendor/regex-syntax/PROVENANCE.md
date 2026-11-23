# regex-syntax provenance

- Package: `regex-syntax` 0.8.12 (crates.io)
- Registry checksum: `d902046e6a28a1e4c3cab97258a0ce11e994903f01c2d97a9447a0b3df85f4f6`
- Repository: https://github.com/rust-lang/regex
- Upstream revision: `4226f214c56e584304520adb9080bcc4c792390e` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `regex-syntax`
- License: MIT OR Apache-2.0; `LICENSE-MIT`, `LICENSE-APACHE` and
  `src/unicode_tables/LICENSE-UNICODE` are the published archive's files, byte
  for byte.
- Vendored: 2026-10-19 (0.8.11); moved to 0.8.12 on 2026-11-23

The whole directory is `tools/regex-syntax-tables.py`'s output for the
published archive: rerun it on that archive to reproduce it, or on a newer
release to update. Local changes (`TILO-PATCH.md`):

- the Unicode name tables (`BY_NAME` of each property table,
  `PROPERTY_NAMES`, `PROPERTY_VALUES`) and the simple case folding table are
  laid out as string literals and integer offsets;
- `src/unicode.rs` and the `arbitrary` impl in `src/ast/mod.rs` read them;
- the archive's `Cargo.lock`, `.cargo_vcs_info.json` and `.cargo-ok` are
  omitted.
