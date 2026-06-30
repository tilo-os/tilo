# Virtual remote-seat design

> **Status:** spec + scaffolding. The compile-only `RemoteSeatRegistry` lands
> alongside this doc; event ingestion + cursor overlay tagging are follow-ups.

## Goal

When a host (Alice) shares a sandboxed app (Flatpak'd or any other Wayland
client adopted by the compositor) with a remote workspace member (Bob),
Bob's pointer / keyboard / touch events must drive the shared app *as if
they came from a local seat*. The Wayland client cannot tell — and must not
need to tell — whether the seat behind it is fed by libinput or by a remote
peer.

Two consequences fall out of "it's a real seat":

1. **Focus + input-event routing** reuse the standard smithay
   `PointerHandle` / `KeyboardHandle` paths. No bespoke "remote input"
   side-channel reaches the client.
2. **Cursor overlay rendering** still needs to distinguish Bob's pointer
   from Alice's — the compositor draws *both* visually but the focused
   client sees one merged stream. The per-event origin tag is the gate
   for that visual split.

This is the next step beyond `crate::network::streaming::InputEvent` +
`State::apply_remote_input`, which today injects pointer events into
Alice's **local** seat and so cannot draw Bob's cursor as a separate
overlay (the cursor jumps wherever the last event came from, regardless
of source).

## What's already in place

- A single local `Seat<State>` is constructed in
  `shell/src/compositor/state.rs:160` (also mirrored in
  `shell/src/network/wayland.rs:79`). Pointer + keyboard devices are
  added inline; the seat name is `"quick-desktop"`.
- `SeatState<State>` lives on `State::seat_state`
  (`shell/src/compositor/state.rs:84`). `SeatHandler` is implemented at
  `shell/src/compositor/state.rs:518`.
- The wire-format input vocabulary is
  `crate::network::streaming::InputEvent`
  (`shell/src/network/streaming.rs:98-109`):
  `PointerMotion { x, y }`, `PointerButton { button, pressed }`,
  `Key { keysym, pressed }`. Carried inside
  `NeighbourMsg::RemoteInput { peer, surface, event: Vec<u8> }`
  (`shell/src/network/protocol.rs:46-50`).
- `State::apply_remote_input`
  (`shell/src/compositor/state.rs:215-232`) gates inbound traffic
  through `ShareRegistry::accept_remote_input` and then injects pointer
  motion / button into the **local** seat via
  `PointerHandle::motion` / `PointerHandle::button`
  (`shell/src/compositor/state.rs:269-310`). The keyboard branch is
  currently a TODO (`shell/src/compositor/state.rs:312-316`).
- Remote-cursor rendering surface already exists:
  `shell/src/shell/remote_cursors.rs` (the `RemoteCursorOverlay`
  widget) reads `PresenceCtx::remote_cursors` and paints one
  caret + label chip per peer, clipped to the active tile's bounds.
  Cursor entries are upserted today from
  `shell/src/compositor/hosted.rs:222` (`presence_ctx.upsert_remote_cursor`).

## Design

### Per-user virtual seats

Introduce a new `RemoteSeat` value owned by the compositor's `State`
beside the local `seat: Seat<Self>`:

```rust
pub struct RemoteSeat {
    /// Stable identity carried in every event Bob produces.
    user_id: UserId,
    /// The smithay `Seat<State>` itself. Has its own pointer and
    /// keyboard `Handle` so events from Bob go through the same
    /// `PointerTarget`/`KeyboardTarget` plumbing as Alice's seat, but
    /// independently (separate focus, separate cursor position).
    seat: Seat<State>,
    /// Per-seat bookkeeping (last pointer position, etc.). Split into
    /// its own `RemoteSeatState` struct so the bookkeeping +
    /// registry shape is unit-testable without a smithay
    /// `DisplayHandle`.
    state: RemoteSeatState,
}
```

The collection lives in a registry keyed by `UserId`:

```rust
pub struct RemoteSeatRegistry {
    /// Production-side: smithay seats minted via `RemoteSeat::new`.
    seats: HashMap<UserId, RemoteSeat>,
    /// Always-populated: per-user bookkeeping. An entry here without
    /// a peer in `seats` means an event arrived before the State
    /// wrapper had a chance to lazily mint the smithay seat — the
    /// pointer position is still tracked so the very first event
    /// isn't dropped on the floor.
    states: HashMap<UserId, RemoteSeatState>,
}
```

The `State` struct gains one field:

```rust
pub remote_seats: RemoteSeatRegistry,
```

### Seat creation

A `RemoteSeat` is created **lazily** on the first event from a new
`user_id`. The smithay seat is constructed via
`seat_state.new_wl_seat(&display_handle, format!("remote-{user_id}"))`,
mirroring the local seat's bring-up — pointer is added unconditionally,
keyboard is added best-effort (smithay's keymap load may fail in
distrobox / headless test environments, which the local path already
tolerates with a `tracing::warn!`).

The seat name carries the user id so wayland clients that introspect
seat names (rare, but `weston-info` does) can tell remote seats apart
from the local one. This is informational only — the focused client
should treat them identically.

Smithay's `SeatState::new_wl_seat` makes the seat a global on the
display, so every existing wayland client receives a `wl_seat` `add`
event. That's intentional: it's how a newly-joined Bob shows up as
"another input source" to apps. Apps that bind multiple seats will get
a `pointer.enter` for the remote seat when Bob's pointer hovers their
surface.

### Per-event origin tagging

Three viable places for the tag, with trade-offs:

| Option | Cost | Pros | Cons |
|---|---|---|---|
| Per-seat lookup via `Seat::user_data()` | low | smithay-native, zero wire-format change | requires a back-reference from `PointerHandle::motion` callbacks to a `Seat` — smithay's `PointerTarget` impl has `seat: &Seat<D>` so we can read it. |
| Side-channel `Mutex<UserId>` written before each injection | low | simplest mental model | racy if event ingestion ever moves off the calloop thread |
| New variant on `InputEvent` carrying `user_id` | medium | self-describing wire format | duplicates information already present at the registry call site |

**Decision:** option 1 — stash a `UserId` clone on each `RemoteSeat`'s
`seat.user_data()` at construction time. The compositor's
`PointerTarget` / `KeyboardTarget` impls (or, in the overlay path,
the per-frame cursor-position drain) read it via
`seat.user_data().get::<UserId>()`. Wire format stays bit-for-bit
identical to today's `InputEvent`; the user_id rides at the *envelope*
layer (`NeighbourMsg::RemoteInput { peer, … }`) and is mapped to
`UserId` by the registry on ingest.

The cursor overlay drain is the cleanest place to surface the tag:
each frame, `State::frame_tick` walks
`RemoteSeatRegistry::seats` and writes one
`PresenceCtx::RemoteCursor` per `(user_id, last_pointer)` pair into
the existing overlay surface. The overlay widget already paints them
per-user with a stable color (from `ui::presence::user_color`).

### Event ingestion path

```
NeighbourMsg::RemoteInput { peer, surface, event }
  ↓ (actor.rs decode)
ShareRegistry::accept_remote_input(peer, surface)  ── existing gate
  ↓ true
State::accept_remote_input(user_id, surface, RemoteInputEvent)
  ↓
RemoteSeatRegistry::get_or_create(user_id, &mut seat_state, &dh)
  ↓
RemoteSeat::dispatch(event, &mut self, time, serial)
  ↓                        ↓
  PointerHandle::motion    KeyboardHandle::input
  (focused by smithay      (focused by smithay
   per seat)                per seat)
```

The seat's focus is owned by smithay; we don't try to slave Bob's focus
to Alice's. When Bob's pointer enters a surface the **shared app**
sees an enter on the remote seat. When Alice's pointer enters a
different surface the local seat sees an enter there. The focused app
processes both streams as if they came from two physical mice.

`PeerId` → `UserId` translation is the actor's responsibility, not the
seat's. The actor already has the `PeerId` → libp2p PeerId mapping;
`UserId` is the workspace-member identity layered on top
(`docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`).
For the v1 scaffolding the translation is `UserId(peer.to_string())`,
which is correct enough for tests and degrades to "one seat per peer"
in production until the real mapping arrives.

### `RemoteInputEvent` wire-format shape

The parallel `qos-collab-types` crate exposes a richer event vocabulary
than the existing `InputEvent`. The scaffolding accepts the assumed
shape:

```rust
pub enum RemoteInputEvent {
    PointerEnter { x: f64, y: f64 },
    PointerLeave,
    PointerMotion { x: f64, y: f64 },
    PointerButton { button: u32, state: ButtonState },
    PointerScroll { dx: f64, dy: f64 },
    KeyboardKey { keycode: u32, state: KeyState, modifiers: u32 },
}
```

`RemoteSeat::dispatch` maps each variant to the matching smithay call:

| `RemoteInputEvent` | smithay call |
|---|---|
| `PointerEnter` | `PointerHandle::motion` with the new focus target (focus resolution is the caller's problem — the registry doesn't know about the cluster tree) |
| `PointerLeave` | `PointerHandle::leave` |
| `PointerMotion` | `PointerHandle::motion` with `focus: None` (smithay re-resolves) + update `last_pointer` |
| `PointerButton` | `PointerHandle::button` + `frame` |
| `PointerScroll` | `PointerHandle::axis` (`AxisFrame::new(time).value(Horizontal, dx).value(Vertical, dy)`) |
| `KeyboardKey` | `KeyboardHandle::input` — keymap translation deferred (see existing `InputEvent::Key` TODO at `shell/src/compositor/state.rs:312-316`) |

For the **scaffolding step**, `accept_remote_input` does not call any
of the above — it constructs the seat (so capabilities propagate to
clients) and stamps `last_pointer` on motion, but defers the actual
`PointerHandle` calls to a follow-up alongside the
`qos-collab-types` integration. This keeps the scaffolding compilable
while landing the registry shape.

### Conflict semantics

Pin the contract: **last-event-wins at the focused client, but cursors
render independently**.

- The focused client sees one logical pointer per seat — Alice's
  local seat keeps its smithay-managed pointer position; each remote
  seat keeps its own. Apps that only listen to seat 0 (typical) will
  only see Alice's events; cooperating apps (xdg-shell handshake
  already lists all seats) see both as a multi-mouse scenario.
- The compositor's cursor overlay paints all seats simultaneously.
  Alice's cursor is drawn from her local seat position; each remote
  seat's `last_pointer` becomes a `PresenceCtx::RemoteCursor` entry,
  picked up by `RemoteCursorOverlay::cursors_to_draw`.
- For windowed (non-shared) clients only Alice's seat exists — no
  visible change to non-shared workflows.
- Multi-user resize / drag: each seat carries its own button state,
  so two users can both hold left-button concurrently without the
  state machine getting confused. The cluster's tile-drag handler
  needs to namespace its drag-state by seat in a follow-up; that's
  outside this design's scope.

### Tear-down

A peer disconnects in one of three ways: explicit "leave workspace"
message, gossipsub heartbeat timeout, or socket EOF. All three funnel
into the actor's `PeerLeft` event. The shell's network handler
(`shell/src/shell/network.rs`) reacts by removing the peer's
`RemoteCursor` from `PresenceCtx`; on the seat side, the same path
calls `RemoteSeatRegistry::drop_user(user_id)`.

`drop_user` does:

1. Look up the seat. If absent, return — the peer never produced
   input.
2. `seat.remove_pointer()` and `seat.remove_keyboard()` so clients
   see the capability removal.
3. `display.remove_global(seat.global().unwrap())` so the
   `wl_seat` itself disappears from new bindings.
4. Drop the `RemoteSeat` from the registry's `HashMap`.

**Smithay limitation note:** smithay 0.7 does **not** expose
`SeatState::remove_seat`. The seat lives inside `SeatState::seats:
Vec<Seat<D>>` and is dropped only when the `SeatState` itself is
dropped. Today's workaround is steps 1-3 above — the seat is
inert (no devices, no global) from the wayland frontend's perspective.
The `SeatRc` arc behind it still consumes one `Vec` slot for the
process lifetime; in a long-running session with many remote joiners
this is a memory drift item, not a correctness bug. Upstream fix:
push a `SeatState::remove_seat` PR to smithay.

## Open questions

1. **Focus-on-enter**: when Bob's pointer enters a shared surface, do
   we want the remote seat's keyboard focus to follow automatically?
   Default smithay behaviour is no — keyboard focus is independent.
   For collaboration the "click-to-focus" expectation suggests yes
   for *Bob's seat* (the local seat's keyboard focus stays put).
   Need a UX decision before implementing.
2. **Multi-user drag** on the cluster tile-binding layer:
   `WorkspaceTree::start_drag(seat_id, …)` doesn't exist today; the
   drag state is global. Two users dragging concurrently will
   stomp each other. Out of scope here, but the seat-tagging this
   spec lands is the prerequisite.
3. **Keyboard layouts per remote user**: Bob may be on a Dvorak
   layout, Alice on QWERTY. Today the local seat's `add_keyboard`
   loads the system XKB config. For the remote seat we likely want
   to honour a keymap shipped in the join handshake, but the wire
   format for that is unstated.
4. **PeerId ↔ UserId mapping**: the actor knows `PeerId` (u64 hashed
   libp2p id). The seat layer wants `UserId` (string from
   `qos-collab-types`). Where does the translation live? Three
   candidates: actor (clean), shell::network (already does PeerId
   tracking), or a new `crate::collab::identity` module.
5. **Touch capability**: the design omits touch. Smithay's seat
   model supports it (`Seat::add_touch`), and `RemoteInputEvent`
   could grow `TouchDown / TouchUp / TouchMotion`, but Bob is
   almost certainly on a laptop. Worth confirming the v1 scope
   skips touch deliberately.
