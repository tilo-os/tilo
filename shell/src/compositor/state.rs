//! Smithay `State` struct — Phase 2 of the real Wayland compositor plan.
//!
//! Smithay follows a "one big state" convention: a single `State` struct owns
//! every protocol global (compositor, xdg-shell, shm, seat, output) and is
//! threaded through the calloop event loop. Each Wayland protocol attaches to
//! the state via the corresponding `delegate_*!` macro, which expands into
//! `Dispatch` / `GlobalDispatch` impls that route messages to the matching
//! `*Handler` trait.
//!
//! Phase 2 only sets up the type plumbing — `compile-only`. Actual rendering,
//! seat input forwarding and surface management happen in Phase 4+.
//!
//! References: smithay 0.7 `examples/minimal.rs` and `examples/compositor.rs`,
//! and `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md` Phase 2.
//!
//! ## Module layout
//!
//! - [`State`] — the giant state struct that lives for the entire compositor
//!   lifetime.
//! - [`ClientState`] — per-client data attached at `insert_client` time. Holds
//!   the smithay `CompositorClientState` so the compositor handler can hand it
//!   back from `client_compositor_state`.
//!
//! Trait impls (`CompositorHandler`, `XdgShellHandler`, …) and the
//! `delegate_*!` macros are wired up below.

use std::sync::Arc;

use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::utils::Serial;
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Format, Modifier};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    self as wl_compositor, CompositorClientState, CompositorHandler, CompositorState,
};
use smithay::wayland::dmabuf::{
    DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::{
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_seat, delegate_shm,
    delegate_virtual_keyboard_manager, delegate_xdg_shell,
};

use crate::collab::flatpak_share::SharedFlatpakSession;
use crate::compositor::remote_seat::RemoteSeats;
use crate::compositor::surface_readback::SurfaceTracker;
use crate::compositor::tab_binding;
use crate::network::protocol::{PeerId as MsgPeerId, SurfaceId};
use crate::wayland::color_management::ColorManagerState;
use crate::network::streaming::{InputEvent, RemoteInputSink, ShareRegistry};
use crate::shell::state::WorkspaceTree;
use smithay::backend::input::KeyState;
use std::collections::HashMap;

/// The compositor's process-wide state.
///
/// This is the single value the calloop event loop hands to every callback,
/// so every Wayland global gets unsynchronised mutable access without `Rc` /
/// `Arc` plumbing.
///
/// Future phases will add: a `Space` for window placement, a renderer handle,
/// the cluster/sandbox model, libp2p collaboration plumbing, etc. For Phase 2
/// we only need the protocol-globals and the loop / display handles.
pub struct State {
    /// Calloop loop handle — used in later phases to register input devices,
    /// session signals, libinput sources, etc.
    pub loop_handle: LoopHandle<'static, Self>,
    /// `wayland-server` display handle. Cloneable; used to insert new clients
    /// when the listening socket accepts a connection.
    pub display_handle: DisplayHandle,
    /// `wl_compositor` + `wl_subcompositor` global state.
    pub compositor_state: CompositorState,
    /// `xdg_wm_base` global state — toplevels and popups.
    pub xdg_shell_state: XdgShellState,
    /// `wl_shm` global state — software buffer support. Empty extra-format
    /// list for now; `wl_shm` always advertises ARGB8888 + XRGB8888.
    pub shm_state: ShmState,
    /// `zwp_linux_dmabuf_v1` global state — enables clients to attach
    /// GPU-allocated (usually GBM) buffers to their surfaces. When
    /// present, the encoder path can `av_hwframe_map` a shared
    /// surface's committed buffer straight into a VA-API surface with
    /// zero CPU-side copy.
    ///
    /// The advertised format list is the intersection of "what the
    /// compositor's wgpu/vello backend will accept for sampling" and
    /// "what a hardware H.264 encoder can consume as
    /// `AV_PIX_FMT_DRM_PRIME`". BGRA/XRGB8888 covers the common
    /// client-side render output; NV12 matches VA-API's native encode
    /// input; P010 is retained for eventual HDR paths but currently
    /// falls back to SHM at the encoder side.
    pub dmabuf_state: DmabufState,
    /// The advertised `zwp_linux_dmabuf_v1` global handle. Held for
    /// the process lifetime — dropping it would revoke the global
    /// from clients that already bound it.
    pub dmabuf_global: DmabufGlobal,
    /// `wl_seat` collection. Phase 2 doesn't add a seat yet; phase 4 will.
    pub seat_state: SeatState<Self>,
    /// `wl_output` + xdg-output manager. Phase 2 advertises the manager
    /// global so xdg-output-aware clients (e.g. Mutter's wl-roots tools) can
    /// bind without erroring; concrete outputs are added in phase 4.
    pub output_manager_state: OutputManagerState,
    /// Persisted workspace tree. Phase 6 of the compositor plan adopts
    /// foreign-client toplevels into this tree as
    /// [`crate::shell::state::TileContent::Foreign`] tabs (see
    /// [`crate::compositor::tab_binding::adopt_toplevel`]).
    pub workspace_tree: WorkspaceTree,
    /// Peer-sync share registry — Phase 4 Task 4.4. Tracks which peers
    /// have been granted streaming + input access to which surfaces, and
    /// the per-peer `read_only` flag. Inbound
    /// [`crate::network::NeighbourMsg::RemoteInput`] events are filtered
    /// through this registry before they reach the seat.
    pub share_registry: ShareRegistry,
    /// Last `RemoteInput` event that survived the share-registry gate.
    /// Read by integration tests that want to assert seat-equivalent
    /// injection happened, without spinning up a real `Seat<State>`.
    /// v2 replaces this with the proper smithay seat-call path.
    pub last_remote_input: Option<(MsgPeerId, SurfaceId, InputEvent)>,
    /// Per-foreign-surface RGBA staging — Peer-sync Phase 4 v2. The
    /// compositor's [`CompositorHandler::commit`] hook calls
    /// [`SurfaceTracker::on_commit`] for every committed surface whose
    /// id is listed in
    /// [`crate::shell::state::Workspace::shared_surfaces`]; the
    /// streaming task drains via [`SurfaceTracker::take_latest`] per
    /// encode tick.
    pub surface_tracker: SurfaceTracker,
    /// The compositor's `wl_seat` handle with a pointer device
    /// already registered. Constructed in [`State::new`] alongside
    /// the rest of the protocol globals. Peer-sync's
    /// [`Self::apply_remote_input`] injects pointer motion / button
    /// events into this seat (D-1 fix) so remote-input traffic
    /// actually drives the local seat instead of being logged and
    /// discarded.
    ///
    /// Keyboard injection is staged behind a follow-up: smithay's
    /// keyboard handle needs a configured keymap, and the v1 path
    /// can ship without it. Pointer motion/button covers the
    /// foreign-tab remote-control use case the Phase 4 spec calls
    /// out.
    pub seat: Seat<Self>,
    /// Monotonic counter for input serials. Smithay's input handles
    /// require fresh `Serial`s per event; we maintain one per state
    /// instance so the seat injection in `apply_remote_input` doesn't
    /// reuse old serials (which would make wayland clients ignore the
    /// motion / button).
    pub serial_counter: u32,
    /// Wall-clock anchor for input timestamps. Pointer events ship
    /// with a `time: u32` in milliseconds since this instant.
    pub start_time: std::time::Instant,
    /// Per-peer virtual seats.
    ///
    /// [`InputEvent::PointerEnter`] auto-grabs the peer's keyboard
    /// focus onto the target surface for downstream `Key` events;
    /// [`InputEvent::PointerLeave`] releases the grab. See
    /// [`crate::compositor::remote_seat::RemoteSeats`] for the full
    /// contract. Multi-user typing interleaves at the physical seat
    /// level — this map is bookkeeping for the per-peer focus state,
    /// not a per-peer independent input pipeline (v1).
    pub remote_seats: RemoteSeats,
    /// Optional surface-id → WlSurface resolver installed by the
    /// backend (`native::run` wires this to the compositor's
    /// wayland-server tracker).
    ///
    /// Populated at boot from the same map the render layer uses to
    /// find surfaces by their protocol id; consulted by
    /// [`Self::apply_remote_input`]'s keyboard-focus flip on
    /// `PointerEnter`. When `None` (headless tests) the focus flip is
    /// a bookkeeping-only no-op — the peer's `RemoteSeat` still
    /// records the target so tests can observe the focus intent.
    pub surface_resolver: Option<Box<dyn Fn(SurfaceId) -> Option<WlSurface> + Send + Sync>>,
    /// Active share sessions keyed by their target `SurfaceId`.
    ///
    /// The [`CompositorHandler::commit`] hook consults this map on
    /// every surface commit — when the committed surface is in the map,
    /// the session's `encode_next_frame` is pumped so a freshly-drawn
    /// buffer flows through the encoder → substream pipeline on the
    /// same tick it landed in the compositor. Without this hook the
    /// encoder would wait for the render loop's next damage pass,
    /// which introduces a per-frame stall.
    ///
    /// Sessions are held by `Arc` because
    /// [`crate::collab::flatpak_share::SharedFlatpakSession`]'s
    /// `encode_next_frame` takes `&self`; the `Arc` also lets the
    /// share manager keep an independent handle for the stop path.
    pub active_sessions: HashMap<SurfaceId, Arc<SharedFlatpakSession>>,
    /// `wp_color_manager_v1` state — advertised primaries / transfers /
    /// features, plus the registered global id. HDR clients bind the
    /// global, query supported capabilities, and attach parametric
    /// image descriptions to their surfaces; the flatpak-share
    /// encoder reads the current per-surface description through
    /// [`crate::wayland::color_management::surface_current_colorimetry`].
    pub color_manager: ColorManagerState,
}

impl State {
    /// Build a fresh `State` with all protocol globals registered.
    ///
    /// Each `*::new::<Self>` / `*::new_with_xdg_output::<Self>` call advertises
    /// the corresponding Wayland global on the display. The actual `wl_seat`
    /// is *not* created yet — that happens in phase 4 when input plumbing
    /// arrives.
    pub fn new(loop_handle: LoopHandle<'static, Self>, display_handle: DisplayHandle) -> Self {
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let mut seat_state = SeatState::new();
        let output_manager_state =
            OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        // Advertise a small, deliberately safe DMABUF format set — the
        // intersection of "the compositor can render / sample this"
        // and "a downstream H.264 encoder can consume it directly".
        // Every format is offered under LINEAR only (implicit modifier
        // negotiation would require an EGL-side format probe we don't
        // do here). Vendor-specific tiling / DCC modifiers are a
        // follow-up once the wgpu backend exposes the driver's
        // modifier list.
        //
        // Format rationale:
        // - BGRA/XRGB8888: client-side RGBA output (most GPU-rendered
        //   toplevels).
        // - NV12: VA-API's native H.264 encode input; a GPU that
        //   composites into NV12 skips the RGBA→NV12 swscale on the
        //   encoder side.
        // - P010: 10-bit HDR; kept in the advertisement so a future
        //   HDR client can attach one, even though the VA-API path
        //   currently falls back to SHM for P010.
        let dmabuf_formats: Vec<Format> = [
            Fourcc::Argb8888,
            Fourcc::Xrgb8888,
            Fourcc::Abgr8888,
            Fourcc::Xbgr8888,
            Fourcc::Nv12,
            Fourcc::P010,
        ]
        .into_iter()
        .map(|code| Format {
            code,
            modifier: Modifier::Linear,
        })
        .collect();
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global =
            dmabuf_state.create_global::<Self>(&display_handle, dmabuf_formats);

        // D-1 fix: register a real seat with a pointer device so
        // `apply_remote_input` has something to inject into. The seat
        // name matches the wayland.rs sibling so multi-binary apps see
        // a consistent identifier. Keyboard injection ships in the
        // follow-up — registering the device here would force a keymap
        // load (`add_keyboard(Default::default(), 200, 25)`) we
        // intentionally defer.
        let mut seat = seat_state.new_wl_seat(&display_handle, "quick-desktop");
        seat.add_pointer();
        // W7 text injection: a keyboard on the seat is required for the
        // on-screen keyboard's `virtual-keyboard-v1` client (and any future
        // `input-method`/`text-input` IME) to deliver keys to the focused
        // surface. Default xkb config; 200ms repeat delay, 25/s rate. Guarded
        // so a keymap-load failure degrades gracefully instead of panicking.
        if let Err(e) = seat.add_keyboard(Default::default(), 200, 25) {
            tracing::warn!("seat: failed to add keyboard (text injection disabled): {e}");
        }
        // W7: advertise the virtual-keyboard manager global so the OSK can
        // inject text. Keys flow through the seat keyboard above to the
        // focused surface; the global self-registers in the display.
        VirtualKeyboardManagerState::new::<Self, _>(&display_handle, |_client| true);
        // Advertise the `wp_color_manager_v1` global. Constructing
        // before the state literal below because `display_handle` is
        // moved into the struct.
        let color_manager = ColorManagerState::new::<Self>(&display_handle);

        Self {
            loop_handle,
            display_handle,
            compositor_state,
            xdg_shell_state,
            shm_state,
            dmabuf_state,
            dmabuf_global,
            seat_state,
            output_manager_state,
            workspace_tree: WorkspaceTree::bootstrap_default(),
            share_registry: ShareRegistry::new(),
            last_remote_input: None,
            surface_tracker: SurfaceTracker::new(),
            seat,
            serial_counter: 0,
            start_time: std::time::Instant::now(),
            remote_seats: RemoteSeats::new(),
            surface_resolver: None,
            active_sessions: HashMap::new(),
            color_manager,
        }
    }

    /// Register a share session so the [`CompositorHandler::commit`]
    /// hook pumps its `encode_next_frame` on every commit of the
    /// target surface. Replaces any prior session for the same
    /// surface — the old session's encoder is dropped.
    pub fn register_share_session(&mut self, session: Arc<SharedFlatpakSession>) {
        let surface = session.surface_id;
        self.active_sessions.insert(surface, session);
    }

    /// Deregister a share session by surface id. No-op if the surface
    /// isn't registered. Called when the share ends or the target
    /// surface is destroyed.
    pub fn unregister_share_session(&mut self, surface: SurfaceId) {
        self.active_sessions.remove(&surface);
    }

    /// Pump the share-session encoder for `surface_id` on a fresh
    /// commit. Called from [`CompositorHandler::commit`] after
    /// [`SurfaceTracker::on_commit`] has staged the buffer bytes.
    ///
    /// - Returns `false` when no session is registered for
    ///   `surface_id`, or when the tracker slot is empty (no staged
    ///   frame this commit — the shared surface committed damage
    ///   without a new buffer attach, e.g. cursor motion).
    /// - Returns `true` when the session accepted the deposit and the
    ///   encode-next-frame tick ran. That does NOT guarantee a frame
    ///   was emitted — the throttle may have rejected the tick or the
    ///   encoder may have failed; the frame's absence is surfaced
    ///   through the session's per-subscriber substream state.
    ///
    /// Split out from `commit` so integration tests can drive the
    /// tick path without spinning up a real `WlSurface`.
    pub fn tick_share_session_for_surface(&mut self, surface_id: SurfaceId) -> bool {
        let Some(session) = self.active_sessions.get(&surface_id).cloned() else {
            return false;
        };
        // DMABUF-direct path takes priority when the compositor's
        // commit hook stashed a Dmabuf on this tick — `take_latest_dmabuf`
        // drains it so a re-tick without a fresh commit finds no
        // stale entry. Failing over to SHM if no DMABUF is present
        // covers the mixed-mode case (a client that briefly attaches
        // a SHM buffer between DMABUF commits).
        if let Some(dmabuf) = self.surface_tracker.take_latest_dmabuf(surface_id) {
            session.deposit_captured_dmabuf(dmabuf);
            let emitted = session.encode_next_frame(std::time::Instant::now());
            if emitted.is_some() {
                // Encoder actually consumed the DMABUF. Only *now* do
                // we bump the tracker's `dmabuf_direct_count` — the
                // metric reflects real zero-copy, not "the buffer was
                // DMABUF-shaped but the encoder rejected it and we
                // fell back to SHM on the following commit".
                self.surface_tracker.note_capture_path(
                    surface_id,
                    crate::compositor::surface_readback::CapturePath::DmabufDirect,
                );
            }
            return true;
        }
        let Some(frame) = self.surface_tracker.peek_latest(surface_id) else {
            return false;
        };
        session.deposit_captured_rgba(frame.rgba);
        let _ = session.encode_next_frame(std::time::Instant::now());
        true
    }

    /// Mint a fresh `Serial` for input events. Wayland clients expect
    /// monotonically increasing serials; this counter never wraps in
    /// practice (a 32-bit space at 1 kHz is 49 days of input traffic).
    fn next_serial(&mut self) -> Serial {
        self.serial_counter = self.serial_counter.wrapping_add(1);
        Serial::from(self.serial_counter)
    }

    /// Apply a remote-input event from `peer` targeting `surface`.
    ///
    /// Phase 4 Task 4.4. The actor's event loop calls this from its
    /// [`crate::network::NeighbourMsg::RemoteInput`] handler. The
    /// per-share `read_only` flag in [`Self::share_registry`] is the
    /// only gate — when set, the event drops here and never reaches the
    /// smithay seat.
    ///
    /// Seat injection itself is deferred to the v2 path once the
    /// compositor's `Seat<State>` is wired into this struct. For v1 we
    /// rely on the routing decision made here (verified by
    /// `streaming::tests::remote_input_blocked_when_read_only`) and
    /// surface the event into [`tracing::debug`] so production logs show
    /// the round-trip.
    pub fn apply_remote_input(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: InputEvent,
    ) -> bool {
        // We can't pass `self` twice (once as `&ShareRegistry` via
        // `&self.share_registry` and once as `&mut RemoteInputSink`), so
        // inline the routing decision: read the gate from the registry,
        // drop if blocked, otherwise call `RemoteInputSink::apply` on
        // ourselves. Equivalent to `ShareRegistry::route_to`, just
        // expressed without an aliasing-prone helper.
        if !self.share_registry.accept_remote_input(peer, surface) {
            return false;
        }
        RemoteInputSink::apply(self, peer, surface, event);
        true
    }
}

// `RemoteInputSink` impl: receives the gated events from
// `ShareRegistry::route_to`.
//
// The pointer path drives the real smithay `Seat<Self>` (constructed in
// `State::new`) via `PointerHandle::motion` / `button`, matching the
// production shape used by `network::wayland::WaylandServer::handle_input`.
//
// Keyboard injection: `InputEvent::Key { keysym, pressed, modifiers }` fires
// `KeyboardHandle::input`. The wire `keysym` is treated as a raw keycode
// (see the `InputEvent` doc for the compat rationale). Modifier state
// flows via explicit modifier-key press/release events the peer sends —
// smithay's `KeyboardInternal::key_input` maintains modifier state from
// the keycode stream, so shifted / ctrl'd key runs work as long as the
// peer sends the modifier keys in order. The `modifiers` bitfield is
// recorded into `last_remote_input` for diagnostics and per-user "typing"
// overlays.
//
// Focus grab: `PointerEnter { surface }` grabs keyboard focus onto the
// peer's virtual `RemoteSeat` — physically, the compositor's single
// `Seat<State>`'s `KeyboardHandle::set_focus` is flipped to point at the
// target `WlSurface` (resolved via `surface_resolver`). `PointerLeave`
// releases the grab. When the resolver returns `None` (headless tests,
// or a peer targeting a surface the local compositor doesn't own), the
// focus flip is a bookkeeping-only no-op and the peer's `RemoteSeat`
// still records the intent. Multi-user typing interleaves at the
// physical seat.
impl RemoteInputSink for State {
    fn apply(&mut self, peer: MsgPeerId, surface: SurfaceId, event: InputEvent) {
        tracing::debug!(
            target: "shell::compositor::state",
            peer, surface, ?event,
            "remote-input applied"
        );
        // Mirror the event into `last_remote_input` so legacy callers
        // / integration tests that introspect the field still see the
        // routing decision, and per-user "typing" overlays can find
        // the origin peer id in one place.
        self.last_remote_input = Some((peer, surface, event.clone()));

        let time = self.start_time.elapsed().as_millis() as u32;
        let serial = self.next_serial();
        match event {
            InputEvent::PointerMotion { x, y } => {
                if let Some(pointer) = self.seat.get_pointer() {
                    // No surface focus here — the remote driver
                    // doesn't know which local wl_surface corresponds
                    // to its `surface` id. The pointer's logical
                    // position still updates, which is what the
                    // compositor's hover / cluster routing layer
                    // consumes.
                    pointer.motion(
                        self,
                        None,
                        &MotionEvent {
                            location: (x, y).into(),
                            serial,
                            time,
                        },
                    );
                    if let Some(pointer) = self.seat.get_pointer() {
                        pointer.frame(self);
                    }
                }
            }
            InputEvent::PointerButton { button, pressed } => {
                if let Some(pointer) = self.seat.get_pointer() {
                    let state = if pressed {
                        smithay::backend::input::ButtonState::Pressed
                    } else {
                        smithay::backend::input::ButtonState::Released
                    };
                    pointer.button(
                        self,
                        &ButtonEvent {
                            button,
                            state,
                            serial,
                            time,
                        },
                    );
                    if let Some(pointer) = self.seat.get_pointer() {
                        pointer.frame(self);
                    }
                }
            }
            InputEvent::PointerEnter { surface: target } => {
                // Grab keyboard focus for the peer's virtual seat.
                let prev = self.remote_seats.seat_mut(peer).enter(target);
                // Only flip the physical seat's keyboard focus if the
                // target actually changed — noisy re-enters otherwise
                // stomp the client's focus state.
                if prev != Some(target) {
                    if let (Some(resolver), Some(keyboard)) =
                        (&self.surface_resolver, self.seat.get_keyboard())
                    {
                        let target_surface = resolver(target);
                        keyboard.set_focus(self, target_surface, serial);
                    }
                }
            }
            InputEvent::PointerLeave { surface: _ } => {
                // Release the peer's grab. We only clear the physical
                // seat's keyboard focus if this peer was actually the
                // one holding it — a `PointerLeave` from a peer that
                // never entered is a no-op.
                let prev = self.remote_seats.seat_mut(peer).leave();
                if prev.is_some() {
                    if let Some(keyboard) = self.seat.get_keyboard() {
                        keyboard.set_focus(self, None, serial);
                    }
                }
            }
            InputEvent::Key {
                keysym,
                pressed,
                modifiers: _modifiers,
            } => {
                // The peer's wire `keysym` is treated as a raw keycode
                // — see the `InputEvent` doc comment. `KeyboardHandle::input`
                // walks the seat's xkbcommon state and delivers into the
                // focused client.
                //
                // Modifier state is *not* force-set here: smithay's
                // internal `key_input` maintains modifier state from
                // the running keycode stream, so peers that need
                // Shift+A send Shift-press → A-press → A-release →
                // Shift-release. The `modifiers` bitfield on the wire
                // is diagnostics only (recorded in
                // `last_remote_input`); a follow-up can force-set
                // modifiers via `Seat::keyboard_shortcuts_inhibitor`
                // when we want to guarantee a specific mod state
                // without waiting for the modifier keycode stream.
                if let Some(keyboard) = self.seat.get_keyboard() {
                    let key_state = if pressed {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    };
                    // Smithay's Keycode wraps u32 — Keysym on the wire
                    // is the value passed straight through. See the
                    // wayland.rs sibling's `handle_input` for the
                    // production shape we mirror here.
                    let _: Option<()> = keyboard.input(
                        self,
                        keysym.into(),
                        key_state,
                        serial,
                        time,
                        |_, _, _| {
                            smithay::input::keyboard::FilterResult::<()>::Forward
                        },
                    );
                }
            }
        }
    }
}

/// Per-client state stored in the `wayland-server` `Client` user-data slot.
///
/// `CompositorHandler::client_compositor_state` is required to hand back a
/// `&CompositorClientState` for any given client; smithay's convention is to
/// stash one inside a `ClientData` impl so it's lifetime-tied to the client.
#[derive(Default)]
pub struct ClientState {
    /// Smithay's per-client compositor bookkeeping (transaction queue, scale).
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl ClientState {
    /// Convenience constructor returning an `Arc` so callers can pass it to
    /// `DisplayHandle::insert_client` directly.
    pub fn new_arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

// -----------------------------------------------------------------------------
// Handler trait impls — Phase 2 stubs.
// -----------------------------------------------------------------------------
//
// All of these are deliberately minimal. Behaviour beyond "compile and accept
// connections" lands in Phase 4 (input + rendering) and Phase 5 (cluster
// integration). Until then, surfaces commit into thin air.

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client we accept is initialised with a `ClientState` in
        // `native::run`; if that contract is violated we'd rather panic
        // loudly here than silently corrupt protocol state.
        &client
            .get_data::<ClientState>()
            .expect("client missing ClientState user-data")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Phase 4 v2 — pull the surface's most-recent buffer into the
        // [`SurfaceTracker`] iff the surface is registered as shared on
        // any workspace in [`WorkspaceTree`]. Most surfaces aren't
        // shared, so the lookup short-circuits at the
        // `is_surface_shared` check before we ever touch the buffer
        // state. Heavy renderer hooks land in a Phase 4 follow-up.
        let surface_id = surface.id().protocol_id() as SurfaceId;
        let is_shared = self
            .workspace_tree
            .workspaces
            .iter()
            .any(|ws| ws.is_surface_shared(surface_id));
        if !is_shared {
            return;
        }
        // Pull the most recent buffer assignment off the surface's
        // cached state. `BufferAssignment::NewBuffer(b)` is the
        // freshly-attached `wl_buffer`; `Removed` means the client
        // detached and we should drop the tracker slot.
        wl_compositor::with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<wl_compositor::SurfaceAttributes>();
            match &attrs.current().buffer {
                Some(wl_compositor::BufferAssignment::NewBuffer(buffer)) => {
                    // Route through path detection — DMABUF buffers
                    // take the direct path (recorded via the tracker's
                    // capture-path counter; encoder pulls the fd from
                    // the wl_buffer). SHM buffers fall through to the
                    // readback + swizzle path.
                    let (path, result) =
                        self.surface_tracker.on_commit_with_path_detection(surface_id, buffer);
                    match result {
                        Ok(_) => {}
                        Err(crate::compositor::surface_readback::ReadbackError::DmaBufUnsupported) => {
                            // The tentative path is `DmabufDirect`.
                            // Pull the Dmabuf off the wl_buffer via
                            // smithay's dmabuf-role lookup and stash
                            // it on the tracker — `tick_share_session_for_surface`
                            // will drain it via
                            // `take_latest_dmabuf` on the same tick.
                            //
                            // If `get_dmabuf` fails despite the
                            // detection saying "not shm" (a client
                            // that attached an unrecognised buffer
                            // type — e.g. `wl_drm` from an obsolete
                            // MIT-shm-less path), the tracker slot
                            // stays empty for this tick; the frame
                            // is dropped and no counter is bumped
                            // (which is the correct behaviour for
                            // "we don't know how to handle this
                            // buffer at all").
                            match smithay::wayland::dmabuf::get_dmabuf(buffer) {
                                Ok(dmabuf) => {
                                    self.surface_tracker.note_dmabuf_committed(
                                        surface_id,
                                        dmabuf.clone(),
                                    );
                                }
                                Err(_) => {
                                    tracing::warn!(
                                        target: "shell::compositor::state",
                                        surface_id,
                                        path = ?path,
                                        "buffer is neither shm nor dmabuf (unknown role); dropping frame"
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                target: "shell::compositor::state",
                                surface_id, ?err, ?path,
                                "surface readback failed; tracker slot left empty",
                            );
                        }
                    }
                }
                Some(wl_compositor::BufferAssignment::Removed) => {
                    self.surface_tracker.forget(surface_id);
                }
                None => {
                    // Nothing attached this commit — preserve the
                    // previously-staged frame so a re-commit without a
                    // new buffer (e.g. just a damage update) doesn't
                    // wipe the tracker.
                }
            }
        });

        // Colorimetry passthrough: whatever the client committed with
        // `wp_color_management_surface_v1.set_image_description`
        // becomes the wire-tagged colorimetry the share session
        // reports on stream_start. Unset clears the override so the
        // config-driven default wins again.
        if let Some(session) = self.active_sessions.get(&surface_id).cloned() {
            match crate::wayland::color_management::surface_current_colorimetry(surface) {
                Some((color, hdr)) => session.set_color_override(color, hdr),
                None => session.clear_color_override(),
            }
        }

        // Share-session tick hook: if this surface has an active
        // Flatpak share session, deposit the freshly-captured RGBA
        // and pump the encoder in the same tick. Without this hook
        // the encoder waits for the render loop's next damage sweep,
        // introducing a full-frame stall between the client's commit
        // and the frame landing in the substream.
        //
        // The delegation to `tick_share_session_for_surface` keeps
        // the pump logic testable outside of a real `WlSurface`.
        let _ = self.tick_share_session_for_surface(surface_id);
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(
        &mut self,
        _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Phase 4 v3: accept every advertised DMABUF import.
        //
        // Smithay's `DmabufHandler` is the "does your renderer accept
        // this DMABUF?" gate — the compositor is free to refuse a
        // format the wgpu/vello backend can't sample. In practice the
        // advertised format list is the same list we'd accept, and
        // the eventual failure surface for a bad buffer is the render
        // pass itself (which drops the frame with a warn). So we
        // accept unconditionally; `notifier.successful()` registers
        // the Dmabuf as the buffer's user-data so the compositor's
        // commit hook can later pull it back via
        // [`smithay::wayland::dmabuf::get_dmabuf`].
        //
        // We don't stash the Dmabuf here because we don't yet know
        // which surface it will land on — a client can import
        // DMABUFs eagerly and attach them to different surfaces
        // later. The tracker's per-surface cache is populated at
        // commit-time via `SurfaceTracker::note_dmabuf_committed`
        // when the compositor's commit hook sees the buffer attached
        // to a shared surface.
        let _ = notifier.successful::<Self>();
        tracing::debug!(
            target: "shell::compositor::state",
            width = dmabuf.width(),
            height = dmabuf.height(),
            fourcc = ?dmabuf.format().code,
            "dmabuf imported and accepted"
        );
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Phase 6 Task 6.1 — adopt the freshly-committed toplevel into
        // the active workspace's tab tree. The `app_id` may be unset at
        // this point; `adopt_toplevel` falls back to a synthetic
        // `anon-<id>` placeholder, and a future Task 6.2 step (re-adopt
        // on `set_app_id`) rewrites it once the client provides one.
        let surface_id = surface.wl_surface().id().protocol_id() as u64;
        let app_id = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        tab_binding::adopt_toplevel(&mut self.workspace_tree, surface_id, app_id);

        // Send a default configure so the client can map. Phase 4 will pick a
        // real size/position from the cluster layout; Phase 2 just needs the
        // handshake to complete.
        surface.send_configure();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Phase 6 Task 6.2 — remove the matching `Foreign` tab so the
        // chrome stops advertising a destroyed surface.
        let surface_id = surface.wl_surface().id().protocol_id() as u64;
        let _ = tab_binding::close_tab_for_destroyed_surface(
            &mut self.workspace_tree,
            surface_id,
        );
        // C-7 fix: drop the tracker's per-surface buffer slot. The
        // commit path's `BufferAssignment::Removed` branch already
        // forgets the slot when a client `attach(NULL)`s before
        // destroying its toplevel, but a destroy without that prior
        // detach left the `Vec<u8>` (sized at the buffer's last frame)
        // allocated forever. Per-surface memory leak; small per
        // surface, accumulates across long sessions with many
        // short-lived toplevels.
        self.surface_tracker.drop_surface(surface_id as SurfaceId);
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        // Phase 7 Task 7.2 — clients call `xdg_toplevel::set_app_id`
        // *after* `xdg_surface::get_toplevel` fires `new_toplevel`, so
        // the tab was initially adopted with a synthetic
        // `anon-<surface_id>` placeholder. Re-read the now-populated
        // app-id from the surface data and rewrite the tab in-place so
        // the chrome (and the Phase 7 integration test) see the real
        // app-id.
        let surface_id = surface.wl_surface().id().protocol_id() as u64;
        let new_app_id = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        if let Some(app_id) = new_app_id {
            let _ = tab_binding::rename_tab_for_app_id(
                &mut self.workspace_tree,
                surface_id,
                app_id,
            );
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        surface.send_configure().ok();
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // Popup grabs land in phase 4 alongside the seat plumbing.
    }

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
        // No-op until popup positioning is wired up.
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

impl OutputHandler for State {}

// -----------------------------------------------------------------------------
// Delegate macros — wire each protocol's `Dispatch` impls onto `State`.
// -----------------------------------------------------------------------------

delegate_compositor!(State);
delegate_xdg_shell!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_seat!(State);
delegate_virtual_keyboard_manager!(State);
delegate_output!(State);

// `AsRef<ColorManagerState>` lets the color-management `GlobalDispatch`
// impl reach the advertised-capability list without adding a public
// getter on `State`.
impl AsRef<ColorManagerState> for State {
    fn as_ref(&self) -> &ColorManagerState {
        &self.color_manager
    }
}

// `wp_color_manager_v1` + friends live outside smithay's delegate
// wrappers — we hand-roll them in `wayland::color_management`. The
// macro invocation below expands to the `Dispatch` /
// `GlobalDispatch` route-through for every interface in the protocol.
crate::delegate_color_management!(State);

#[cfg(test)]
mod tick_hook_tests {
    use super::*;
    use crate::collab::flatpak_share::SharedFlatpakSession;
    use crate::config::{EncoderChoice, FlatpakShareConfig};
    use crate::network::protocol::{PeerId, SurfaceId};
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;

    fn make_state() -> State {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop");
        let display: Display<State> =
            Display::new().expect("display");
        State::new(event_loop.handle(), display.handle())
    }

    fn cfg() -> FlatpakShareConfig {
        FlatpakShareConfig {
            max_bitrate_kbps: 8000,
            target_fps: 30,
            encoder: EncoderChoice::Openh264,
            hdr: None,
        }
    }

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                v.push(((x * 255) / w.max(1)) as u8);
                v.push(((y * 255) / h.max(1)) as u8);
                v.push(0x80);
                v.push(0xFF);
            }
        }
        v
    }

    /// Two commits with a share session registered must produce two
    /// encoded frames on the session's substream. The commit hook
    /// pumps `encode_next_frame` once per commit; the substream's
    /// per-subscriber queue accumulates them.
    #[test]
    fn two_commits_produce_two_encoded_frames() {
        const W: u32 = 64;
        const H: u32 = 64;
        const HOST: PeerId = 0xAAAA;
        const VIEWER: PeerId = 0xBBBB;
        const SURFACE: SurfaceId = 42;

        let session = Arc::new(
            SharedFlatpakSession::start(HOST, "app", SURFACE, W, H, cfg())
                .expect("session"),
        );
        session.add_viewer(VIEWER, false);

        let mut state = make_state();
        state.register_share_session(session.clone());

        // Simulate the sequence a real commit would produce:
        // `on_commit` deposits bytes into the tracker (mocked here via
        // `deposit_for_test`), then the commit handler calls
        // `tick_share_session_for_surface`. Test drives the tick
        // directly so no real `WlSurface` is needed.
        state
            .surface_tracker
            .deposit_for_test(SURFACE, gradient(W, H), W, H);
        let ran = state.tick_share_session_for_surface(SURFACE);
        assert!(ran, "first commit must fire the tick");
        // Nudge the throttle clock by re-depositing and pumping again
        // — the throttle is time-based, and in tests two back-to-back
        // ticks land inside the 33 ms window. Advance the wall clock
        // by sleeping the interval; the throttle's `should_encode`
        // reads `Instant::now()` and will accept the second tick.
        std::thread::sleep(std::time::Duration::from_millis(40));
        state
            .surface_tracker
            .deposit_for_test(SURFACE, gradient(W, H), W, H);
        let ran2 = state.tick_share_session_for_surface(SURFACE);
        assert!(ran2, "second commit must fire the tick");

        // Both encoded frames land in the subscriber's substream
        // queue with monotonic sequence numbers.
        let f0 = session
            .poll_stream_frame(VIEWER)
            .expect("first frame on the substream");
        let f1 = session
            .poll_stream_frame(VIEWER)
            .expect("second frame on the substream");
        assert_eq!(f0.sequence, 0);
        assert_eq!(f1.sequence, 1);
        assert!(session.poll_stream_frame(VIEWER).is_none());
    }

    /// A commit for a surface with no registered share session is a
    /// no-op (no encoder pumped, no substream traffic).
    #[test]
    fn commit_without_active_session_is_noop() {
        let mut state = make_state();
        state
            .surface_tracker
            .deposit_for_test(999, vec![0; 64], 4, 4);
        assert!(!state.tick_share_session_for_surface(999));
    }

    /// A commit with a registered session but no staged frame in the
    /// tracker returns `false` (nothing to encode). This is the
    /// damage-only commit path — smithay fires `commit` for cursor
    /// motion / null-attach without a new buffer.
    #[test]
    fn commit_without_staged_frame_returns_false() {
        const HOST: PeerId = 0xAAAA;
        const SURFACE: SurfaceId = 55;

        let session = Arc::new(
            SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, cfg())
                .expect("session"),
        );
        let mut state = make_state();
        state.register_share_session(session);
        // Tracker slot is empty.
        assert!(!state.tick_share_session_for_surface(SURFACE));
    }

    /// `unregister_share_session` stops the tick hook from pumping.
    #[test]
    fn unregister_stops_the_tick_hook() {
        const HOST: PeerId = 0xAAAA;
        const SURFACE: SurfaceId = 77;

        let session = Arc::new(
            SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, cfg())
                .expect("session"),
        );
        let mut state = make_state();
        state.register_share_session(session);
        state
            .surface_tracker
            .deposit_for_test(SURFACE, vec![0xFFu8; 32 * 32 * 4], 32, 32);
        assert!(state.tick_share_session_for_surface(SURFACE));
        state.unregister_share_session(SURFACE);
        // Post-unregister the tick is a no-op even with a staged
        // frame.
        state
            .surface_tracker
            .deposit_for_test(SURFACE, vec![0xFFu8; 32 * 32 * 4], 32, 32);
        assert!(!state.tick_share_session_for_surface(SURFACE));
    }
}
