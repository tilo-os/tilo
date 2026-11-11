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

The backends no Tilo crate builds are left out too: the X11 backend
(`backend_x11`), Vulkan (`backend_vulkan`, its allocator included) and the
glow renderer (`renderer_glow`), with their features and the `ash` and `glow`
dependencies only they used.

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

## A focused field is entered without an input method (`text-input-v3`)

smithay entered a client's text-input only while an input method ran and
discarded every text-input request otherwise, so on a machine with no IME or
on-screen keyboard no field ever reported its caret or content type, and
nothing the compositor places at the caret could find it.

- `src/wayland/seat/keyboard.rs`: keyboard `enter`/`leave` always send
  `enter`/`leave` to the surface's text-inputs.
- `src/wayland/text_input/mod.rs`: a text-input made while its client has the
  keyboard is entered on its own, not by re-entering the client's others.
- `src/wayland/text_input/text_input_handle.rs`: requests are tracked whether
  or not an input method runs; what goes to the method still needs one.
- `src/wayland/input_method/input_method_handle.rs`: an input method that
  goes away leaves the focused field entered.

Remove this once smithay enters text-inputs on keyboard focus alone.

## A toplevel icon outlives the objects that carried it (`xdg_toplevel_icon_v1`)

The protocol says a client may destroy its `xdg_toplevel_icon_v1` right after
`set_icon`, and its buffers once the icon object is gone, and that the
toplevel keeps the icon either way. This release kept the protocol objects in
the surface's cached icon state instead, so an icon read after the client
freed them found dead objects (and panicked), and never survived the commit
that makes it current. Freeing the buffer also disconnected the client: the
icon's buffer-destruction hook could not be unregistered.

- `src/wayland/shm/mod.rs`: `ShmBufferUserData::remove_destruction_hook`
  removed the first hook whose id *differed* from the one asked for, leaving
  every hook in place.
- `src/wayland/xdg_toplevel_icon.rs`: assigning an icon snapshots what it
  holds — `ToplevelIconData` (its name) with a `ToplevelIconBuffer` per size
  and scale, each carrying the pixels copied out of the client's pool, the
  format, size and stride. `ToplevelIconCachedState` keeps that snapshot
  (shared as an `Arc`, so the commit copies a pointer) rather than the
  protocol object, so `icon()`, `icon_name()` and `buffers()` keep answering
  after the client freed the icon and its buffers. An icon with neither a
  name nor pixels resets the toplevel's icon, as the protocol says a null one
  does. `set_name`/`add_buffer` stop after the `immutable` error instead of
  going on to change a frozen icon, and a replaced buffer that is already
  gone no longer panics the deduplication.

Remove this once a smithay release keeps a toplevel's icon once it is set,
copied out of the client's pool.

## An input method composes for the compositor's own fields (`input-method-v2`)

A text field is not always a client's: the session draws its own (a toolkit
surface it hosts, the desk's search box). Its input method has to compose
there too, and its candidate window has to stand where that field's caret is.

- `src/wayland/input_method/mod.rs`: `InputMethodHandler` gains
  `commit_to_host(&mut self, text: &str)` and
  `preedit_to_host(&mut self, text: &str, cursor_begin: i32, cursor_end: i32)`,
  both defaulting to nothing. `CommitString` and `SetPreeditString` call them
  when no `text-input-v3` client holds the active field, instead of dropping
  what the method composed.
- `src/wayland/input_method/input_method_handle.rs`:
  `InputMethodHandle::activate_without_text_input(state, rect)` activates the
  method for a field the compositor serves: the reported rectangle becomes
  the caret the popup follows, and a popup that stands already is registered
  again, parentless, at that caret (its parent is a client's surface, which
  the field is not). `has_instance`, `set_text_input_rectangle`,
  `activate_input_method` and `deactivate_input_method` are public, so a
  compositor can ask whether a method is connected, follow a caret the
  person moves and end a composition when the field goes away.
  Deactivating dismisses the popup and clears its parent.
- `src/wayland/input_method/input_method_popup_surface.rs`: a popup that is
  created tells the method the caret the compositor already knows, so a field
  whose caret was reported before the popup existed places it correctly.

## The keys an input method gives back reach the compositor's router
(`virtual-keyboard-v1`)

smithay forwarded a virtual keyboard's keys straight to the focused
`wl_keyboard`, so a compositor that serves fields itself never saw them: an
engine could not type into the field it was composing in.

- `src/wayland/virtual_keyboard/mod.rs`:
  `VirtualKeyboardHandler::virtual_key(&mut self, key, pressed, time) -> bool`
  defaults to `false`.
- `src/wayland/virtual_keyboard/virtual_keyboard_handle.rs`: after the keymap
  is current, `Key` asks the handler first; `true` means the compositor
  routed the key itself, and only `false` falls back to upstream's forwarding
  to the focused `wl_keyboard`. The handler gets the key in the protocol's
  codes, 8 above the evdev ones a client maps from the same keymap.

Remove both once smithay lets a compositor serve its own text fields through
an input method and route a virtual keyboard's keys through its own input.

## A mode set does not hold the compositor's thread (`drm`)

Setting a mode is a state commit, and this release makes one blocking: the
atomic commit returns once the driver's commit tail ran, and that is where the
link to the display trains — seconds when a sink's training fails and the
driver waits its timeout out. The caller is the compositor's event loop, so
every other display kept scanning out the frame it had, and no input was read,
until it returned. The crtc also forgot the commit it still had: `commit_frame`
dropped the frame it committed instead of tracking it, so nothing knew a
commit was in flight.

- `src/backend/drm/surface/atomic.rs`: one `commit_flags(event)` for state
  commits, `ALLOW_MODESET | NONBLOCK` plus `PAGE_FLIP_EVENT` when the caller
  waits for the commit, used by `commit` and by `clear_state`. Page flips
  already queued theirs.
- `src/backend/drm/compositor/mod.rs`: `commit_frame` asks for the event, and
  `PendingFrame`/`QueuedFrame` hold `Option<U>`, so a frame committed without
  user data is tracked like one queued with it. `frame_submitted` retires it
  on its event, no second commit goes to that crtc before it, a frame
  committed while one is in flight waits for the same event, and
  `reset_state` drops both because the state they were committed against is
  read back again.

A commit the driver fails after the call returned cannot be reported to the
caller; whoever commits owns that. Tilo's DRM backend judges a commit by the
vblank and the kernel state that follow it, and redoes the modeset once. The
backend only creates atomic surfaces; the legacy surface keeps
`drmModeSetCrtc`, which is blocking by API.

Remove this once upstream queues its state commits and tracks them, or reports
which commit is still in flight.

## An X11 display that outlives its server (`XWaylandListener`)

One display number serves one workspace, and a workspace runs an X server
only while one of its apps does: the compositor binds the workspace's display
at its first launch, hands `DISPLAY` to the apps it starts there, and starts
Xwayland when one of them actually connects. `spawn` cannot express that,
because it binds the display's sockets and the server in one call, so a
server exists from the moment the display does.

- `src/xwayland/xserver.rs`: `XWaylandListener` — the sockets and lock of a
  display, listening, with no server on them (`prepare`, `display_number`,
  `sockets`); clients that connect queue until a server accepts them.
  `XWayland::spawn_on` spawns on a listener's sockets, which stay the
  caller's, so the display serves the next server after this one stops.
  `spawn` is `prepare_x11_sockets` + `spawn_on_sockets`, the shared body, and
  `Instance` holds `Option<X11Lock>` and its display number instead of the
  lock's.
- `src/xwayland/mod.rs`: exports `XWaylandListener`.

The listening sockets need a backlog that holds the clients a display queues
before its server runs, and the readiness pipe must read as the server going
away when it closes it:

- `src/xwayland/x11_sockets.rs`: `listen(&fd, 128)` instead of the default
  backlog, which a burst of apps opening a display outran.
- `src/xwayland/xserver.rs`: the displayfd's `Ok(0)` (the pipe closed) is an
  `UnexpectedEof` error like any read error, instead of polling it forever.

A server whose Wayland connection closes keeps running: Xwayland prints
`failed to read Wayland events: Broken pipe` and stays in its event loop, so
a display that stopped serving would leave an idle Xwayland behind for as
long as the session lives.

- `src/xwayland/xserver.rs`: `Drop for XWayland` kills its process (the
  thread that waits on it reaps it) before it disconnects the Wayland client.

Remove these once upstream separates binding a display from spawning its
server, with a backlog deep enough for the clients waiting on one, and ends
its server when the Wayland connection does.

## Shared memory in the opaque 10-bit format

`Xrgb2101010` had no way in: its format table entry has no transparent
counterpart to look up, so `fourcc_to_gl_formats` answered `None` and a GLES
renderer refused the buffer, leaving a client that sent one with no window.
Its bytes also lie the other way round from the formats the table maps: DRM
keeps blue in the low bits where GL's `RGBA` reading of
`UNSIGNED_INT_2_10_10_10_REV` finds red, so uploading it as `Abgr2101010`
would paint it with two channels swapped.

- `src/backend/renderer/gles/format.rs`: `Xrgb2101010` maps to `RGB10_A2`,
  read as `RGBA` with `UNSIGNED_INT_2_10_10_10_REV`, and the shader's opaque
  variant supplies alpha 1, so the format's unused high bits are not read.
- `src/backend/renderer/gles/mod.rs`: it joins the shared-memory formats a
  GLES 3 renderer imports, and `swizzle_blue_first` swaps red and blue on the
  texture that carries it, in both import paths.

Remove this once upstream maps the opaque 10-bit RGB formats.
