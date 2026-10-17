# cosmic-text provenance

- Package: `cosmic-text` 0.19.0 (crates.io)
- Registry checksum: `be17b688510d934ce13f48a2beba700e11583e281e0fda99c22bb256a14eda73`
- Repository: https://github.com/pop-os/cosmic-text
- Upstream revision: `c24886c2471e5606587c46090cd25dbbf209186b` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: repository root
- License: MIT OR Apache-2.0; `LICENSE-MIT` and `LICENSE-APACHE` are the
  published archive's files, byte for byte.
- Vendored: 2026-10-04 (`ebeb2855c2`)

Local changes: variable fonts are shaped at the width (`wdth`) a run asks for,
not only at its weight, so condensed text gets condensed advances
(`src/font/mod.rs`, `src/font/system.rs`, `src/font/fallback/mod.rs`,
`src/shape.rs`). The manifest also moves `skrifa` and `harfrust` to the versions the rest of
the workspace uses. `TILO-PATCH.md` has the details. Only the library is
vendored: the manifest drops the test, bench and example targets, and their
files (tests, benches, sample text, fonts, screenshots, scripts, CI files,
`CHANGELOG.md`, `Cargo.lock`) are omitted.
