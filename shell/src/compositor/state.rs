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
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    self as wl_compositor, CompositorClientState, CompositorHandler, CompositorState,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell};

use crate::compositor::surface_readback::SurfaceTracker;
use crate::compositor::tab_binding;
use crate::network::protocol::{PeerId as MsgPeerId, SurfaceId};
use crate::network::streaming::{InputEvent, RemoteInputSink, ShareRegistry};
use crate::shell::state::WorkspaceTree;

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

        // D-1 fix: register a real seat with a pointer device so
        // `apply_remote_input` has something to inject into. The seat
        // name matches the wayland.rs sibling so multi-binary apps see
        // a consistent identifier. Keyboard injection ships in the
        // follow-up — registering the device here would force a keymap
        // load (`add_keyboard(Default::default(), 200, 25)`) we
        // intentionally defer.
        let mut seat = seat_state.new_wl_seat(&display_handle, "quick-desktop");
        seat.add_pointer();

        Self {
            loop_handle,
            display_handle,
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            output_manager_state,
            workspace_tree: WorkspaceTree::bootstrap_default(),
            share_registry: ShareRegistry::new(),
            last_remote_input: None,
            surface_tracker: SurfaceTracker::new(),
            seat,
            serial_counter: 0,
            start_time: std::time::Instant::now(),
        }
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
// D-1 fix: this previously only logged + recorded the event in
// `last_remote_input`, leaving the boolean return of
// `apply_remote_input` as the only signal that anything had happened.
// The seat path was a no-op. Post-fix, pointer motion / button events
// drive the real smithay `Seat<Self>` (constructed in `State::new`)
// via `PointerHandle::motion` / `button`, matching the production
// shape used by `network::wayland::WaylandServer::handle_input`.
//
// Keyboard injection is intentionally deferred to a follow-up — the
// keymap load + key-code translation is non-trivial and the
// foreign-tab remote-control use case the Phase 4 spec calls out is
// pointer-driven. The `Key` variant still records into
// `last_remote_input` so a future wire-up can flip it on without
// shifting the public API.
impl RemoteInputSink for State {
    fn apply(&mut self, peer: MsgPeerId, surface: SurfaceId, event: InputEvent) {
        tracing::debug!(
            target: "shell::compositor::state",
            peer, surface, ?event,
            "remote-input applied (D-1: pointer events injected into seat; \
             keyboard injection deferred)"
        );
        // Mirror the event into `last_remote_input` so legacy callers
        // / integration tests that introspect the field still see the
        // routing decision. Pre-fix this was the *only* side effect;
        // post-fix it's a redundant trace.
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
            InputEvent::Key { .. } => {
                // Keyboard injection deferred — see module comment.
                // We've already stamped `last_remote_input`, which is
                // the read-only-gate-passed signal callers rely on.
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
                    match self.surface_tracker.on_commit(surface_id, buffer) {
                        Ok(_) => {}
                        Err(err) => {
                            tracing::warn!(
                                target: "shell::compositor::state",
                                surface_id, ?err,
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
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(
        &mut self,
        _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
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
delegate_seat!(State);
delegate_output!(State);
