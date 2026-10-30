## vendor/smithay/
# smithay provenance

- Package: `smithay` 0.7.0 (crates.io)
- Registry checksum: `740cea6927892bc182d5bf70c8f79806c8bc9f68f2fb96e55a30be171b63af98`
- Repository: https://github.com/Smithay/smithay
- Upstream revision: `a166cf4c94b5aedc332a65aa1dd753e8148829c3` (the archive's `.cargo_vcs_info.json`)
- License: MIT. `LICENSE.txt` is the archive's file, byte for byte.
- Vendored: the crate as published under `src`, `build.rs` with the two C
  probes it compiles, its manifest as shipped minus the example and bench
  targets, the licence and the README. Examples, benches, anvil, CI files and
  the changelog stay upstream, and the archive's `Cargo.lock` is omitted.

Local changes are listed in `TILO-PATCH.md`: the warnings the workspace's
`-D warnings` build trips, and tablet pads (`zwp_tablet_pad_v2`) beside the
tablets and tools a tablet seat already announces. See `vendor/PATCHES.md`.
