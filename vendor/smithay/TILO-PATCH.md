# Tilo patch: smithay

Source: crates.io `smithay` 0.7.0, checksum
`740cea6927892bc182d5bf70c8f79806c8bc9f68f2fb96e55a30be171b63af98`,
upstream revision `a166cf4c94b5aedc332a65aa1dd753e8148829c3`
(https://github.com/Smithay/smithay, the archive's `.cargo_vcs_info.json`).
License: MIT, `LICENSE.txt` as published.

Only the library is vendored: `src`, `build.rs` with the two C probes it
compiles, the manifest as shipped minus its example and bench targets, the
license and README. Examples, benches, anvil, CI files and the changelog stay
upstream.

## Warning-free on the workspace toolchain

A path dependency's warnings are not capped like a registry crate's, and CI
builds with `-D warnings`. Five `use tracing::{..., error, ...}` lists drop
the `error` macro they never call (`src/backend/drm/surface/gbm.rs`,
`src/backend/session/libseat.rs`, `src/input/keyboard/mod.rs`,
`src/wayland/seat/keyboard.rs`, `src/xwayland/xwm/mod.rs`), and
`src/wayland/shm/pool.rs` casts its SIGBUS handler through a pointer before
the integer (`function_casts_as_integer`). No behaviour changes.

Drop these once a smithay release builds without warnings on this toolchain.
