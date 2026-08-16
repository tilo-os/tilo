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

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Format, Modifier};
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
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    self as wl_compositor, CompositorClientState, CompositorHandler, CompositorState,
};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::fractional_scale::{
    self, FractionalScaleHandler, FractionalScaleManagerState,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::{
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_dmabuf,
    delegate_ext_data_control, delegate_fractional_scale, delegate_output,
    delegate_pointer_gestures, delegate_primary_selection, delegate_seat, delegate_session_lock,
    delegate_shm, delegate_viewporter, delegate_virtual_keyboard_manager, delegate_xdg_shell,
};

use crate::collab::flatpak_share::SharedFlatpakSession;
use crate::compositor::remote_seat::RemoteSeats;
use crate::compositor::render_node::{ProbeError, RenderNode};
use crate::compositor::selection::SelectionState;
use crate::compositor::surface_readback::SurfaceTracker;
use crate::compositor::tab_binding;
use crate::network::protocol::{PeerId as MsgPeerId, SurfaceId};
use crate::network::input_firewall::{Denial, RemoteInputFirewall};
use crate::network::streaming::{InputEvent, RemoteAction, RemoteInputSink, ShareRegistry};
use crate::wayland::color_management::ColorManagerState;

use crate::shell::state::WorkspaceTree;
use smithay::backend::input::KeyState;
use std::collections::HashMap;

/// Read the persisted HiDPI display scale from the shared settings store.
///
/// The Settings app writes `~/.config/qos/settings.json` with a
/// `"display"` object carrying a `"scale"` number (1.0 = 100 %, 1.5 =
/// 150 %, …). The compositor reads the same file — and resolves its path
/// through `settings_store`, so it cannot drift from the apps the way it
/// had: this was the one reader that applied the XDG rule for a relative
/// `XDG_CONFIG_HOME` while the apps did not, which put them on two
/// different files. `settings_store` carries no dependency on the Settings
/// app; the contract is still the JSON on disk. Any read/parse failure, a
/// missing file, or a value outside a sane range falls back to `1.0` so a
/// corrupt config can never leave the session at an unusable zoom.
fn load_display_scale() -> f64 {
    let Some(path) = settings_store::settings_path() else {
        return 1.0;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 1.0;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 1.0;
    };
    match json
        .get("display")
        .and_then(|d| d.get("scale"))
        .and_then(|s| s.as_f64())
    {
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
    /// `wl_data_device_manager` global state — the clipboard and
    /// drag-and-drop channel.
    ///
    /// Not optional in practice, whatever a compositor's own apps need.
    /// GTK refuses to use a Wayland display at all when this global is
    /// absent — `gdk_wayland_display_open` checks for `wl_compositor`,
    /// `wl_shm` and `wl_data_device_manager` together and bails with
    /// "the Wayland compositor does not provide one or more of the
    /// required interfaces" if any is missing. So without it every GTK
    /// application on the machine — which is most Flatpaks — connects,
    /// is refused, and falls back to an X11 display that does not exist
    /// either. It never reaches the point of mapping a window, and the
    /// failure looks like the app being broken rather than the
    /// compositor being incomplete.
    ///
    /// qOS's own toolkit does not use it, which is why it was missing:
    /// nothing that shipped in-tree ever asked for it.
    pub data_device_state: DataDeviceState,
    /// `zwp_linux_dmabuf_v1` global state — enables clients to attach
    /// GPU-allocated (usually GBM) buffers to their surfaces.
    ///
    /// The global is created **with default feedback** (protocol
    /// version 4), which is what carries the `main_device` event. That
    /// event is the only remaining way a Mesa EGL/Vulkan client can
    /// learn which GPU to allocate on now that `wl_drm` is gone; a
    /// version-3 global leaves the client with no device, which it
    /// reports as `failed to get driver name for fd -1` before failing
    /// to create its window at all.
    ///
    /// The advertised formats are RGBA-family FourCCs under
    /// `Modifier::Linear` only, because
    /// [`crate::compositor::foreign::import_dmabuf_rgba`] imports a
    /// client buffer by CPU-mapping its plane. The compositor
    /// deliberately advertises exactly what it can composite — offering
    /// tiled modifiers or multi-planar YUV would make conforming
    /// clients hand us buffers we would then have to reject on screen.
    pub dmabuf_state: DmabufState,
    /// The advertised `zwp_linux_dmabuf_v1` global handle. Held for
    /// the process lifetime — dropping it would revoke the global
    /// from clients that already bound it.
    ///
    /// `None` when [`crate::compositor::render_node::probe`] found no
    /// GPU whose dma-bufs the compositor can actually read (no DRM
    /// render node at all, or only drivers that can't CPU-map their
    /// exports). Not advertising is the honest answer there: Mesa then
    /// takes its `wl_shm` software path, which the compositor composites
    /// correctly, instead of negotiating dmabufs every frame would have
    /// to reject.
    pub dmabuf_global: Option<DmabufGlobal>,
    /// The render node handed to clients as
    /// `zwp_linux_dmabuf_feedback_v1.main_device`, or the reason none
    /// was. Kept so the boot log, the test-control state dump and the
    /// integration tests can all read the *decision* rather than
    /// re-deriving it.
    pub dmabuf_device: Result<RenderNode, ProbeError>,
    /// `wl_seat` collection. Phase 2 doesn't add a seat yet; phase 4 will.
    pub seat_state: SeatState<Self>,
    /// Clipboard, primary selection and drag-and-drop.
    ///
    /// Owns the four selection globals plus the compositor's clipboard
    /// mirror, which is what lets in-process chrome paste without a Wayland
    /// round trip. See [`crate::compositor::selection`] — in particular for
    /// why the two data-control globals are gated to privileged clients.
    pub selection: SelectionState,
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
    /// The rest of the remote-input firewall: region, z-order, keyboard
    /// scrubber, rate limit, physical-input priority and the kill switch.
    ///
    /// The share registry decides *whether* a peer may drive a surface;
    /// this decides what, where and how fast. Fed a view of the screen
    /// once per tick by the compositor loop — until then it has observed
    /// nothing and refuses everything.
    pub remote_input_firewall: RemoteInputFirewall,
    /// Last `RemoteInput` event that survived the share-registry gate.
    /// Read by integration tests that want to assert seat-equivalent
    /// injection happened, without spinning up a real `Seat<State>`.
    /// v2 replaces this with the proper smithay seat-call path.
    pub last_remote_input: Option<(MsgPeerId, SurfaceId, InputEvent)>,
    /// Last `RemoteInput` event the firewall refused, and the rule that
    /// refused it. The counterpart to `last_remote_input`: a dropped
    /// event is otherwise invisible, and "nothing happened" is exactly
    /// what a broken filter also looks like.
    pub last_remote_input_denial: Option<(MsgPeerId, SurfaceId, Denial)>,
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
    /// Every output as `(position, size, scale)` in the compositor's
    /// combined coordinate space, mirrored from the backend.
    ///
    /// [`Self::scale`] is one number for the whole session, which is only
    /// right on a machine with one display: a laptop docked to an external
    /// monitor has a dense panel and a sparse one, and a client told the
    /// wrong factor renders soft on one of them. This is what lets the
    /// preferred `wp_fractional_scale` be per-surface — the scale of the
    /// output the surface is actually on.
    pub output_layout: Vec<((i32, i32), (u32, u32), f64)>,
    /// Preferred scale per client surface id, derived from
    /// [`Self::output_layout`] and where the surface's tile sits. Refreshed
    /// by the run loop, which is the only place that has both the output
    /// table and the laid-out tile rects.
    pub surface_scales: HashMap<u64, f64>,
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
    /// Local-input → client-surface routing state: the live foreign-surface
    /// registry, the seat's keyboard / pointer focus bookkeeping, and the
    /// queue of foreign toplevels waiting to be mirrored into the shell
    /// chrome's tab tree. Every method on this field lives in
    /// [`crate::compositor::client_input`].
    pub client_input: crate::compositor::client_input::ClientInput,
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
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let mut seat_state = SeatState::new();
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        // `zwp_linux_dmabuf_v1`. Two things have to be right for a GPU
        // client to boot against us, and only one of them is the format
        // list:
        //
        // 1. **A main device.** Mesa dropped `wl_drm`, so
        //    `dmabuf_feedback.main_device` (protocol version 4) is the
        //    only channel that tells a client which GPU to allocate on.
        //    A version-3 global — what `DmabufState::create_global`
        //    builds — gives the client the protocol without the device,
        //    and EGL dies with `failed to get driver name for fd -1`
        //    before the client ever maps a toplevel.
        // 2. **Only formats we can composite.** A client buffer becomes
        //    tile pixels through `foreign::import_dmabuf_rgba`, which
        //    CPU-maps the plane fd. That is correct for single-plane
        //    RGBA under `Modifier::Linear` and nothing else, so that is
        //    exactly what goes on the wire. (The NV12/P010 the encoder
        //    path consumes are *capture* formats produced downstream of
        //    a share session — advertising them to clients would invite
        //    toplevels we'd have to reject on screen every frame.)
        let dmabuf_formats: Vec<Format> = [
            Fourcc::Argb8888,
            Fourcc::Xrgb8888,
            Fourcc::Abgr8888,
            Fourcc::Xbgr8888,
        ]
        .into_iter()
        .map(|code| Format {
            code,
            modifier: Modifier::Linear,
        })
        .collect();
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_device = crate::compositor::render_node::probe();
        let dmabuf_global = match &dmabuf_device {
            Ok(node) => {
                match DmabufFeedbackBuilder::new(node.dev, dmabuf_formats.clone()).build() {
                    Ok(feedback) => {
                        // Publish the decision for the import side: the
                        // buffers clients hand us were allocated on this
                        // GPU, so that is the GPU that must read them back.
                        crate::compositor::render_node::set_advertised(node.path.clone());
                        tracing::info!(
                            target: "shell::compositor::dmabuf",
                            device = %node.path.display(),
                            driver = node.driver.as_deref().unwrap_or("<unknown>"),
                            dev = node.dev,
                            formats = dmabuf_formats.len(),
                            "advertising zwp_linux_dmabuf_v1 with feedback"
                        );
                        Some(dmabuf_state.create_global_with_default_feedback::<Self>(
                            &display_handle,
                            &feedback,
                        ))
                    }
                    Err(e) => {
                        // The format table lives in a sealed memfd; if we
                        // can't create it we cannot send correct feedback,
                        // and a feedback-less global is the broken state
                        // this whole block exists to avoid.
                        tracing::error!(
                            target: "shell::compositor::dmabuf",
                            error = %e,
                            "could not build dmabuf feedback; not advertising dmabuf — \
                             GPU clients will fall back to wl_shm"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "shell::compositor::dmabuf",
                    reason = %e,
                    "not advertising zwp_linux_dmabuf_v1 — GPU clients will render \
                     through wl_shm (software) instead"
                );
                None
            }
        };

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
        // Keyboard auto-repeat comes from the persisted `input.key_repeat`
        // preference rather than a hard-coded pair, so the Settings
        // control the compositor never read now reaches the seat.
        let (repeat_delay, repeat_rate) =
            crate::compositor::backend::drm::input_config::InputSettings::load()
                .key_repeat_params();
        if let Err(e) = seat.add_keyboard(
            Default::default(),
            repeat_delay.as_millis() as i32,
            repeat_rate as i32,
        ) {
            tracing::warn!("seat: failed to add keyboard (text injection disabled): {e}");
        }
        // A touchscreen has no way to reach a client without this: the
        // compositor's own chrome is driven by the synthesised pointer
        // stream, but a client that wants real multi-touch needs the seat
        // to advertise `wl_touch`.
        seat.add_touch();
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
        // Clipboard + primary selection + drag-and-drop. Advertised before the
        // struct literal because `display_handle` is moved into it. Selection
        // *focus* is pushed alongside keyboard focus every tick — see
        // `client_input::sync_keyboard_focus` — because smithay gates
        // `set_selection` on keyboard focus.
        let selection = SelectionState::new(&display_handle);

        Self {
            loop_handle,
            display_handle,
            compositor_state,
            xdg_shell_state,
            shm_state,
            data_device_state,
            dmabuf_state,
            dmabuf_global,
            dmabuf_device,
            seat_state,
            selection,
            output_manager_state,
            workspace_tree: WorkspaceTree::bootstrap_default(),
            share_registry: ShareRegistry::new(),
            remote_input_firewall: RemoteInputFirewall::new(),
            last_remote_input: None,
            last_remote_input_denial: None,
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
            output_layout: Vec::new(),
            surface_scales: HashMap::new(),
            fullscreen_group: None,
            output_size: (0, 0),
            session_lock_state,
            session_lock: SessionLockHooks::default(),
            pointer_gestures_state,
            client_input: Default::default(),
        }
    }

    /// Mirror the backend's output table.
    ///
    /// Called at startup and on every hotplug. Dropping the per-surface
    /// scale cache alongside it is deliberate: the layout is what the
    /// cached values were derived from, so keeping them would hand
    /// clients the scale of a monitor that has moved or gone.
    pub fn set_output_layout(&mut self, outputs: &[crate::compositor::backend::Output]) {
        self.output_layout = outputs
            .iter()
            .map(|o| (o.position, o.size, o.scale as f64))
            .collect();
        self.surface_scales.clear();
    }

    /// The scale a surface whose tile sits at `origin` should render at.
    ///
    /// Falls back to the session-wide [`Self::scale`] when no output table
    /// has arrived yet — a hosted or headless run has one display and no
    /// per-output answer to give.
    pub fn scale_for_origin(&self, origin: (f64, f64)) -> f64 {
        if self.output_layout.is_empty() {
            return self.scale;
        }
        crate::compositor::backend::drm::scale::preferred_scale_at(
            &self.output_layout,
            origin.0,
            origin.1,
        )
    }

    /// The preferred scale for one client surface: the scale of the output
    /// it is on, or the session-wide value when it has not been placed.
    pub fn surface_scale(&self, surface_id: u64) -> f64 {
        self.surface_scales
            .get(&surface_id)
            .copied()
            .unwrap_or(self.scale)
    }

    /// Record where each client surface's tile sits and push the resulting
    /// preferred `wp_fractional_scale` to any that changed.
    ///
    /// `placements` is `(surface id, tile origin)`. Only surfaces whose
    /// value actually moved generate wire traffic — `set_preferred_scale`
    /// is idempotent, but recomputing the whole table every frame and
    /// calling it unconditionally would still walk every surface's state
    /// lock for nothing.
    pub fn update_surface_scales(&mut self, placements: &[(u64, (f64, f64))]) {
        for (surface_id, origin) in placements {
            let scale = self.scale_for_origin(*origin);
            let previous = self.surface_scales.insert(*surface_id, scale);
            if previous == Some(scale) {
                continue;
            }
            let Some(surface) = self.client_input.surface(*surface_id).cloned() else {
                continue;
            };
            wl_compositor::with_states(&surface, |states| {
                fractional_scale::with_fractional_scale(states, |fs| {
                    fs.set_preferred_scale(scale);
                });
            });
        }
        // Surfaces that are no longer placed lose their cached scale, so a
        // re-placed one is recomputed rather than resurrected at whatever
        // it happened to be last time.
        let live: Vec<u64> = placements.iter().map(|(id, _)| *id).collect();
        self.surface_scales.retain(|id, _| live.contains(id));
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

    /// Stage the buffer a foreign (external-client) toplevel just
    /// committed so its tab can paint the client's actual pixels.
    ///
    /// Split out of [`CompositorHandler::commit`] to keep the hook itself
    /// a plain dispatch. `wl_shm` and dmabuf are both handled by
    /// [`crate::compositor::foreign::ForeignFrameStore::capture_commit`];
    /// a buffer neither path can decode is recorded as an error the tile
    /// renders, never as a silently stale frame.
    fn capture_foreign_surface(&mut self, surface: &WlSurface, surface_id: u64) {
        use crate::compositor::foreign::{CaptureOutcome, store};
        wl_compositor::with_states(surface, |states| {
            let mut attrs = states
                .cached_state
                .get::<wl_compositor::SurfaceAttributes>();
            match &attrs.current().buffer {
                Some(wl_compositor::BufferAssignment::NewBuffer(buffer)) => {
                    if let CaptureOutcome::Failed(reason) =
                        store().capture_commit(surface_id, buffer)
                    {
                        tracing::warn!(
                            target: "shell::compositor::foreign",
                            surface_id,
                            reason,
                            "foreign surface capture failed; tile shows the reason"
                        );
                    }
                }
                Some(wl_compositor::BufferAssignment::Removed) => {
                    // The client unmapped: stop painting the last frame.
                    store().clear_frame(surface_id);
                }
                None => {
                    // Damage-only commit — keep the staged frame.
                }
            }
        });
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
    /// The actor's event loop calls this from its
    /// [`crate::network::NeighbourMsg::RemoteInput`] handler. It is the
    /// only door between the network and the seat, and the whole input
    /// firewall reads top to bottom here:
    ///
    /// 1. host overrides — the kill switch, and physical-input priority;
    /// 2. the per-share `read_only` flag;
    /// 3. surface ownership — is there a share for this pair at all;
    /// 4. region — is the coordinate inside the shared surface's rect;
    /// 5. z-order — is that surface really on top at that point;
    /// 6. keyboard scrubber — system chords, and the focus guard;
    /// 7. rate limit;
    /// 8. inject into the smithay seat.
    ///
    /// Steps 4-7 live in [`crate::network::input_firewall`], which also
    /// translates an accepted pointer coordinate out of the peer's
    /// surface-local space into the output space the seat wants.
    ///
    /// Returns whether the event reached the seat. Use
    /// [`Self::review_remote_input`] when the refusal reason matters.
    pub fn apply_remote_input(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: InputEvent,
    ) -> bool {
        self.review_remote_input(peer, surface, event).is_ok()
    }

    /// [`Self::apply_remote_input`], reporting which rule refused the
    /// event rather than collapsing every refusal to `false`.
    pub fn review_remote_input(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: InputEvent,
    ) -> Result<(), Denial> {
        let now = std::time::Instant::now();

        // The host's own machine wins, unconditionally and before
        // anything about the share is consulted.
        if let Some(denial) = self.remote_input_firewall.host_override(now) {
            return Err(self.refuse_remote_input(peer, surface, &event, denial));
        }

        // Read-only, then ownership. `None` covers a peer inventing a
        // surface id it was never shared.
        match self.share_registry.get(peer, surface) {
            Some(share) if share.read_only => {
                return Err(self.refuse_remote_input(peer, surface, &event, Denial::ReadOnly));
            }
            Some(_) => {}
            None => {
                return Err(self.refuse_remote_input(peer, surface, &event, Denial::NotShared));
            }
        }

        // Region, z-order, scrubber, rate limit. What comes back is the
        // event to inject: pointer coordinates have been moved into
        // output space, having been checked against the rect they were
        // expressed relative to.
        let injectable = match self
            .remote_input_firewall
            .review(peer, surface, &event, now)
        {
            Ok(ev) => ev,
            Err(denial) => return Err(self.refuse_remote_input(peer, surface, &event, denial)),
        };

        // We can't pass `self` twice (once as `&ShareRegistry` via
        // `&self.share_registry` and once as `&mut RemoteInputSink`), so
        // the routing decision is inlined above and the sink call made
        // directly on ourselves.
        RemoteInputSink::apply(self, peer, surface, injectable);
        Ok(())
    }

    /// Record a refused remote-input event and hand the reason back.
    ///
    /// Every denial goes through here so a dropped event is visible in
    /// production logs with the rule that dropped it, and so
    /// `last_remote_input_denial` can be asserted on.
    fn refuse_remote_input(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: &InputEvent,
        denial: Denial,
    ) -> Denial {
        tracing::debug!(
            target: "shell::compositor::state",
            peer, surface, ?event, reason = denial.reason(),
            "remote-input refused"
        );
        self.last_remote_input_denial = Some((peer, surface, denial));
        denial
    }

    /// Engage the remote-input kill switch: every peer's input stops at
    /// once and stays stopped, and no share keeps write access.
    ///
    /// Flipping the shares as well as latching the firewall is what makes
    /// this survive a restart of the input path — a latch alone would be
    /// undone by anything that rebuilt the firewall, and the user pressed
    /// the panic key.
    ///
    /// Returns the number of shares that lost write access, so the caller
    /// can tell the user what just happened.
    pub fn engage_remote_input_kill_switch(&mut self) -> usize {
        self.remote_input_firewall.engage_kill_switch();
        let revoked = self.share_registry.revoke_all_input();
        tracing::warn!(
            target: "shell::compositor::state",
            revoked,
            "remote-input kill switch engaged"
        );
        revoked
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
                let kc: Keycode = raw.into();
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
                    let _: Option<()> =
                        keyboard.input(self, keysym.into(), key_state, serial, time, |_, _, _| {
                            smithay::input::keyboard::FilterResult::<()>::Forward
                        });
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
pub struct ClientState {
    /// Smithay's per-client compositor bookkeeping (transaction queue, scale).
    pub compositor_state: CompositorClientState,
    /// A number unique to this connection for the compositor's lifetime.
    ///
    /// Exists because a Wayland object id is only unique *within* a client:
    /// two clients each get id 10 for their first `wl_surface`. The compositor
    /// keys tabs, share sessions and input routing off a single flat
    /// `SurfaceId` space, so a bare protocol id makes the second client's
    /// window collide with the first's — it is never adopted as its own tab,
    /// and input meant for one is delivered to the other. [`surface_id`]
    /// composes this index with the protocol id to keep that space flat and
    /// still unique.
    ///
    /// Starts at 1: index 0 is reserved for "the client could not be
    /// determined", which happens only for an already-dead resource.
    pub index: u32,
    /// Whether this connection arrived on the compositor's **privileged**
    /// socket rather than the public one.
    ///
    /// The only thing it currently gates is the clipboard-manager protocol
    /// pair (`zwlr_data_control_manager_v1` / `ext_data_control_manager_v1`),
    /// which hands its holder every selection on the seat with no focus
    /// requirement — see [`crate::compositor::selection`] for the full
    /// argument. It is a *socket* privilege rather than a flag a client can
    /// ask for: the sandbox launcher bind-mounts exactly one Wayland socket
    /// into the sandbox, the public one, so a sandboxed app has no path to a
    /// privileged connection.
    pub privileged: bool,
}

/// Hand-written rather than derived so every construction path — including
/// `ClientState::default()` in tests — gets its own [`ClientState::index`]. A
/// derived `Default` would hand every client index 0 and reintroduce exactly
/// the collision the field exists to prevent.
impl Default for ClientState {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Self {
            compositor_state: CompositorClientState::default(),
            index: NEXT.fetch_add(1, Ordering::Relaxed),
            privileged: false,
        }
    }
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

    /// Per-client data for a connection accepted on the privileged socket.
    pub fn privileged_arc() -> Arc<Self> {
        Arc::new(Self {
            privileged: true,
            ..Self::default()
        })
    }
}

/// The compositor's globally unique id for `surface`.
///
/// A Wayland object id is unique only within one client's connection, so the
/// bare `protocol_id` of two clients' first surfaces is the same number. Every
/// place the compositor stores a surface — the workspace tree's foreign tabs,
/// the input router's surface map, the share registry, the frame tracker —
/// keys off one flat `SurfaceId`, so that collision let a second client's
/// window overwrite the first's rather than becoming its own tab.
///
/// The id is the client's connection index in the high 32 bits and the
/// protocol id in the low 32. Both halves are stable for the life of the
/// object, so an id can be recomputed at any point rather than remembered, and
/// the value stays a plain `u64` for the peer-sync wire protocol.
pub fn surface_id(surface: &WlSurface) -> u64 {
    let index = surface
        .client()
        .and_then(|c| c.get_data::<ClientState>().map(|d| d.index))
        // A surface whose client is already gone cannot collide with a live
        // one, and reserving 0 keeps it out of every live client's range.
        .unwrap_or(0);
    ((index as u64) << 32) | surface.id().protocol_id() as u64
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
        // HiDPI: push this surface's preferred `wp_fractional_scale` — the
        // scale of the *output it is on*, not one session-wide number,
        // because a laptop docked to an external monitor has two different
        // right answers. Runs for every surface (not just shared ones) so
        // any HiDPI-aware client that created a `wp_fractional_scale_v1`
        // object learns the scale it should render at.
        // `set_preferred_scale` is idempotent — it only emits a wire event
        // when the value actually changes, and it stores the value even
        // before the client has created the object (smithay replays it on
        // `get_fractional_scale`).
        let scale = self.surface_scale(surface.id().protocol_id() as u64);
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
        let surface_id = crate::compositor::state::surface_id(surface);
        let is_shared = self
            .workspace_tree
            .workspaces
            .iter()
            .any(|ws| ws.is_surface_shared(surface_id));
        // Foreign-surface compositing: a toplevel adopted into a tab needs
        // its buffer captured every commit so
        // `compositor::foreign::ForeignSurfaceWidget` can paint it into
        // the tab's rect. Independent of the collaboration share path
        // above — a surface can be one, both, or neither.
        let is_foreign_tab =
            tab_binding::group_id_for_surface(&self.workspace_tree, surface_id as u64).is_some();
        if is_foreign_tab {
            self.capture_foreign_surface(surface, surface_id as u64);
        }
        if !is_shared {
            return;
        }
        // Pull the most recent buffer assignment off the surface's
        // cached state. `BufferAssignment::NewBuffer(b)` is the
        // freshly-attached `wl_buffer`; `Removed` means the client
        // detached and we should drop the tracker slot.
        wl_compositor::with_states(surface, |states| {
            let mut attrs = states
                .cached_state
                .get::<wl_compositor::SurfaceAttributes>();
            match &attrs.current().buffer {
                Some(wl_compositor::BufferAssignment::NewBuffer(buffer)) => {
                    // Route through path detection — DMABUF buffers
                    // take the direct path (recorded via the tracker's
                    // capture-path counter; encoder pulls the fd from
                    // the wl_buffer). SHM buffers fall through to the
                    // readback + swizzle path.
                    let (path, result) = self
                        .surface_tracker
                        .on_commit_with_path_detection(surface_id, buffer);
                    match result {
                        Ok(_) => {}
                        Err(
                            crate::compositor::surface_readback::ReadbackError::DmaBufUnsupported,
                        ) => {
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
                                    self.surface_tracker
                                        .note_dmabuf_committed(surface_id, dmabuf.clone());
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
        // Smithay's `DmabufHandler` is the "can you actually use this
        // buffer?" gate, and it is the last point at which a client can
        // still be told *no* in a way it can act on: a failed import
        // makes `zwp_linux_buffer_params_v1.create` fail, which is a
        // toolkit's cue to renegotiate or drop to `wl_shm`. Accepting
        // a buffer we cannot composite would instead leave the client
        // submitting frames into a tile that shows an error string, with
        // no protocol-level signal that anything is wrong.
        //
        // So the gate is exactly the importer's own precondition —
        // single-plane, linear (or implicit), RGBA-family — shared with
        // `foreign::import_dmabuf_rgba` so the two can't drift.
        if let Err(err) = crate::compositor::foreign::validate_importable(&dmabuf) {
            tracing::warn!(
                target: "shell::compositor::dmabuf",
                width = dmabuf.width(),
                height = dmabuf.height(),
                fourcc = ?dmabuf.format().code,
                modifier = u64::from(dmabuf.format().modifier),
                planes = dmabuf.num_planes(),
                error = %err,
                "rejecting client dmabuf: not composable by this compositor"
            );
            notifier.failed();
            return;
        }
        // `notifier.successful()` registers the Dmabuf as the buffer's
        // user-data so the commit hook can pull it back via
        // [`smithay::wayland::dmabuf::get_dmabuf`].
        //
        // We don't stash the Dmabuf here because we don't yet know
        // which surface it will land on — a client can import
        // DMABUFs eagerly and attach them to different surfaces
        // later. The per-surface caches are populated at commit time
        // (`foreign::ForeignFrameStore::capture_commit` for tiles,
        // `SurfaceTracker::note_dmabuf_committed` for share sessions).
        let _ = notifier.successful::<Self>();
        tracing::debug!(
            target: "shell::compositor::dmabuf",
            width = dmabuf.width(),
            height = dmabuf.height(),
            fourcc = ?dmabuf.format().code,
            modifier = u64::from(dmabuf.format().modifier),
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
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        let app_id = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        tab_binding::adopt_toplevel(&mut self.workspace_tree, surface_id, app_id.clone());

        // Register the toplevel with the input router so local keyboard /
        // pointer events can be delivered to it once it is focused. The
        // matching chrome-side tab is spliced in by
        // `client_input::sync_foreign_tabs` on the next run-loop tick.
        self.client_input
            .register_toplevel(surface_id, surface.wl_surface().clone(), app_id);

        // Send a default configure so the client can map. Phase 4 will pick a
        // real size/position from the cluster layout; Phase 2 just needs the
        // handshake to complete.
        surface.send_configure();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Phase 6 Task 6.2 — remove the matching `Foreign` tab so the
        // chrome stops advertising a destroyed surface.
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        // If the destroyed surface held the output fullscreen, drop the
        // transient promotion so the layout restores rather than staying
        // collapsed around a group that no longer exists.
        if self.fullscreen_group
            == tab_binding::group_id_for_surface(&self.workspace_tree, surface_id)
        {
            self.fullscreen_group = None;
        }
        let _ = tab_binding::close_tab_for_destroyed_surface(&mut self.workspace_tree, surface_id);
        // C-7 fix: drop the tracker's per-surface buffer slot. The
        // commit path's `BufferAssignment::Removed` branch already
        // forgets the slot when a client `attach(NULL)`s before
        // destroying its toplevel, but a destroy without that prior
        // detach left the `Vec<u8>` (sized at the buffer's last frame)
        // allocated forever. Per-surface memory leak; small per
        // surface, accumulates across long sessions with many
        // short-lived toplevels.
        self.surface_tracker.drop_surface(surface_id as SurfaceId);
        // Same reclaim on the foreign-compositing side: the store holds a
        // full framebuffer per surface, so a destroyed toplevel that kept
        // its entry would leak `width*height*4` bytes for the life of the
        // session.
        crate::compositor::foreign::store().forget(surface_id);
        // Drop the input router's handle so no further key / pointer event
        // can be aimed at a dead surface, and queue the chrome-side tab
        // reap for the next run-loop tick.
        self.client_input.unregister_toplevel(surface_id);
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        // Phase 7 Task 7.2 — clients call `xdg_toplevel::set_app_id`
        // *after* `xdg_surface::get_toplevel` fires `new_toplevel`, so
        // the tab was initially adopted with a synthetic
        // `anon-<surface_id>` placeholder. Re-read the now-populated
        // app-id from the surface data and rewrite the tab in-place so
        // the chrome (and the Phase 7 integration test) see the real
        // app-id.
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
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
                app_id.clone(),
            );
            // Mirror the rename onto the chrome-side tab the input router
            // mounted (or onto its still-pending adoption).
            self.client_input.note_app_id(surface_id, app_id);
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
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _wl_output: Option<WlOutput>) {
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        self.fullscreen_group = tab_binding::group_id_for_surface(&self.workspace_tree, surface_id);
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
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
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
        // surface. Seed it with the scale of the output it is on (or the
        // session-wide one if it has not been placed yet) immediately so
        // the client doesn't have to wait for its first commit to learn
        // the preferred scale. `set_preferred_scale` is idempotent and
        // safe to call before any buffer is attached.
        let scale = self.surface_scale(surface.id().protocol_id() as u64);
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

// Selection: clipboard, primary selection and drag-and-drop. The handler
// impls live in `compositor::selection` next to the mirror they read and
// write; only the dispatch wiring belongs here with the other globals.
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_data_control!(State);
delegate_ext_data_control!(State);

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
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
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

    fn out(id: u32, position: (i32, i32), size: (u32, u32), scale: f32) -> crate::compositor::backend::Output {
        crate::compositor::backend::Output {
            id: crate::compositor::backend::OutputId(id),
            position,
            size,
            scale,
        }
    }

    /// Before any output table arrives — a hosted or headless run — the
    /// session-wide scale is the only answer there is.
    #[test]
    fn without_an_output_table_the_scale_is_the_session_wide_one() {
        let mut state = make_state();
        state.scale = 1.75;
        assert_eq!(state.scale_for_origin((0.0, 0.0)), 1.75);
        assert_eq!(state.surface_scale(7), 1.75);
    }

    /// The docked-laptop case: a dense built-in panel beside a sparse
    /// external monitor. One session-wide number is wrong for one of them.
    #[test]
    fn a_surface_is_told_the_scale_of_the_output_it_sits_on() {
        let mut state = make_state();
        state.scale = 1.0;
        state.set_output_layout(&[
            out(0, (0, 0), (1920, 1080), 1.5),
            out(1, (1920, 0), (2560, 1440), 1.0),
        ]);
        assert_eq!(state.scale_for_origin((100.0, 100.0)), 1.5);
        assert_eq!(state.scale_for_origin((2000.0, 100.0)), 1.0);
    }

    /// A window dragged from the laptop panel to the external monitor has
    /// to be told to re-render at the other one's density. The cache is
    /// what makes that observable to the commit hook.
    #[test]
    fn moving_a_window_between_outputs_changes_its_recorded_scale() {
        let mut state = make_state();
        state.set_output_layout(&[
            out(0, (0, 0), (1920, 1080), 2.0),
            out(1, (1920, 0), (1920, 1080), 1.0),
        ]);
        state.update_surface_scales(&[(42, (10.0, 10.0))]);
        assert_eq!(state.surface_scale(42), 2.0);
        state.update_surface_scales(&[(42, (2500.0, 10.0))]);
        assert_eq!(state.surface_scale(42), 1.0);
    }

    /// A surface that is no longer placed must not keep a stale scale: it
    /// would be handed back on re-placement without being recomputed.
    #[test]
    fn an_unplaced_surface_loses_its_cached_scale() {
        let mut state = make_state();
        state.scale = 1.25;
        state.set_output_layout(&[out(0, (0, 0), (1920, 1080), 2.0)]);
        state.update_surface_scales(&[(42, (10.0, 10.0))]);
        assert_eq!(state.surface_scale(42), 2.0);
        state.update_surface_scales(&[]);
        assert_eq!(
            state.surface_scale(42),
            1.25,
            "an unplaced surface falls back rather than keeping a stale value"
        );
    }

    /// A hotplug invalidates every cached scale, because the layout the
    /// values were derived from is the thing that moved.
    #[test]
    fn a_new_output_layout_drops_the_cached_scales() {
        let mut state = make_state();
        state.scale = 1.0;
        state.set_output_layout(&[out(0, (0, 0), (1920, 1080), 2.0)]);
        state.update_surface_scales(&[(42, (10.0, 10.0))]);
        assert_eq!(state.surface_scale(42), 2.0);
        // The external monitor is unplugged and the laptop panel is now
        // the only display, at a different scale.
        state.set_output_layout(&[out(0, (0, 0), (1920, 1080), 1.5)]);
        assert_eq!(
            state.surface_scale(42),
            1.0,
            "a stale per-surface scale must not survive the layout it came from"
        );
        state.update_surface_scales(&[(42, (10.0, 10.0))]);
        assert_eq!(state.surface_scale(42), 1.5);
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
            SharedFlatpakSession::start(HOST, "app", SURFACE, W, H, cfg()).expect("session"),
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
            SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, cfg()).expect("session"),
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
            SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, cfg()).expect("session"),
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
        assert!(
            !h.take_lock_requested(),
            "lock edge is consumed exactly once"
        );
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
