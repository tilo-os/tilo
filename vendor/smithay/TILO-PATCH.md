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

## Tablet pads for clients (`zwp_tablet_pad_v2`)

smithay's tablet seat announces tablets and tools but no pads, and keeps its
resources private, so a compositor cannot add them from outside. The patch
adds pads beside tablets and tools, in the same shape:

- `src/wayland/tablet_manager/tablet_pad.rs` (new): `TabletPadDescriptor`
  (name, path, button count, groups) and `TabletPadGroupDescriptor`
  (buttons, rings, strips, mode count); `TabletPadHandle` announces the pad
  to every tablet seat instance (`pad_added`, its groups with their rings and
  strips, `path`, `buttons`, `done`), enters and leaves surfaces
  (`set_focus`, the current mode of every group after `enter`), and sends
  `button`, ring `angle`/`stop`, strip `position`/`stop` (each with its
  `frame`) and `mode_switch` to the focused client; `has_focused_client`
  tells whether anyone gets them. `set_feedback` requests are accepted and
  ignored.
- `src/wayland/tablet_manager/tablet_seat.rs`: `TabletSeatHandle::add_pad`
  (attached to a tablet the seat knows), `get_pad`, `count_pads`,
  `remove_pad` and `clear_pads` (clients get `removed`), and
  `set_pad_focus`, which focuses every pad on a surface and remembers it for
  pads added later; new seat instances learn about existing pads.
- `src/wayland/tablet_manager/mod.rs`: exports, the `Dispatch` bounds for
  the four pad interfaces, and `delegate_tablet_manager!` delegating them.

The protocol version stays 1 (no dials). Remove this once a smithay release
supports tablet pads.
