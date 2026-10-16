# vello_encoding provenance

- Package: `vello_encoding` 0.9.0 (crates.io), carried as `tilo-vello-encoding` 0.9.0
- Registry checksum: `2346f5f0d7dccb3582fcd397b4a57b43165209f1424e0d76e85dd814db164af7`
- Repository: https://github.com/linebender/vello
- Upstream revision: `875f324f21da93019cae9e8e61d4abfd69893206` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `vello_encoding`
- License: Apache-2.0 OR MIT; `LICENSE-APACHE` and `LICENSE-MIT` are the
  published archive's files, byte for byte.
- Vendored: 2026-08-27 (`1ea1c718aa`)

Local changes: `BumpBudget` and buffer sizing from it (`src/config.rs`),
`Encoding::append` keeping forced transform state (`src/encoding.rs`), image
atlas size, limit, eviction, shrink and stats (`src/image_cache.rs`,
`src/resolve.rs`), and the re-export (`src/lib.rs`). The manifest carries the
`tilo-vello-encoding` publication identity. `vendor/PATCHES.md` is the change
log ("Vello changes" and the `vendor/vello_encoding/...` sections after it,
"Published forks"); the standalone history `vendor-upstream/vello-0.9`
("Exportable histories") holds the changes as reviewable commits on the
pristine archive, and `TILO-FORK.md` is the published crate's readme.
