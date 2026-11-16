# hexane provenance

- Package: `hexane` 1.0.0-alpha.5 (crates.io), the column store under `automerge` 0.12.0
- Registry checksum: `81df830f11babb0eee9a082e09135ef0a7d8791f3fe9b279bb9485bf7eaf23e0`
- Repository: https://github.com/automerge/automerge
- Upstream revision: `8d20da046282a43b5af3c3bdacd4467b48545087` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `rust/hexane`
- License: MIT (the manifest's `license`). The published archive carries no licence text;
  `LICENSE` is the Automerge repository's root file, the one `vendor/automerge` carries.
- Only the library is vendored: the manifest drops the example, test and bench targets and
  the `divan` dev-dependency they use, and the archive's `benches/`, `examples/`, `tests/`,
  `proptest-regressions/`, `Cargo.lock`, `CHANGELOG.md`, `TODO` and `V1_MIGRATION.md` are
  omitted.

The one local change is described, with its reason and pinning test, in `TILO-PATCH.md`.
