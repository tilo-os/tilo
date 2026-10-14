# wayland-scanner test asset provenance

Not a crate: this directory holds one file,
`tests/scanner_assets/test-protocol.xml`, which `vendor/wayland-backend`'s
in-crate tests read through `../wayland-scanner/...`, the path they use in the
upstream repository.

- Source: `wayland-scanner` 0.31.10 (crates.io), the release cut from the same
  upstream revision as `wayland-backend` 0.3.15
- Registry checksum: `9c324a910fd86ebdc364a3e61ec1f11737d3b1d6c273c0239ee8ff4bc0d24b4a`
- Repository: https://github.com/smithay/wayland-rs
- Upstream revision: `a3d7927d87799b2955bf491b51c7c2a3a82da661`
- Upstream path: `wayland-scanner/tests/scanner_assets/test-protocol.xml`
- License: MIT; `LICENSE.txt` is that archive's file, byte for byte.
- Vendored: 2026-09-20 (`2806eac627`), with `vendor/wayland-backend`

Local changes: none. The file is identical to the one in that archive (and in
every `wayland-scanner` 0.31 release).
