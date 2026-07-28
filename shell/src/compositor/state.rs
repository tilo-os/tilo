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

use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
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
use smithay::wayland::fractional_scale::{
    self, FractionalScaleHandler, FractionalScaleManagerState,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::{
    delegate_compositor, delegate_dmabuf, delegate_fractional_scale, delegate_output,
    delegate_pointer_gestures, delegate_seat, delegate_session_lock, delegate_shm,
    delegate_viewporter, delegate_virtual_keyboard_manager, delegate_xdg_shell,
};

use crate::collab::flatpak_share::SharedFlatpakSession;
use crate::compositor::remote_seat::RemoteSeats;
use crate::compositor::surface_readback::SurfaceTracker;
use crate::compositor::tab_binding;
use crate::network::protocol::{PeerId as MsgPeerId, SurfaceId};
use crate::wayland::color_management::ColorManagerState;
use crate::network::streaming::{InputEvent, RemoteAction, RemoteInputSink, ShareRegistry};
use crate::shell::state::WorkspaceTree;
use smithay::backend::input::KeyState;
use std::collections::HashMap;

/// Read the persisted HiDPI display scale from the shared settings store.
///
/// The Settings app writes `~/.config/qos/settings.json` with a
/// `"display"` object carrying a `"scale"` number (1.0 = 100 %, 1.5 =
/// 150 %, …). The compositor reads the same file — the contract is the
/// JSON on disk, not a shared crate, so the compositor never depends on
/// the Settings app. Any read/parse failure, a missing file, or a value
/// outside a sane range falls back to `1.0` so a corrupt config can never
/// leave the session at an unusable zoom.
fn load_display_scale() -> f64 {
    let Some(path) = dirs::config_dir().map(|d| d.join("qos").join("settings.json")) else {
        return 1.0;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 1.0;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 1.0;
    };
    match json.get("display").and_then(|d| d.get("scale")).and_then(|s| s.as_f64()) {
        // Clamp to the range the Settings control offers (100 %–200 %),
        // with a small margin, so an out-of-band value can't wedge the UI.
        Some(scale) if (0.5..=3.0).contains(&scale) => scale,
        _ => 1.0,
    }
}

/// Bridges the `ext_session_lock_v1` protocol handler (which runs inside the
/// smithay dispatch on [`State`]) to the native run loop that owns the
/// authoritative lock state + the compositor-drawn lock overlay.
///
/// The protocol and the compositor share **one** locked state: a client
/// `lock` request stashes the [`SessionLocker`] here and raises
/// `lock_requested`; the run loop locks the session, paints the overlay, then
/// confirms via [`SessionLocker::lock`] so the client's `locked` event only
/// fires once app content is actually hidden. A client `unlock_and_destroy`
/// raises `unlock_requested`, which the run loop consumes to clear the lock
/// (the client authenticated on its side; the compositor's own PAM path
/// authenticates chord/idle-driven locks). Same-thread, so no `Arc`/lock —
/// the run loop reads these fields directly each tick.
#[derive(Default)]
pub struct SessionLockHooks {
    lock_requested: bool,
    unlock_requested: bool,
    pending_locker: Option<SessionLocker>,
}

impl SessionLockHooks {
    /// Take the "a client asked to lock" edge (clears it; true at most once
    /// per protocol `lock` request).
    pub fn take_lock_requested(&mut self) -> bool {
        std::mem::take(&mut self.lock_requested)
    }

    /// Take the "a client asked to unlock" edge (clears it).
    pub fn take_unlock_requested(&mut self) -> bool {
        std::mem::take(&mut self.unlock_requested)
    }

    /// Take the stashed [`SessionLocker`] so the run loop can confirm the lock
    /// with `.lock()` after the overlay has been presented. `None` when
    /// nothing is pending confirmation.
    pub fn take_pending_locker(&mut self) -> Option<SessionLocker> {
        self.pending_locker.take()
    }

    /// Whether a client currently holds (or is mid-acquiring) the lock —
    /// there is a locker pending confirmation. Used only by tests.
    #[cfg(test)]
    pub fn has_pending_locker(&self) -> bool {
        self.pending_locker.is_some()
    }
}

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
    /// `wp_fractional_scale_manager_v1` state — lets HiDPI clients ask the
    /// compositor for the preferred fractional scale of their surface (as
    /// a `1/120`-unit integer on the wire). The preferred value the
    /// compositor sends is [`Self::scale`]; it is pushed to every surface
    /// on commit so a client that renders at, say, 1.5× produces crisp
    /// output instead of a bilinearly-upscaled 1× buffer.
    pub fractional_scale_manager: FractionalScaleManagerState,
    /// `wp_viewporter` state — the companion protocol to fractional
    /// scaling. A client that renders a 1.5×-sized buffer uses
    /// `wp_viewport.set_destination` to declare the logical size the
    /// buffer maps to, so the compositor scales it correctly (no need for
    /// integer `wl_surface.set_buffer_scale`, which cannot express 1.5).
    pub viewporter: ViewporterState,
    /// Current display scale factor (HiDPI). Sent to clients as the
    /// preferred `wp_fractional_scale`. Read once at session start from the
    /// shared settings store (`~/.config/qos/settings.json`, section
    /// `display.scale`) so the value the Settings app's "Display scale"
    /// control writes drives what the compositor advertises. Defaults to
    /// `1.0` when unset or the file is absent.
    pub scale: f64,
    /// **Transient fullscreen surface** — the window group id of the toplevel
    /// that currently holds the output fullscreen via
    /// `xdg_toplevel.set_fullscreen`, or `None` in the normal tiled state.
    /// Set by [`XdgShellHandler::fullscreen_request`] and cleared by
    /// `unfullscreen_request` (or by surface destroy). The native render loop
    /// mirrors this into the `FrameBuilder` each frame so the shell scene
    /// promotes the group to the whole output; it is deliberately NOT part of
    /// the persisted `WorkspaceTree` (a live presentation must never survive a
    /// reboot) and restores the exact prior layout when cleared.
    pub fullscreen_group: Option<u64>,
    /// Combined output size in physical pixels, mirrored from the backend by
    /// the native loop (`(0, 0)` until the first output is known). Used as the
    /// size sent in a fullscreen `xdg_toplevel` configure so a fullscreened
    /// client fills the whole output; `(0, 0)` leaves the size unspecified so
    /// the client keeps its own.
    pub output_size: (i32, i32),
    /// `ext_session_lock_v1` global state (tracks which outputs a client lock
    /// has covered). Advertised so `loginctl lock-session` / a conforming lock
    /// client can drive the session lock through the standard protocol.
    pub session_lock_state: SessionLockManagerState,
    /// Bridge from the protocol handler to the native run loop's authoritative
    /// lock (see [`SessionLockHooks`]).
    pub session_lock: SessionLockHooks,
    /// `zwp_pointer_gestures_v1` global state — advertises touchpad pinch /
    /// swipe / hold gestures to clients. On a bare-TTY (DRM) session Mutter
    /// isn't around to expose gestures, so the compositor advertises this
    /// itself; winit clients bind it and create a `zwp_pointer_gesture_pinch_v1`
    /// per pointer. Delivering the gesture events to the focused surface is
    /// gated on the compositor routing local libinput pointer input through the
    /// seat (today local input drives the shell chrome, not client surfaces).
    pub pointer_gestures_state: PointerGesturesState,
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
        // Advertise `wp_fractional_scale_manager_v1` + `wp_viewporter`.
        // Together they let HiDPI clients render at a fractional scale
        // (e.g. 1.5×) and have the compositor composite the result at the
        // correct logical size. The preferred scale itself is pushed
        // per-surface on commit (see `CompositorHandler::commit`).
        let fractional_scale_manager = FractionalScaleManagerState::new::<Self>(&display_handle);
        let viewporter = ViewporterState::new::<Self>(&display_handle);
        // Advertise `ext_session_lock_manager_v1` so a conforming lock client
        // (or `loginctl lock-session` routed through one) can lock the session
        // through the standard protocol. The filter allows every client; the
        // compositor still owns the on-screen lock UI + authentication.
        let session_lock_state =
            SessionLockManagerState::new::<Self, _>(&display_handle, |_client| true);
        // Read the persisted display scale from the shared settings store
        // so the compositor advertises the value the Settings app wrote.
        let scale = load_display_scale();
        // Advertise touchpad pinch/swipe/hold gestures so clients (winit apps)
        // can bind them on a bare-TTY session where no host compositor exposes
        // the protocol. Constructed before the struct literal because
        // `display_handle` is moved into the struct.
        let pointer_gestures_state = PointerGesturesState::new::<Self>(&display_handle);

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
            fractional_scale_manager,
            viewporter,
            scale,
            fullscreen_group: None,
            output_size: (0, 0),
            session_lock_state,
            session_lock: SessionLockHooks::default(),
            pointer_gestures_state,
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

    /// Inject a single `org.freedesktop.impl.portal.RemoteDesktop`
    /// [`RemoteAction`] into the real seat, targeting whatever surface
    /// currently holds seat focus.
    ///
    /// This is the *local-app* counterpart to [`Self::apply_remote_input`]
    /// (which serves *peer-sync* and is gated by the share registry).
    /// RemoteDesktop consent is mediated by the portal grant + the started
    /// session's device selection — enforced in the portal backend — so by
    /// the time an action reaches here it has already cleared consent, and
    /// this path drives the seat directly. In production the compositor's
    /// event loop drains a
    /// [`RemoteInputInjector`](crate::portal::RemoteInputInjector) channel
    /// and calls this per action.
    ///
    /// Returns `true` when the action reached a live seat device, `false`
    /// when the relevant device is absent (headless without a seat) or a
    /// keysym couldn't be resolved to a keycode in the active layout.
    pub fn apply_remote_desktop_action(&mut self, action: RemoteAction) -> bool {
        let time = self.start_time.elapsed().as_millis() as u32;
        let serial = self.next_serial();
        match action {
            RemoteAction::PointerMotionAbsolute { x, y, .. } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return false;
                };
                pointer.motion(
                    self,
                    None,
                    &MotionEvent {
                        location: (x, y).into(),
                        serial,
                        time,
                    },
                );
                pointer.frame(self);
                true
            }
            RemoteAction::PointerMotionRelative { dx, dy } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return false;
                };
                let loc = pointer.current_location();
                pointer.motion(
                    self,
                    None,
                    &MotionEvent {
                        location: (loc.x + dx, loc.y + dy).into(),
                        serial,
                        time,
                    },
                );
                // Also surface the raw delta for clients that consume
                // `wp_relative_pointer` (games, pointer-locked apps).
                pointer.relative_motion(
                    self,
                    None,
                    &RelativeMotionEvent {
                        delta: (dx, dy).into(),
                        delta_unaccel: (dx, dy).into(),
                        utime: (time as u64) * 1000,
                    },
                );
                pointer.frame(self);
                true
            }
            RemoteAction::PointerButton { button, pressed } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return false;
                };
                let state = if pressed {
                    smithay::backend::input::ButtonState::Pressed
                } else {
                    smithay::backend::input::ButtonState::Released
                };
                pointer.button(
                    self,
                    &ButtonEvent {
                        button: button as u32,
                        state,
                        serial,
                        time,
                    },
                );
                pointer.frame(self);
                true
            }
            RemoteAction::PointerAxis { dx, dy } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return false;
                };
                let mut frame = AxisFrame::new(time)
                    .source(smithay::backend::input::AxisSource::Continuous);
                if dx != 0.0 {
                    frame = frame.value(smithay::backend::input::Axis::Horizontal, dx);
                }
                if dy != 0.0 {
                    frame = frame.value(smithay::backend::input::Axis::Vertical, dy);
                }
                pointer.axis(self, frame);
                pointer.frame(self);
                true
            }
            RemoteAction::PointerAxisDiscrete { axis, steps } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return false;
                };
                let ax = if axis == 1 {
                    smithay::backend::input::Axis::Horizontal
                } else {
                    smithay::backend::input::Axis::Vertical
                };
                // One wheel detent == 120 in the high-resolution `v120`
                // convention; the legacy `value` carries a 15px-per-step
                // fallback for clients that don't understand v120.
                let frame = AxisFrame::new(time)
                    .source(smithay::backend::input::AxisSource::Wheel)
                    .v120(ax, steps * 120)
                    .value(ax, (steps as f64) * 15.0);
                pointer.axis(self, frame);
                pointer.frame(self);
                true
            }
            RemoteAction::KeyboardKeycode { keycode, pressed } => {
                self.inject_seat_key(keycode as u32, pressed, serial, time)
            }
            RemoteAction::KeyboardKeysym { keysym, pressed } => {
                match self.keysym_to_keycode(keysym as u32) {
                    Some(kc) => self.inject_seat_key(kc, pressed, serial, time),
                    None => {
                        tracing::debug!(
                            keysym,
                            "RemoteDesktop: keysym has no keycode in the active layout; dropped"
                        );
                        false
                    }
                }
            }
        }
    }

    /// Deliver a raw keycode press/release to the seat keyboard. Shared by
    /// the RemoteDesktop keycode + keysym paths. The keycode is passed
    /// straight through to smithay's `KeyboardHandle::input` — the same
    /// convention `network::wayland::WaylandServer::handle_input` and
    /// [`Self::apply_remote_input`]'s `Key` arm use.
    fn inject_seat_key(
        &mut self,
        keycode: u32,
        pressed: bool,
        serial: Serial,
        time: u32,
    ) -> bool {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let key_state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let _: Option<()> = keyboard.input(
            self,
            keycode.into(),
            key_state,
            serial,
            time,
            |_, _, _| smithay::input::keyboard::FilterResult::<()>::Forward,
        );
        true
    }

    /// Reverse-lookup a keysym to a keycode in the seat keyboard's active
    /// layout, so `NotifyKeyboardKeysym` can be delivered even though
    /// smithay's injection API is keycode-oriented. Scans the evdev/xkb
    /// keycode range for a key whose level-0 symbol matches. Returns `None`
    /// when the active layout can't produce the keysym (the portal caller
    /// should fall back to `NotifyKeyboardKeycode`).
    fn keysym_to_keycode(&mut self, target: u32) -> Option<u32> {
        use smithay::input::keyboard::{Keycode, Keysym};
        let keyboard = self.seat.get_keyboard()?;
        let target = Keysym::new(target);
        keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().ok()?;
            let layout = xkb.active_layout();
            // xkb keycodes start at 8 (evdev 0 == xkb 8); 8..=255 covers a
            // standard keyboard's key range.
            for raw in 8u32..=255 {
                let kc = Keycode::new(raw);
                if xkb
                    .raw_syms_for_key_in_layout(kc, layout)
                    .iter()
                    .any(|s| *s == target)
                {
                    return Some(raw);
                }
            }
            None
        })
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
        // HiDPI: push the compositor's current display scale to the
        // surface as its preferred `wp_fractional_scale`. This runs for
        // every surface (not just shared ones) so any HiDPI-aware client
        // that created a `wp_fractional_scale_v1` object learns the scale
        // it should render at. `set_preferred_scale` is idempotent — it
        // only emits a wire event when the value actually changes, and it
        // stores the value even before the client has created the object
        // (smithay replays it on `get_fractional_scale`).
        let scale = self.scale;
        wl_compositor::with_states(surface, |states| {
            fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });

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
        // If the destroyed surface held the output fullscreen, drop the
        // transient promotion so the layout restores rather than staying
        // collapsed around a group that no longer exists.
        if self.fullscreen_group
            == tab_binding::group_id_for_surface(&self.workspace_tree, surface_id)
        {
            self.fullscreen_group = None;
        }
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

    /// A client asked to occupy its output fullscreen
    /// (`xdg_toplevel.set_fullscreen`). We honor it: record which window group
    /// the surface belongs to (so the native render loop promotes it to the
    /// whole output — top bar / sidebar / other strips suppressed — and
    /// restores exactly on exit), and send the client a configure carrying the
    /// `Fullscreen` state plus the full output size so its own surface fills
    /// the screen. The `_wl_output` hint is ignored: this compositor spans one
    /// combined scene across all outputs, so fullscreen always targets the
    /// single presentation surface (see the multi-output note in
    /// `plan/compositor.md`).
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _wl_output: Option<WlOutput>,
    ) {
        let surface_id = surface.wl_surface().id().protocol_id() as u64;
        self.fullscreen_group =
            tab_binding::group_id_for_surface(&self.workspace_tree, surface_id);
        let (w, h) = self.output_size;
        surface.with_pending_state(|state| {
            state.states.set(
                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
            );
            // Size the surface to the whole output when we know it; leave it
            // untouched otherwise so the client keeps its own extent.
            if w > 0 && h > 0 {
                state.size = Some((w, h).into());
            }
        });
        surface.send_configure();
    }

    /// A client dropped out of fullscreen (`xdg_toplevel.unset_fullscreen`),
    /// or Esc / a compositor exit flowed back through the UI. Clear the
    /// transient promotion — the native loop restores the exact prior tiling —
    /// and send a configure clearing the `Fullscreen` state and the imposed
    /// size so the surface returns to its windowed extent.
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let surface_id = surface.wl_surface().id().protocol_id() as u64;
        // Only clear if THIS surface was the fullscreen one (a stale request
        // from a background toplevel must not steal another's fullscreen).
        if self.fullscreen_group
            == tab_binding::group_id_for_surface(&self.workspace_tree, surface_id)
        {
            self.fullscreen_group = None;
        }
        surface.with_pending_state(|state| {
            state.states.unset(
                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
            );
            state.size = None;
        });
        surface.send_configure();
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

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // A client just created a `wp_fractional_scale_v1` for this
        // surface. Seed it with the current display scale immediately so
        // the client doesn't have to wait for its first commit to learn
        // the preferred scale. `set_preferred_scale` is idempotent and
        // safe to call before any buffer is attached.
        let scale = self.scale;
        wl_compositor::with_states(&surface, |states| {
            fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });
    }
}

// -----------------------------------------------------------------------------
// Delegate macros — wire each protocol's `Dispatch` impls onto `State`.
// -----------------------------------------------------------------------------

delegate_compositor!(State);
delegate_xdg_shell!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_seat!(State);
delegate_virtual_keyboard_manager!(State);
delegate_pointer_gestures!(State);
delegate_output!(State);
delegate_fractional_scale!(State);
delegate_viewporter!(State);

// `ext_session_lock_v1`: a client can lock the session; while locked the
// compositor hides all normal client surfaces + routes input only to the lock
// surface (enforced by the native run loop, which owns the authoritative lock
// + the compositor-drawn overlay). qOS renders its own lock UI rather than a
// client lock surface, so the handler bridges a protocol `lock`/`unlock` into
// that one lock state via [`SessionLockHooks`] instead of compositing a client
// surface.
impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Raise the request for the run loop to enter the locked state and
        // paint the overlay; stash the confirmation so the loop calls
        // `.lock()` only after app content is hidden (the client's `locked`
        // event must not fire before the screen is actually covered).
        self.session_lock.lock_requested = true;
        self.session_lock.pending_locker = Some(confirmation);
    }

    fn unlock(&mut self) {
        // The client tore down its lock (having authenticated on its side).
        self.session_lock.unlock_requested = true;
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        // qOS renders its own lock overlay as compositor chrome, so a client
        // lock surface is accepted (so a conforming client does not error or
        // hang) but is not composited — the compositor overlay is the lock UI.
        // Configure it to the current output size so the client can commit a
        // correctly-sized buffer per the protocol.
        let (w, h) = self.output_size;
        if w > 0 && h > 0 {
            surface.with_pending_state(|state| {
                state.size = Some((w as u32, h as u32).into());
            });
            surface.send_configure();
        }
    }
}
delegate_session_lock!(State);

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

    /// The `ext_session_lock_v1` bridge reports each lock/unlock request as a
    /// one-shot edge the run loop consumes: reading it returns `true` once,
    /// then `false`, so the loop can't re-fire a stale request.
    #[test]
    fn session_lock_hooks_report_and_clear_edges() {
        let mut h = SessionLockHooks::default();
        assert!(!h.take_lock_requested());
        assert!(!h.take_unlock_requested());
        assert!(!h.has_pending_locker());

        h.lock_requested = true;
        h.unlock_requested = true;
        assert!(h.take_lock_requested());
        assert!(!h.take_lock_requested(), "lock edge is consumed exactly once");
        assert!(h.take_unlock_requested());
        assert!(!h.take_unlock_requested(), "unlock edge is consumed once");
    }

    /// A freshly constructed state advertises the session-lock global (its
    /// constructor ran `SessionLockManagerState::new` without panicking) and
    /// starts with no client holding a lock.
    #[test]
    fn fresh_state_advertises_session_lock_and_starts_unlocked() {
        let mut state = make_state();
        // Constructing the state ran `SessionLockManagerState::new` (global
        // advertised) without panic; no client holds a lock yet.
        assert!(!state.session_lock.take_lock_requested());
        assert!(!state.session_lock.take_unlock_requested());
        assert!(!state.session_lock.has_pending_locker());
    }
}
