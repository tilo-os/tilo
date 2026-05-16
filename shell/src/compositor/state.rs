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
        let seat_state = SeatState::new();
        let output_manager_state =
            OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

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
        }
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
// `ShareRegistry::route_to`. v1 logs and stores the most recently
// applied event so tests can introspect; v2 wires this into the smithay
// `Seat::input` API. The reason the seat call is staged behind a v2
// landing is that the compositor's `Seat<State>` field is added by
// Task 4.4's follow-up alongside the input-device source registration;
// the routing / read-only gating ships now so the test surface is
// stable.
impl RemoteInputSink for State {
    fn apply(&mut self, peer: MsgPeerId, surface: SurfaceId, event: InputEvent) {
        tracing::debug!(
            target: "quick_desktop::compositor::state",
            peer, surface, ?event,
            "remote-input applied (v1 — seat injection lands in 4.4 follow-up)"
        );
        self.last_remote_input = Some((peer, surface, event));
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

    fn commit(&mut self, _surface: &WlSurface) {
        // Phase 4 hooks the renderer here.
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
