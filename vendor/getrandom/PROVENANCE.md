# getrandom provenance

- Package: `getrandom` 0.4.3 (crates.io)
- Registry checksum: `300e883d756b2e4ec94e02791f39b04b522276138852cfc41d9fb7e904106099`
- Repository: https://github.com/rust-random/getrandom
- Upstream revision: `5e7cd5733536844a9856dc7259bd4696bbe5e3ae` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: the repository root
- License: MIT OR Apache-2.0, the release's own `LICENSE-MIT` and
  `LICENSE-APACHE`, byte for byte.
- Vendored: 2026-11-18, so a WebAssembly module that names the `custom`
  backend compiles beside the `wasm_js` feature this workspace enables.

Local changes:

1. In `src/error.rs`, the arm upstream compiles whenever the `wasm_js` feature
   is on also requires that the `custom` backend is not selected
   (`not(getrandom_backend = "custom")`). Upstream's error text names
   `Error::WEB_CRYPTO`, which only the `wasm_js` backend defines, so with
   `custom` selected the module failed to compile. See `TILO-PATCH.md` and
   `vendor/PATCHES.md`, "getrandom's custom backend beside the `wasm_js`
   feature"; remove the patch once upstream releases the fix.

Every other source file is the release's. The archive's `Cargo.toml.orig` and
`.cargo_vcs_info.json` are kept as the release ships them.
