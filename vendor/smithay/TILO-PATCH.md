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
builds with `-D warnings`. Six `use tracing::{..., error, ...}` lists drop
the `error` macro they never call (`src/backend/drm/surface/gbm.rs`,
`src/backend/session/libseat.rs`, `src/backend/winit/mod.rs`,
`src/input/keyboard/mod.rs`, `src/wayland/seat/keyboard.rs`,
`src/xwayland/xwm/mod.rs`), and
`src/wayland/shm/pool.rs` casts its SIGBUS handler through a pointer before
the integer (`function_casts_as_integer`). No behaviour changes.

Drop these once a smithay release builds without warnings on this toolchain.

## The window backend builds against the toolkit's winit

Tilo's own windows are built with `tilo-winit` (`vendor/winit`), and a
compositor in a window must run its window on the same crate as the toolkit
around it rather than a second copy of winit beside it. The manifest therefore
depends on `winit` as `package = "tilo-winit"`, at the version this release
asks for, from `../winit`. The fork's event enum has three variants this
release does not know (`PrivateIme`, `KeyboardLayoutChanged`, `PenSample`),
which `src/backend/winit/mod.rs` ignores beside the events it already does not
use.

Remove this once the workspace can redirect a vendored crate's dependency (or
`tilo-winit` is published and this release takes it).

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

## Drops on targets the compositor serves itself

A client's drag dropped where no Wayland client accepted an offer (a hosted
surface, or an X11 window the compositor bridges itself) always told the
source `cancelled`, so a move never completed and the source kept the item.

- `src/wayland/selection/data_device/mod.rs`:
  `ClientDndGrabHandler::drop_target_action(&mut self, seat) -> Option<DndAction>`,
  default `None` (upstream behaviour).
- `src/wayland/selection/data_device/dnd_grab.rs`: the shared pointer and
  touch `drop()` asks it when there is a data source and no accepted offer.
  `Some(action)` sends the source `action` and `dnd_drop_performed` (by
  version) instead of `cancelled`, and calls `dropped` with `validated`
  set; the compositor sends `dnd_finished` once its target read the data.

Remove this once smithay lets a compositor complete drops on its own targets.

## What the focused field committed (`text-input-v3`)

smithay's text-input handle keeps a client's pending state but takes the
committed state out of it on `commit` and only hands it to the input method,
so a compositor cannot tell which field has the focus, what it declared, or
whether it changed since it was looked at.

- `src/wayland/text_input/text_input_handle.rs`: each instance keeps
  `committed_state` (the serial of the commit that accepted it, the content
  type it declared and the cursor rectangle it reported; a commit that does
  not restate either keeps the previous one, since clients send them only
  when they change). A commit that is accepted stores it in the same places
  the handler continues past its "discarding …" returns, a commit that
  disables the field clears it, and a discarded commit changes nothing.
  `TextInputHandle::with_active_text_input_committed(f)` reports it for the
  active instance — the text-input object, its surface and a
  `CommittedState` — and returns whether there is one.

Remove this once a smithay release keeps committed text-input state,
including the cursor rectangle, or exposes what the focused field declared
and where its caret is.
