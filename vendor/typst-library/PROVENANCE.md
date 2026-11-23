# typst-library provenance

- Package: `typst-library` 0.15.1 (crates.io)
- Registry checksum: `12becd02464849f1507f3c4ef3f401a4d28d5938cc74b6acc6f1d703458bf393`
- Repository: https://github.com/typst/typst
- Upstream revision: `9dfd3a08500b7896045f907433cf7b4b02434fad` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `crates/typst-library`
- License: Apache-2.0 (the manifest's `license`). The published archive carries no licence text;
  `LICENSE` is Typst's repository root file, the one the tree's other Typst crates are under.
- Vendored: 2026-11-19
- Only the library is vendored: the manifest already builds no test, bench or example target,
  and the archive's `Cargo.lock` is omitted.

The one local change is described, with its reason, in `TILO-PATCH.md`. The manifest also adds
this tree's shared wasm host (`crates/core/tilo-wasm-host`) as a path dependency, which is
what the change is.
