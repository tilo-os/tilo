# taffy provenance

- Package: `taffy` 0.10.1 (crates.io)
- Registry checksum: `aea22054047c16c3f34d3ac473a2170be1424b1115b2a3adcf28cfb067c88859`
- Repository: https://github.com/DioxusLabs/taffy
- Upstream revision: `639f0acdd20f914f847abffbeaa903d66cae4882` (the archive's `.cargo_vcs_info.json`)
- License: MIT. The archive carries no licence text, so `LICENSE.md` is the
  repository root's file at the upstream revision, byte for byte.
- Vendored: 2026-10-30, so layout can round to the display's pixel grid and a
  flex item is measured at the cross size it is laid out at; `TILO-PATCH.md`
  describes both changes.

The tree is the archive's `src/` and `README.md`, with `Cargo.toml` from the
archive's `Cargo.toml.orig` without the example targets, as `TILO-PATCH.md`
describes. The archive's `examples/` and `Cargo.lock` are omitted. Drop the
copy for the crates.io release once upstream rounds to a configurable grid and
clamps the known cross size.
