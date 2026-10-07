# On-screen keyboard architecture

The on-screen keyboard (OSK) is a session service, not an ordinary app and not
part of the Settings process. Settings stores the user's intent in
`accessibility.on_screen_keyboard`; the compositor watches that setting and
owns the service lifecycle, protocol authority, placement, and visibility.

## Trust boundary

The compositor exposes `text-input-v3` on the normal Wayland display. That is
safe for applications: Smithay associates each text-input object with the
seat's focused surface, so a client can only describe its own focused
field.

`input-method-v2` and `virtual-keyboard-v1` are different: either can inject
input into another application. Their globals are filtered to an immutable
`ClientState::osk` credential. The credential is assigned only to connections
accepted on a dedicated, randomly named OSK display socket which is not
exported in the session environment. The acceptor also checks the kernel's
`SO_PEERCRED` UID/PID belongs to the compositor-created service. The launcher
is put into a new process group (`pgid == launcher pid`), rather than inheriting
the compositor/session group, and the connecting process must both be in that
group and have the launcher as an ancestor in `/proc`. A same-user sibling
cannot obtain authority by discovering the socket or joining/spoofing a group;
the path name is not treated as a secret capability. The compositor launches
the installed, signed `so.tilo.osk` service through the ordinary Tilo sandbox
launcher and binds that socket as its Wayland display.

An app-id is display metadata, not authentication. Tilo additionally requires
the exact `so.tilo.osk` app-id before promoting its toplevel into the OSK
overlay, but app-id alone never reveals an input global. A public-display
client which copies the id remains an ordinary tiled application.

Turning the preference off first closes the private socket accept gate and then
terminates the launcher's complete process group, with bounded escalation. A
crashed service is retried with backoff only while the preference remains on.

A normal verified package update also updates the running keyboard. The session
follows the existing installed-package catalogue watcher and its bounded
freshness fallback, comparing the installed keyboard row rather than another
application's update. Before replacement it revokes the old private socket's
accept authority and disconnects the old trusted client. Its supervisor then
terminates and reaps the entire old process group with bounded escalation;
only completed cleanup permits the replacement to acquire socket authority.
The replacement uses the ordinary sandbox launcher and the installed manifest's
verified runtime, preserving the preference and the focused text input. No
restart action or additional global input permission is needed.

This lifecycle belongs to the running compositor. The initial greeter uses Cage
before the compositor starts; updating this service does not provide a Cage
input method or change greeter authentication.

## Text lifecycle

Editable Tilo widgets explicitly opt into IME handling. The UI runtime follows
focus and sends winit the field purpose and cursor rectangle before enabling
IME. Winit emits `text-input-v3` enable/commit/disable requests. Smithay couples
that text input to seat keyboard focus and activates the single input-method
client. Purpose changes select the OSK layout. The compositor paints and
hit-tests the OSK only while that focused text input is active; the keyboard
also paints an empty frame while inactive. This server-side decision is
intentional because Wayland xdg-toplevels have no client-side map/unmap API.

Printable taps, swipe results, and suggestion choices use
`input-method-v2.commit_string`, then commit. Backspace uses the separately
filtered virtual keyboard because winit does not currently implement
`delete_surrounding_text`. That fallback is unavailable to public clients.

## Surface and input behavior

The OSK uses a normal xdg-toplevel so it stays compatible with winit. The
compositor recognizes it only after both trust checks, removes it from pending
tile adoption, and mirrors it as a compositor-owned floating surface. It is
centered near the bottom of the output containing the focused application,
with bounded margins and height, and is painted above application content.

Touch and pointer events are transformed into that surface's local coordinates.
Pointer focus may move to the keyboard, but clicking it never raises the
surface or changes seat keyboard focus; otherwise the text input would
deactivate before the key could be committed. An inactive OSK is excluded from
painting and hit testing even while its service connection stays alive.

## Deliberate limits

- Winit currently exposes only Normal, Password, and Terminal purposes. Tilo
  native widgets can therefore select those today. Other toolkits which emit
  richer text-input-v3 purposes still reach the OSK's numeric, phone, URL, and
  email layouts. Number widgets validate input even though they request the
  Normal purpose until winit exposes Numeric.
- Smithay accepts input-method candidate popup surfaces, but the compositor
  does not paint or position them yet. The Tilo keyboard's own suggestion strip
  is part of its main overlay and works without popup surfaces.
- Placement chooses among Tilo's current horizontally arranged output model.
  Per-output rotation and non-horizontal output origins must land with the
  broader display-layout work.
- The protocol and policy paths have focused unit tests. A real touch device,
  third-party toolkit client, and packaged signed-service installation still
  require end-to-end hardware/session testing.
