# Legacy windows on the browser desktop

The existing authenticated relay session carries the native session's
`PermissionGrant`, `SurfaceStreamStart`, `SurfaceFrame`, and `SurfaceStreamEnd`
presence. A publishing device must be subscribed, currently certified and an
owner/editor with current room Write permission in a governed room. Its
attributed author and `source_peer` must
name that exact device. Only a surface explicitly granted to this host is
bridged into the room's existing `tilo-moq` live record tracks. Browser members
receive them with their normal room live grant; a browser cannot publish this
media framing through the validated live-ops ingress.

The relay sends an index and H.264 groups headed by a start and keyframe. It
retains the current sequence for late subscribers, including an unchanged
window's still picture. Per-stream sequence retention is 8 MiB, frame size is
4 MiB, metadata is bounded to 32 streams per device and 64 per room, and the
aggregate encoded replay cache is 64 MiB. Explicit grant revocation ends the
stream. Role loss or certificate revocation ends it on the 250 ms maintenance
observation; a lost session has a 15-second reconnect grace. Empty indexes
close browser windows, and the viewer also expires missing indexes. Dropping
the listener clears retained media and refuses late native publication/input.

The browser uses the shared desktop's `PeerWindow` tiles and content rectangles,
with a WebCodecs canvas over each tile. It letterboxes pixels and maps input
back to surface coordinates. Stream count, canvas pixels, reassembly, decoder
backlog and queued input are bounded. Unwatch cancels retries; stream removal,
member departure, peer replacement and mount cleanup release subscriptions and
decoders. Blur, pointer cancellation and hiding release held keys/buttons.

`POST /peer/v1/live/legacy-input` accepts postcard `InputRequest` with the
ordinary `x-tilo-session` token. An empty event list probes room eligibility.
Owners/editors with current room Write permission may forward input; viewers
and members subject to read-only capability restrictions cannot, including
after a role/policy change.
In an ungoverned workspace, only the publisher's own user can forward input.
The relay rewrites every surface-bearing event to the selected stream and
routes it to that device alone. A `204` acknowledges forwarding, not injection:
the native compositor's existing share registry and input firewall remain the
final gate (read-only, lock, kill switch, physical priority and private focus).
Besides pointer and keys, input carries the wheel (`PointerAxis`, at the
member's last pointer position), pasted text (`ClipboardText`, offered as the
session's clipboard before the paste chord) and `Close`: a member who drives
a window and closes its desktop window asks the session to close it, as a
person's close of its tab does. A watcher closing it closes only its view,
and the desktop does not reopen a window the person closed while it streams.

## Applications on a node

A node runs a session beside the relay (`run --session-device`); `POST
/peer/v1/node` names its device to members ([`NodeInfo`]). The page asks that
session for the room's `workspace:<room>` catalogue and lists its unconfined
applications (`applications.host-app`: Chromium, LibreOffice, …) and the Tilo
applications the page cannot run itself (`applications.app`: Terminal, …) in
desktop search and the navigation catalogue; search no longer calls those
"Linux and macOS only". Opening one invokes its `app.<id>`/`open` action on
the session, which launches it in the room: the relay governs the room but
runs no apps, so the session opens even a Tilo app itself rather than through
the governor. A headless session whose active room has a host paints and
encodes while that room shows such a window, so it arrives here as above,
beside the focused one.

## Explicit remaining gaps

- IME/text commit and touch input need native endpoints.
- This increment consumes consented H.264 surface streams; it creates no native
  capture/share endpoint. A session must already grant and stream the surface.
  There is no independent PNG/JPEG/image delivery endpoint or codec negotiation.
- These tests prove real relay HTTP/auth/presence, role changes, private input,
  and MoQ delivery/replay, plus the production browser adapter with a simulated
  DOM/WebCodecs. They do not constitute a Linux Wayland capture -> browser
  WebCodecs -> native seat proof. That requires an isolated Linux compositor,
  a consented legacy client and a browser supporting WebCodecs.

## Focused validation

Run plain Cargo with inherited budget/sccache settings, unsetting an inherited
`CARGO_TARGET_DIR` if present:

```sh
cargo test --locked -p tilo-relay-protocol --lib legacy_window
cargo test --locked -p tilo-relay --features web --lib web::
cargo test --locked -p tilo-relay --features web --lib rooms::tests::legacy_window_write_respects_current_owner_capability_restrictions
cargo test --locked -p tilo-browser-peer --test legacy_windows
cargo check --locked -p tilo-relay --features web --tests
cargo check --locked -p tilo-browser-runtime --target wasm32-unknown-unknown --lib
node --test crates/browser/tilo-browser-runtime/web/legacy-windows.test.mjs crates/browser/tilo-browser-runtime/web/peer.test.mjs
```

The viewer/reassembly/input mapping reuses pre-deletion PR #541 implementation
from commits `7578bc4de6` and `47a326a69d`; framing is now in
`tilo-relay-protocol`, and delivery uses `tilo-moq`/the current relay rather than
a second transport control plane.
