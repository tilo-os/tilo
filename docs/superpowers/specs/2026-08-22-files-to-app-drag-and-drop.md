# Files → app drag-and-drop (Wayland)

**Goal.** In the native compositor, a file dragged out of the built-in Files app and
released over a sandboxed `ui` app's window is delivered to that app as a drop, and the
word processor inserts a dropped image at the drop point. This is the demo beat
"ada drags an image from Files into the document" (`plan/yc-demo.md`, beat 5).

**Why it does not work today** (verified against `60cc7e01`):

1. Files is not a Wayland client. In native mode it is a widget hosted *inside the
   compositor process* (`shell/src/shell/mod.rs:93,156-168`, `AppKind::FileExplorer` in
   `shell/src/shell/state.rs:306`). Its drag is in-process only — `FileCommand::BeginDrag`
   (`apps/file-explorer/src/files_app.rs:947-957`) carries `drag_paths`, and `finish_drag`
   (`files_app.rs:1229-1250`) resolves the drop against the view's own folder rows and the
   sidebar. Nothing ever leaves the widget tree.
2. The compositor never starts a server-side drag. `impl ServerDndGrabHandler for State {}`
   is an explicit no-op with a comment saying qOS does not start those
   (`shell/src/compositor/selection.rs:806-815`). The implicit pointer grab
   (`PointerOwner::Chrome`, `shell/src/compositor/client_input.rs:86-125`) keeps the gesture
   on chrome from button-down to button-up, so the pointer never focuses a client during a
   Files drag.
3. The vendored winit's Wayland backend has no data-device code at all —
   `WindowEvent::{HoveredFile, DroppedFile, HoveredFileCancelled}` are emitted only by the
   X11, macOS and Windows backends (`vendor/winit/src/platform_impl/linux/wayland/` has zero
   `data_device` hits; X11 emits at `x11/event_processor.rs:547`). Every `ui` app is a winit
   Wayland client, so no app can receive a drop.
4. The word processor's drop handler already exists and is correct:
   `WindowEvent::DroppedFile(path)` seats the caret at the tracked pointer and calls
   `insert_image_from_path` (`apps/word-processor/src/widget.rs:5755-5777`, `:2222`), which
   reads the file and calls `insert_image_bytes` (`word_processor.rs:1409`). It simply
   never fires on Wayland.

**Non-goals.** Client→client DnD between two sandboxed apps (the compositor already relays
those via `ClientDndGrabHandler`, `selection.rs:778-804`); dragging *into* Files from a
client; drag icons composited from client surfaces (deliberately refused, `:780-790`);
drops onto the empty workspace (`shell/src/shell/workspace_drop.rs` is a separate,
unwired concept — leave it).

---

## Design

Three parts, each testable on its own. Build them in this order; the demo needs all three.

### Part A — compositor: promote a Files drag to a server-side Wayland drag

When a hosted Files drag leaves the Files tile and the pointer enters a **foreign** tile
(`client_input::foreign_tile_at`, `client_input.rs:368-380`), the compositor starts a
smithay server-initiated DnD:

```rust
smithay::wayland::selection::data_device::start_dnd(
    &dh, &seat, &mut state, serial,
    pointer.grab_start_data(),      // smithay-0.7.0/src/input/pointer/mod.rs:202
    None,
    SourceMetadata { mime_types, dnd_action: DndAction::Copy },   // data_device/source.rs:17
)
```

- `mime_types`: always `text/uri-list`; additionally the file's real mime for single-file
  drags (`image/png`, `image/jpeg`, …) — use the `infer`/`mime_guess` crates Files already
  depends on. Offering the bytes is what makes this work for a **confined** client: the
  word processor's sandbox (`apps/word-processor/qos-app.toml:40-52`, Landlock strict) does
  not see `/var/workspace`, and `documents_for_session()` is `host_paths()`
  (`shell/src/portal/mod.rs:642-644`), i.e. no runtime document publishing exists yet.
- Implement `ServerDndGrabHandler for State` (`selection.rs:806-815`): `send(mime, fd, seat)`
  writes either the `file://` URI list or the file bytes into `fd`, on a worker thread, the
  same way `serve_selection` serves the clipboard (`selection.rs:735-752`); `dropped`,
  `cancelled`, `finished` clear the compositor's drag state and tell the hosted Files widget
  to end its drag without moving anything (today `finish_drag` would try to resolve a folder
  target — add a `FileCommand::CancelDrag`/`EndDragExternally` or equivalent so Files does
  not act on a drop that landed elsewhere).
- Pointer ownership: the implicit grab is `PointerOwner::Chrome` for the whole gesture.
  The hand-off point is "Files has an active drag (`drag_paths` non-empty) **and**
  `foreign_tile_at(cursor)` is `Some`". At that point start the smithay grab; smithay then
  owns enter/motion/drop delivery to the client until release. Read `client_input.rs`
  around `:1469-1620` (per-tick routing, `current_pointer_target`) before touching it —
  this is the load-bearing part. Remote input (`network/input_firewall.rs`) must keep
  answering the same "what is under the point" question; don't fork the traversal.
- Drag feedback: the compositor already draws its own cursor and drag ghost for Files
  (`files_app.rs` ghost drawing around `:1536-1580`); keep drawing that over foreign tiles
  too, so the user sees the file travel.
- Expose the drag's state to the hosted Files widget the way `DropTargets`/`rename_rect`
  already share host-owned state (`apps/file-explorer/src/views/file_view.rs:78-98`), not via
  globals.

### Part B — vendored winit: Wayland data-device receive

Patch `vendor/winit` (already a fork; edition/version stay as they are). sctk 0.19 ships
`data_device_manager/{data_device,data_offer,data_source,read_pipe}.rs` — use it, do not
hand-roll the protocol.

- Add a `DataDeviceManagerState` to `WinitState` (`vendor/winit/src/platform_impl/linux/wayland/state.rs:41-79`)
  and one `DataDevice` per seat, created where `WinitSeatState` is created (`:147-151`).
- Implement sctk's `DataDeviceHandler` (`enter`, `leave`, `motion`, `drop_performed`,
  ignore `selection` — clipboard stays in `smithay-clipboard`, see `ui/src/clipboard.rs:1-40`)
  and `DataOfferHandler`.
- On `enter`: accept `text/uri-list` if offered, else the first `image/*`; set the DnD
  action to Copy; emit `HoveredFile(path)` per URI (for bytes offers emit `HoveredFile` with
  the spool path you will use on drop). Also emit `WindowEvent::CursorMoved` with the
  surface-local position — and again on every `motion` — because during a drag the
  compositor sends `wl_data_device.motion`, not `wl_pointer.motion`, and the word processor
  locates the drop from its tracked pointer (`widget.rs:5755-5764`). Mirror the existing
  emission in `seat/pointer/mod.rs:137-156`.
- On `drop_performed`: `receive` the accepted mime on a pipe (`read_pipe.rs`), read it to
  completion (worker thread; poll via the event loop's `calloop` source as sctk's examples
  do), then:
  - `text/uri-list` → one `DroppedFile(PathBuf)` per `file://` entry (percent-decode;
    ignore non-file schemes).
  - bytes → write to `$XDG_RUNTIME_DIR/winit-dnd-<n>.<ext>` (inside the sandbox, so the app
    can read it; ext from the mime) and emit one `DroppedFile` with that path.
  - then `finish()` the offer.
- On `leave` without drop: `HoveredFileCancelled`.
- Keep the patch minimal and self-contained in a new `seat/data_device.rs` (or similar);
  add the `delegate_data_device!` macro next to the existing `sctk::delegate_*` calls.

### Part C — word processor: nothing new, one cleanup

`DroppedFile` handling is done. Two touches:
- `insert_image_from_path` should `remove_file` a spool path that lives under
  `$XDG_RUNTIME_DIR` after a successful read (Part B leaves it behind otherwise). Do not
  delete paths outside the runtime dir.
- Confirm `HoveredFile` is ignored gracefully (it is today — no arm; `video_editor` has one
  at `apps/video_editor/src/app.rs:1884` if you want a hover affordance, optional).

---

## Tests (required — the repo's verification covenant applies, `CLAUDE.md` "Verification covenant")

- **Part A, unit:** a `State` with a hosted Files tab and one `TileContent::Foreign` tab;
  drive `BeginDrag` → pointer motion into the foreign rect → assert a DnD grab is active
  on the seat and `SourceMetadata.mime_types` contains `text/uri-list` (+ the image mime for
  a `.png`). Release outside any foreign tile → Files' own `finish_drag` path still runs
  (regression for the existing move-into-folder behaviour, `files_app.rs:3845` area tests).
- **Part A, fd:** call the handler's `send("text/uri-list", fd)` with a pipe and assert the
  bytes are `file:///…/photo.png\r\n`; same for `image/png` → the file's bytes.
- **Part B:** winit has no Wayland test rig; cover the pure parts — URI-list parsing/decoding
  and mime preference — as unit tests in the new module.
- **End-to-end, cross-process (the one that proves the beat):** spawn the compositor
  `--backend headless` with `QOS_TEST_CONTROL` (`scripts/headless-examples/spawn-headless.sh`),
  open Files on a temp dir containing a PNG, launch the word processor as a real Wayland
  client into the session (the launcher path, or a direct `WAYLAND_DISPLAY` spawn if the
  sandbox makes the test heavy — say which in the test), then `{"op":"drag", …}`
  (`shell/src/compositor/test_control.rs:63-227`) from the row's rect (`list_targets` /
  `click_named`) to the word-processor tile, `snapshot`, and assert the document now
  contains an image — read it back from the app, e.g. by having the app save (`Ctrl+S`
  to a temp `.odt`) and parsing the container for the embedded picture
  (`crates/doc/qdoc/src/codecs/odt/export.rs:138-160` shows where it lands). Pixel
  assertions on the snapshot are a bonus, not the proof.

Run scoped: `cargo test -p shell --lib <name>`, `cargo test -p file-explorer`,
`cargo test -p word-processor`. Don't run workspace-wide tests. `shell --lib` is ~950 tests;
name yours so they can be run alone.

---

## Constraints and gotchas

- Work in a worktree (`docs/BRANCHBOX.md`), never share `target/` between worktrees
  (`CLAUDE.md` "Build environment"). Plain `cargo`; do not set `RUSTC_WRAPPER`/`CARGO_TARGET_DIR`.
- Editions: `shell`/`ui` are 2024, apps are 2021, `vendor/winit` keeps its own — don't normalise.
- Adding fields to `Workspace`/`Strip`/`TileContent` ripples into every by-field test fixture
  (`CLAUDE.md` "Where state lives"). Prefer new state on `ClientInput` / `SelectionStore`.
- `Cargo.lock` is committed and must stay consistent; if sctk features need enabling in
  `vendor/winit/Cargo.toml` (`data_device_manager` is part of sctk core, so likely none),
  read the lock diff before committing.
- `ClientDndGrabHandler::started` records `dnd_active` (`selection.rs:778-793`); give the
  server-side grab the same bookkeeping so the two cannot overlap.
- Security: the source is compositor chrome acting on the user's explicit gesture, so
  serving bytes of a file the user dragged is the same trust as the file chooser
  (`shell/src/portal/file_chooser.rs:155-225`). Do **not** additionally widen the target
  app's grants (`Workspace::grants`) because of a drop.
- Leave `quickdesktop://`, `workspace_drop.rs`, and `ui/src/dnd` (in-process tab drags)
  alone — different concepts.

## Done when

1. On a real nested session (`cargo run -p shell -- --backend winit`, Files + a launched
   word processor side by side), dragging `photo.png` from Files onto the document inserts it
   at the drop point.
2. The cross-process e2e above passes and is not `#[ignore]`d.
3. Existing Files drag-to-folder and drag-to-Favourites tests still pass.
4. Commit per part (A, B, C + e2e), messages in the repo's `area: what` style.
