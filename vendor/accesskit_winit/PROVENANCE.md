# accesskit_winit provenance

- Package: `accesskit_winit` 0.34.0 (crates.io), carried as `tilo-accesskit-winit` 0.34.0
- Registry checksum: `1eb960a51582305f94b3884a7ff54b3462abe38f0de95a9e7e04dd7ab7b757da`
- Repository: https://github.com/AccessKit/accesskit
- Upstream revision: `42e53b0d829e7a0b34dc8803bb06012db2e80cc6` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `adapters/winit`
- License: Apache-2.0. The published archive carries no licence text;
  `LICENSE-APACHE` is the repository root's file at the upstream revision,
  byte for byte.
- Vendored: 2026-09-28 (`e1732cc587`)

Local changes: none to the source. The manifest gives the crate its crates.io
identity (`tilo-accesskit-winit`) and points its two `winit` dependencies at
`tilo-winit` (`../winit`), so the published Tilo SDK hands AccessKit the same
winit types it uses itself. The archive's `Cargo.lock` and
`.cargo_vcs_info.json` are omitted. See `TILO-FORK.md` and `vendor/PATCHES.md`,
"Published forks".
