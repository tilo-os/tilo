# pulldown-cmark provenance

- Package: `pulldown-cmark` 0.12.2 (crates.io)
- Registry checksum: `f86ba2052aebccc42cbbb3ed234b8b13ce76f75c3551a303cb2bcffcff12bb14`
- Repository: https://github.com/raphlinus/pulldown-cmark
- Upstream revision: `c118e3002565fe3e5c0a439f2f0649bd10b7c49c` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `pulldown-cmark`
- License: MIT; `LICENSE` is the published archive's file, byte for byte.
- Vendored: 2026-10-19

Local changes:

- `src/entities.rs` lays the HTML5 entity table out without pointers
  (`TILO-PATCH.md`);
- the archive's `tests/` (the CommonMark spec suite), `Cargo.lock`,
  `.cargo_vcs_info.json` and `.cargo-ok` are omitted. Everything else is the
  published source.
