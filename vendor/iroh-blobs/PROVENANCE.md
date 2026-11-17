# iroh-blobs provenance

- Package: `iroh-blobs` 0.103.1 (crates.io)
- Registry checksum: `a3148ad01585eb6b7cfb641e4100564f4f56db644f4cbed8c10ab2478663161f`
- Repository: https://github.com/n0-computer/iroh-blobs
- Upstream revision: `7a4c53a45ecd2bbbb5dd86b3639271004ae89d39` (the archive's `.cargo_vcs_info.json`)
- License: MIT OR Apache-2.0; `LICENSE-MIT` and `LICENSE-APACHE` are the published archive's.
- Only the library is vendored: the manifest drops the example and test targets and the
  dev-dependencies, and the archive's `examples/`, `tests/`, `docs/` (but `docs/img/get_machine.drawio.svg`, which `src/get.rs` includes), `proptest-regressions/`,
  `Cargo.lock`, `CHANGELOG.md`, `DESIGN.md`, `Makefile.toml`, `cliff.toml`, `deny.toml` and
  `code_of_conduct.md` are omitted.

The one local change is described, with its reason, in `TILO-PATCH.md`.
