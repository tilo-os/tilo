//! Smithay `State` struct: the single value threaded through the calloop
//! event loop, following smithay's "one big state" convention — every
//! protocol global attaches via the matching `delegate_*!` macro, expanding
//! into `Dispatch`/`GlobalDispatch` impls routed to the `*Handler` traits.
//! References: smithay 0.7 `examples/minimal.rs`, `examples/compositor.rs`.
//!
//! [`State`] lives for the compositor's lifetime; [`ClientState`] is
//! per-client data attached at `insert_client` time. Trait impls and `delegate_*!` macros follow.

use std::sync::Arc;

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Format, Modifier};
use smithay::input::keyboard::{Layout, XkbConfig};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent};

use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::utils::{Logical, Rectangle, Serial};
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
use smithay::output::Output as SmithayOutput;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::input_method::{
    InputMethodHandler, InputMethodManagerState, PopupSurface as InputMethodPopupSurface,
};
use smithay::wayland::text_input::{TextInputManagerState, TextInputSeat};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::{
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_dmabuf,
    delegate_ext_data_control, delegate_fractional_scale, delegate_output,
    delegate_input_method_manager, delegate_pointer_gestures, delegate_primary_selection,
    delegate_seat, delegate_session_lock, delegate_shm, delegate_text_input_manager,
    delegate_viewporter, delegate_virtual_keyboard_manager, delegate_xdg_decoration,
    delegate_xdg_shell,
};

use crate::collab::flatpak_share::SharedFlatpakSession;
use crate::compositor::remote_seat::RemoteSeats;
use crate::compositor::render_node::{ProbeError, RenderNode};
use crate::compositor::selection::SelectionState;
use crate::compositor::surface_readback::SurfaceTracker;
use crate::compositor::tab_binding;
use crate::network::input_firewall::{Denial, RemoteInputFirewall};
use crate::network::protocol::{PeerId as MsgPeerId, SurfaceId};
use crate::network::streaming::{InputEvent, RemoteAction, RemoteInputSink, ShareRegistry};
use crate::wayland::color_management::ColorManagerState;

use crate::shell::state::WorkspaceTree;
use smithay::backend::input::KeyState;
use std::collections::HashMap;
use vello::kurbo::Rect;

/// Stable identity from `apps/osk/qos-app.toml`. It is necessary but never
/// sufficient for promotion: the connection must also be accepted on the
/// compositor's private OSK socket.
pub const OSK_APP_ID: &str = "dev.qos.osk";

/// A deliberately narrow app-id convention for the clean, audience-facing
/// side of a presentation.  It is a normal xdg toplevel, but qOS owns its
/// placement so it cannot collapse the presenter's workspace group.
pub const OUTPUT_PRESENTATION_APP_ID_SUFFIX: &str = ".audience";

fn is_output_presentation_app_id(app_id: &str) -> bool {
    app_id
        .strip_suffix(OUTPUT_PRESENTATION_APP_ID_SUFFIX)
        .is_some_and(|base| !base.is_empty())
}

/// `.audience` is not a privilege a random client can claim. The matching
/// presenter must already be a live, ordinary toplevel from the *same*
/// Wayland connection, with the exact base app-id.
fn output_presentation_owner_is_live(
    tree: &WorkspaceTree,
    audience_surface: SurfaceId,
    audience_app_id: &str,
) -> bool {
    let Some(base) = audience_app_id.strip_suffix(OUTPUT_PRESENTATION_APP_ID_SUFFIX) else {
        return false;
    };
    let client = audience_surface >> 32;
    tree.workspaces.iter().any(|workspace| {
        workspace.strips.iter().any(|strip| {
            strip.window_groups.iter().any(|group| {
                group.tabs.iter().any(|tab| {
                    matches!(
                        &tab.tile,
                        crate::shell::state::TileContent::Foreign { surface_id, app_id }
                            if *surface_id != audience_surface
                                && (*surface_id >> 32) == client
                                && app_id == base
                    )
                })
            })
        })
    })
}

/// A foreign surface painted directly into one physical output rather than
/// through the workspace tree.  This is intentionally transient: removing a
/// display or destroying the toplevel cannot leave persisted workspace state
/// pointing at a vanished projector.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutputPresentationSurface {
    pub surface_id: SurfaceId,
    pub app_id: String,
    pub output: crate::compositor::backend::OutputId,
    pub rect: Rect,
}

/// Frozen pixels from the final app-owned frame. The shell moves this exact
/// image under the pointer while the authenticated secondary surface warms,
/// then drops it atomically on the first native buffer.
#[derive(Clone, Debug)]
pub(crate) struct FloatingTransferFrame {
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// Primary-app rect the frozen transfer was cut from. The proxy remains here
/// until the app explicitly confirms that its cleared primary frame presented;
/// moving it sooner would reveal the same card in both owners.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FloatingTransferSource {
    pub rect: vello::kurbo::Rect,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingFloatingToplevel {
    pub token: String,
    pub connection: u64,
    pub public_id: String,
    pub app_id: String,
    pub content_rect: vello::kurbo::Rect,
    pub rect: vello::kurbo::Rect,
    pub transfer_frame: Option<FloatingTransferFrame>,
    pub transfer_source: Option<FloatingTransferSource>,
    pub suspended: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FloatingToplevel {
    pub surface_id: u64,
    pub token: String,
    pub connection: u64,
    pub public_id: String,
    pub app_id: String,
    pub content_rect: vello::kurbo::Rect,
    pub rect: vello::kurbo::Rect,
    pub transfer_frame: Option<FloatingTransferFrame>,
    pub transfer_source: Option<FloatingTransferSource>,
    /// Hidden while the activity is attached inside its app. The Wayland
    /// surface and authenticated capability deliberately remain alive so a
    /// later detach can resume without renderer or identity churn.
    pub suspended: bool,
    /// True only after the first commit carrying pixels. Constructing an
    /// `xdg_toplevel` alone is not a visible ownership transfer.
    pub presented: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum FloatingToplevelEvent {
    Mapped(FloatingToplevel),
    Destroyed { connection: u64, public_id: String },
}

fn suspend_floating_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
) -> bool {
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        let changed = !surface.suspended;
        surface.suspended = true;
        return changed;
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        let changed = !pending.suspended;
        pending.suspended = true;
        return changed;
    }
    false
}

fn resume_floating_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
    geometry: (vello::kurbo::Rect, vello::kurbo::Rect),
    transfer_frame: Option<FloatingTransferFrame>,
    transfer_source: Option<FloatingTransferSource>,
) -> Option<String> {
    let (rect, content_rect) = geometry;
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        surface.rect = rect;
        surface.content_rect = content_rect;
        surface.transfer_frame = transfer_frame;
        surface.transfer_source = transfer_source;
        surface.suspended = false;
        // Keep the frozen app frame on top until a fresh buffer commits; the
        // retained window's previous buffer must never flash stale.
        surface.presented = false;
        return Some(surface.token.clone());
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        pending.rect = rect;
        pending.content_rect = content_rect;
        pending.transfer_frame = transfer_frame;
        pending.transfer_source = transfer_source;
        pending.suspended = false;
        return Some(pending.token.clone());
    }
    None
}

fn complete_floating_return_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
) -> bool {
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        if !surface.suspended {
            return false;
        }
        let changed = surface.transfer_source.take().is_some();
        surface.transfer_frame = None;
        return changed;
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        if !pending.suspended {
            return false;
        }
        let changed = pending.transfer_source.take().is_some();
        pending.transfer_frame = None;
        return changed;
    }
    false
}

fn clear_floating_source_entries(
    pending: &mut [PendingFloatingToplevel],
    live: &mut [FloatingToplevel],
    connection: u64,
    public_id: &str,
) -> bool {
    if let Some(surface) = live
        .iter_mut()
        .find(|surface| surface.connection == connection && surface.public_id == public_id)
    {
        // A late detach acknowledgement must never clear the newer reverse
        // handoff proxy after the surface has already been suspended.
        return !surface.suspended && surface.transfer_source.take().is_some();
    }
    if let Some(pending) = pending
        .iter_mut()
        .find(|pending| pending.connection == connection && pending.public_id == public_id)
    {
        return !pending.suspended && pending.transfer_source.take().is_some();
    }
    false
}

fn dismiss_floating_entries(
    pending: &mut Vec<PendingFloatingToplevel>,
    live: &mut Vec<FloatingToplevel>,
    connection: u64,
    public_id: &str,
) -> bool {
    let pending_before = pending.len();
    pending
        .retain(|candidate| candidate.connection != connection || candidate.public_id != public_id);
    let live_before = live.len();
    live.retain(|surface| surface.connection != connection || surface.public_id != public_id);
    pending_before != pending.len() || live_before != live.len()
}

/// Read the persisted `HiDPI` display scale from the shared settings store.
///
/// The Settings app writes `~/.config/qos/settings.json` with a
/// `"display"` object carrying a `"scale"` number (1.0=100%, …); the
/// compositor reads the same file via `settings_store`, resolving the same
/// XDG rule the apps use. Any read/parse failure or out-of-range value falls back to `1.0`.
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
        .and_then(serde_json::Value::as_f64)
    {
        // Clamp to the Settings control's range (100%-200%, with margin) so an out-of-band value can't wedge the UI.
        Some(scale) if (0.5..=3.0).contains(&scale) => scale,
        _ => 1.0,
    }
}

/// Bridges the `ext_session_lock_v1` protocol handler (running inside the
/// smithay dispatch on [`State`]) to the native run loop that owns the
/// authoritative lock state + the compositor-drawn lock overlay.
///
/// The protocol and compositor share **one** locked state: a client `lock`
/// stashes the [`SessionLocker`] here and raises `lock_requested`; the run
/// loop locks, paints the overlay, then confirms via [`SessionLocker::lock`]
/// so `locked` fires only once content is hidden. `unlock_and_destroy` raises `unlock_requested`. Same-thread, so no `Arc`/lock.
#[derive(Default)]
pub struct SessionLockHooks {
    lock_requested: bool,
    unlock_requested: bool,
    pending_locker: Option<SessionLocker>,
}

impl SessionLockHooks {
    /// Take the "a client asked to lock" edge (clears it; true at most once per protocol `lock` request).
    pub fn take_lock_requested(&mut self) -> bool {
        std::mem::take(&mut self.lock_requested)
    }

    /// Take the "a client asked to unlock" edge (clears it).
    pub fn take_unlock_requested(&mut self) -> bool {
        std::mem::take(&mut self.unlock_requested)
    }

    /// Take the stashed [`SessionLocker`] so the run loop can confirm the lock after the overlay is presented; `None` if nothing pending.
    pub fn take_pending_locker(&mut self) -> Option<SessionLocker> {
        self.pending_locker.take()
    }

    /// Whether a client currently holds (or is mid-acquiring) the lock; used only by tests.
    #[cfg(test)]
    pub fn has_pending_locker(&self) -> bool {
        self.pending_locker.is_some()
    }
}

/// One output's `(position, size, scale)` in the compositor's combined
/// coordinate space — see [`State::output_layout`]. Factored into a named
/// alias to keep that field (and every function taking a slice of it) readable.
pub type OutputPlacement = ((i32, i32), (u32, u32), f64);

/// A client-owned transient that is painted above its parent rather than
/// adopted into the workspace tree.  Keeping the protocol handle here lets
/// us reconfigure reactive popups and, more importantly, tear the whole
/// transient chain down when either endpoint disappears.
#[derive(Clone)]
pub(crate) struct TrackedPopup {
    surface: PopupSurface,
    parent: SurfaceId,
    geometry: Rectangle<i32, Logical>,
    grabbed: bool,
}

/// The compositor's process-wide state.
///
/// The single value the calloop event loop hands to every callback, so every Wayland global gets unsynchronised mutable access without `Rc`/`Arc` plumbing.
pub struct State {
    /// Calloop loop handle — for registering input devices, session signals, libinput sources, etc.
    pub loop_handle: LoopHandle<'static, Self>,
    /// `wayland-server` display handle. Cloneable; used to insert new clients when the listening socket accepts a connection.
    pub display_handle: DisplayHandle,
    /// `wl_compositor` + `wl_subcompositor` global state.
    pub compositor_state: CompositorState,
    /// `xdg_wm_base` global state — toplevels and popups.
    pub xdg_shell_state: XdgShellState,
    /// `zxdg_decoration_manager_v1` global state — who draws the frame
    /// around a toplevel.
    ///
    /// This compositor does, always: a window lives in a tab, and the tab's
    /// chrome *is* its decoration. Without this global a toolkit has nothing
    /// to negotiate with and falls back to drawing its own title bar — a
    /// second frame inside the first, subtracting from the client's configured size.
    pub xdg_decoration_state: XdgDecorationState,
    /// `wl_shm` global state — software buffer support; always advertises ARGB8888 + XRGB8888, no extra formats yet.
    pub shm_state: ShmState,
    /// `zwp_linux_dmabuf_v1` global state — enables clients to attach
    /// GPU-allocated (usually GBM) buffers to their surfaces.
    ///
    /// Created **with default feedback** (protocol version 4), carrying the
    /// `main_device` event, now that `wl_drm` is gone the only way a Mesa
    /// client learns which GPU to allocate on. Advertised formats are
    /// RGBA-family under `Modifier::Linear` only, matching what [`crate::compositor::foreign::import_dmabuf_rgba`] can CPU-map.
    pub dmabuf_state: DmabufState,
    /// The advertised `zwp_linux_dmabuf_v1` global handle. Held for the
    /// process lifetime — dropping it would revoke the global from clients that already bound it.
    ///
    /// `None` when [`crate::compositor::render_node::probe`] found no GPU
    /// whose dma-bufs the compositor can actually read. Not advertising is
    /// the honest answer there: Mesa falls back to its `wl_shm` software path instead of offering dmabufs it would have to reject.
    pub dmabuf_global: Option<DmabufGlobal>,
    /// The render node handed to clients as
    /// `zwp_linux_dmabuf_feedback_v1.main_device`, or the reason none was —
    /// kept so the boot log, state dump and integration tests read the *decision* rather than re-deriving it.
    pub dmabuf_device: Result<RenderNode, ProbeError>,
    /// `wl_seat` collection.
    pub seat_state: SeatState<Self>,
    /// Public `text-input-v3`: ordinary clients may describe only their own
    /// focused fields. Smithay couples it to the seat focus and the one input
    /// method below.
    pub text_input_manager_state: TextInputManagerState,
    /// Private `input-method-v2`: its global filter accepts only connections
    /// arriving on the dedicated OSK socket.
    pub input_method_manager_state: InputMethodManagerState,
    /// Private `virtual-keyboard-v1`, retained for editing keys winit's
    /// text-input client cannot yet express (notably Backspace).
    pub virtual_keyboard_manager_state: VirtualKeyboardManagerState,
    /// Clipboard, primary selection and drag-and-drop.
    ///
    /// Owns the four selection globals plus the compositor's clipboard
    /// mirror, letting in-process chrome paste without a Wayland round trip. See [`crate::compositor::selection`] for why the data-control globals are gated to privileged clients.
    pub selection: SelectionState,
    /// `wl_output` + xdg-output manager; concrete outputs are added separately.
    pub output_manager_state: OutputManagerState,
    /// Persisted workspace tree; foreign-client toplevels adopt into it as [`crate::shell::state::TileContent::Foreign`] tabs.
    pub workspace_tree: WorkspaceTree,
    /// Peer-sync share registry. Tracks which peers have been granted
    /// streaming + input access to which surfaces, and the per-peer
    /// `read_only` flag. Inbound [`crate::network::NeighbourMsg::RemoteInput`] events are filtered through this before reaching the seat.
    pub share_registry: ShareRegistry,
    /// The rest of the remote-input firewall: region, z-order, keyboard
    /// scrubber, rate limit, physical-input priority and the kill switch.
    /// The share registry decides *whether* a peer may drive a surface;
    /// this decides what, where and how fast. Fed a view of the screen once per tick — until then it refuses everything.
    pub remote_input_firewall: RemoteInputFirewall,
    /// Last `RemoteInput` event that survived the share-registry gate; read by tests to assert injection without a real `Seat<State>` (v2 replaces this).
    pub last_remote_input: Option<(MsgPeerId, SurfaceId, InputEvent)>,
    /// Last `RemoteInput` event the firewall refused, and the rule that refused it — otherwise indistinguishable from a broken filter.
    pub last_remote_input_denial: Option<(MsgPeerId, SurfaceId, Denial)>,
    /// Per-foreign-surface RGBA staging. [`CompositorHandler::commit`] calls
    /// [`SurfaceTracker::on_commit`] for every committed surface listed in
    /// [`crate::shell::state::Workspace::shared_surfaces`]; the streaming task drains via [`SurfaceTracker::take_latest`] per encode tick.
    pub surface_tracker: SurfaceTracker,
    /// The compositor's `wl_seat` handle with a pointer device already
    /// registered, constructed in [`State::new`]. Peer-sync's
    /// [`Self::apply_remote_input`] injects gated pointer and keyboard events
    /// into this one seat, so every collaborator appears as one coherent
    /// input source to an ordinary Wayland client.
    pub seat: Seat<Self>,
    /// Monotonic counter for input serials — reusing old `Serial`s would make wayland clients ignore the motion/button.
    pub serial_counter: u32,
    /// Wall-clock anchor for input timestamps; pointer events ship `time: u32` in milliseconds since this instant.
    pub start_time: std::time::Instant,
    /// Per-peer virtual-seat attribution. Click focus scopes which shared
    /// surface may receive that peer's keys, while pointer leave preserves
    /// the caret and the active tab retains the one app-facing
    /// keyboard/text-input focus. See
    /// [`crate::compositor::remote_seat::RemoteSeats`].
    pub remote_seats: RemoteSeats,
    /// Every live client surface, keyed by the stable collaboration id.
    /// Native output bookkeeping uses this to emit `wl_surface.enter`, and
    /// remote pointer motion uses it to resolve the real focus surface.
    pub surface_handles: HashMap<SurfaceId, WlSurface>,
    /// Active share sessions keyed by their target `SurfaceId`.
    ///
    /// [`CompositorHandler::commit`] consults this map on every surface
    /// commit — when the surface is in the map, `encode_next_frame` is
    /// pumped the same tick a fresh buffer lands, avoiding a per-frame
    /// stall waiting on the next damage pass. Held as `Arc` since
    /// [`crate::collab::flatpak_share::SharedFlatpakSession`]'s `encode_next_frame` takes `&self`.
    pub active_sessions: HashMap<SurfaceId, Arc<SharedFlatpakSession>>,
    /// Non-blocking bridge from surface commits into the collaboration
    /// transport. Installed by the native compositor when peer sync is on;
    /// absent in isolated tests and offline sessions.
    legacy_stream_publisher:
        Option<Arc<dyn Fn(crate::network::NeighbourMsg) -> bool + Send + Sync>>,
    /// `wp_color_manager_v1` state — advertised primaries/transfers/features
    /// plus the registered global id. HDR clients bind it, query
    /// capabilities, and attach parametric image descriptions; the
    /// flatpak-share encoder reads the current per-surface description through [`crate::wayland::color_management::surface_current_colorimetry`].
    pub color_manager: ColorManagerState,
    /// `wp_fractional_scale_manager_v1` state — lets `HiDPI` clients ask the
    /// compositor for their surface's preferred fractional scale (a
    /// `1/120`-unit integer). The preferred value is [`Self::scale`],
    /// pushed on commit so a client rendering at, say, 1.5× produces crisp output instead of a bilinearly-upscaled 1× buffer.
    pub fractional_scale_manager: FractionalScaleManagerState,
    /// `wp_viewporter` state, fractional scaling's companion protocol — `wp_viewport.set_destination` declares a 1.5×-buffer's logical size, which integer `set_buffer_scale` can't express.
    pub viewporter: ViewporterState,
    /// Current display scale factor (`HiDPI`), sent to clients as the
    /// preferred `wp_fractional_scale`. Read once at session start from the
    /// shared settings store (`display.scale`), so it tracks the Settings app's "Display scale" control. Defaults to `1.0` when unset or absent.
    pub scale: f64,
    /// Every output as `(position, size, scale)` in the compositor's
    /// combined coordinate space, mirrored from the backend.
    ///
    /// [`Self::scale`] is one number for the whole session, only right on a
    /// single-display machine — a docked laptop has a dense panel and a sparse one, and the wrong factor renders soft. This is what lets the preferred `wp_fractional_scale` be per-surface.
    pub output_layout: Vec<OutputPlacement>,
    /// Configurable dimensions keyed by the stable backend output identity.
    /// This is intentionally separate from `output_layout`, whose tuple is
    /// consumed by scale/pointer code and predates targeted fullscreen.
    output_sizes: HashMap<crate::compositor::backend::OutputId, (i32, i32)>,
    output_rects: HashMap<crate::compositor::backend::OutputId, Rect>,
    /// Preferred scale per client surface id, derived from [`Self::output_layout`] + tile position; refreshed by the run loop, the only place with both.
    pub surface_scales: HashMap<u64, f64>,
    /// **Transient fullscreen surface** — the window group id of the
    /// toplevel currently holding the output fullscreen via
    /// `xdg_toplevel.set_fullscreen`, or `None`. Set by
    /// [`XdgShellHandler::fullscreen_request`], cleared by
    /// `unfullscreen_request` (or surface destroy). The native render loop
    /// mirrors this into the `FrameBuilder` each frame; deliberately NOT
    /// part of the persisted `WorkspaceTree` (a live presentation must never survive a reboot).
    pub fullscreen_group: Option<u64>,
    /// Physical/virtual output requested by the fullscreen client. Kept
    /// separately from the group because hotplug can invalidate the output
    /// while the toplevel still exists; the native loop then reconciles to a
    /// safe fallback before it paints another frame.
    pub fullscreen_output: Option<crate::compositor::backend::OutputId>,
    /// Audience presentation surfaces kept out of `WorkspaceTree`. Each one
    /// is painted into exactly one output rectangle by the native compositor.
    pub(crate) output_presentation_surfaces: Vec<OutputPresentationSurface>,
    /// A fullscreen request can arrive before `set_app_id`; retain its output
    /// hint until the audience convention is known rather than briefly
    /// fullscreening the anonymous tab in the presenter's workspace.
    pending_presentation_outputs: HashMap<SurfaceId, Option<crate::compositor::backend::OutputId>>,
    /// Pointer clicks opt into audience keyboard focus.  Keeping this
    /// separate from workspace focus means starting a presentation never
    /// steals the presenter's local keyboard.
    output_presentation_focus: Option<SurfaceId>,
    output_presentation_sequences: HashMap<SurfaceId, u64>,
    /// Combined output size in physical pixels, mirrored from the backend
    /// (`(0, 0)` until the first output is known). Used as the size in a
    /// fullscreen `xdg_toplevel` configure; `(0, 0)` leaves the size unspecified so the client keeps its own.
    pub output_size: (i32, i32),
    /// `ext_session_lock_v1` global state (tracks which outputs a client lock covered), letting `loginctl lock-session` drive the lock via protocol.
    pub session_lock_state: SessionLockManagerState,
    /// Bridge from the protocol handler to the native run loop's authoritative lock (see [`SessionLockHooks`]).
    pub session_lock: SessionLockHooks,
    /// `zwp_pointer_gestures_v1` global state — advertises touchpad
    /// pinch/swipe/hold gestures to clients. On a bare-TTY (DRM) session
    /// Mutter isn't around to expose gestures, so the compositor does; winit
    /// clients bind it per pointer. Delivering to the focused surface is
    /// gated on routing local libinput input through the seat (today local input drives shell chrome, not client surfaces).
    pub pointer_gestures_state: PointerGesturesState,
    /// Local-input → client-surface routing state: the live foreign-surface
    /// registry, seat focus bookkeeping, and the queue of foreign toplevels
    /// waiting to be mirrored into the shell chrome's tab tree. Every method lives in [`crate::compositor::client_input`].
    pub client_input: crate::compositor::client_input::ClientInput,
    /// XDG popups are separate surfaces, not tabs.  They are captured and
    /// composed above their parent through this registry.
    pub(crate) popups: Vec<TrackedPopup>,
    /// Input-method candidate surfaces are similarly transient.  They stay
    /// outside the app tree so a candidate list can never become a tab.
    pub(crate) input_method_popups: Vec<InputMethodPopupSurface>,
    /// The trusted OSK's xdg-toplevel surface, promoted out of the workspace
    /// tree and positioned by [`crate::compositor::frame::FrameBuilder`].
    pub osk_surface: Option<u64>,
    /// One-time capabilities issued through the authenticated shell-ui
    /// service. A public Wayland client is promoted only when its app-id
    /// carries an exact token from this set.
    pub(crate) pending_floating_toplevels: Vec<PendingFloatingToplevel>,
    /// Live app-owned secondary surfaces, outside the normal workspace tree.
    pub(crate) floating_toplevels: Vec<FloatingToplevel>,
    pub(crate) floating_toplevel_events: Vec<FloatingToplevelEvent>,
}

impl State {
    pub(crate) fn authorize_floating_toplevel(&mut self, pending: PendingFloatingToplevel) {
        self.pending_floating_toplevels.retain(|candidate| {
            candidate.connection != pending.connection || candidate.public_id != pending.public_id
        });
        self.pending_floating_toplevels.push(pending);
    }

    fn promote_floating_toplevel(&mut self, surface_id: u64, app_id: &str) -> bool {
        let Some(token) = floating_capability_token(app_id) else {
            return false;
        };
        let Some(index) = self
            .pending_floating_toplevels
            .iter()
            .position(|pending| pending.token == token)
        else {
            return false;
        };
        let pending = self.pending_floating_toplevels.remove(index);
        self.client_input.promote_to_overlay(surface_id);
        let surface = FloatingToplevel {
            surface_id,
            token: pending.token,
            connection: pending.connection,
            public_id: pending.public_id,
            app_id: pending.app_id,
            content_rect: pending.content_rect,
            rect: pending.rect,
            transfer_frame: pending.transfer_frame,
            transfer_source: pending.transfer_source,
            suspended: pending.suspended,
            presented: false,
        };
        self.floating_toplevels.push(surface);
        true
    }

    fn mark_floating_toplevel_presented(&mut self, surface_id: u64) {
        let Some(surface) = self
            .floating_toplevels
            .iter_mut()
            .find(|surface| surface.surface_id == surface_id)
        else {
            return;
        };
        if surface.presented {
            return;
        }
        surface.presented = true;
        if !surface.suspended {
            self.floating_toplevel_events
                .push(FloatingToplevelEvent::Mapped(surface.clone()));
        }
    }

    pub(crate) fn update_floating_toplevel_rect(
        &mut self,
        connection: u64,
        public_id: &str,
        rect: vello::kurbo::Rect,
        content_rect: vello::kurbo::Rect,
    ) -> bool {
        if let Some(surface) = self
            .floating_toplevels
            .iter_mut()
            .find(|surface| surface.connection == connection && surface.public_id == public_id)
        {
            let changed = surface.rect != rect || surface.content_rect != content_rect;
            surface.rect = rect;
            surface.content_rect = content_rect;
            return changed;
        }
        if let Some(pending) = self
            .pending_floating_toplevels
            .iter_mut()
            .find(|pending| pending.connection == connection && pending.public_id == public_id)
        {
            let changed = pending.rect != rect || pending.content_rect != content_rect;
            pending.rect = rect;
            pending.content_rect = content_rect;
            return changed;
        }
        false
    }

    /// Hide one app-owned surface without invalidating its capability or
    /// dropping its authenticated Wayland identity. This also covers the
    /// very-fast return that arrives before the secondary toplevel maps.
    pub(crate) fn suspend_floating_toplevel(
        &mut self,
        connection: u64,
        public_id: &str,
        return_frame: Option<FloatingTransferFrame>,
    ) -> bool {
        if let Some(frame) = return_frame
            && let Some(surface) = self
                .floating_toplevels
                .iter_mut()
                .find(|surface| surface.connection == connection && surface.public_id == public_id)
        {
            // Preserve the last shell-owned pixels at their exact desktop
            // geometry until the primary app window confirms its replacement
            // frame was presented. This is the reverse of SourceCleared.
            surface.transfer_frame = Some(frame);
            surface.transfer_source = Some(FloatingTransferSource { rect: surface.rect });
        }
        suspend_floating_entries(
            &mut self.pending_floating_toplevels,
            &mut self.floating_toplevels,
            connection,
            public_id,
        )
    }

    /// Release the shell → app return proxy only after the app's primary
    /// render worker confirms the reattached pixels reached presentation.
    pub(crate) fn mark_floating_destination_presented(
        &mut self,
        connection: u64,
        public_id: &str,
    ) -> bool {
        complete_floating_return_entries(
            &mut self.pending_floating_toplevels,
            &mut self.floating_toplevels,
            connection,
            public_id,
        )
    }

    /// Release the source-frame gate only after the app's primary render
    /// worker confirms that a frame without the transferred pixels reached
    /// presentation. Until this call, proxy and native geometry remain pinned
    /// over the captured source rect.
    pub(crate) fn mark_floating_source_cleared(
        &mut self,
        connection: u64,
        public_id: &str,
    ) -> bool {
        clear_floating_source_entries(
            &mut self.pending_floating_toplevels,
            &mut self.floating_toplevels,
            connection,
            public_id,
        )
    }

    /// Resume the exact capability previously suspended for this
    /// authenticated service connection. Returning the original token is the
    /// proof that no new overlay authority was minted for the round trip.
    pub(crate) fn resume_floating_toplevel(
        &mut self,
        connection: u64,
        public_id: &str,
        rect: vello::kurbo::Rect,
        content_rect: vello::kurbo::Rect,
        transfer_frame: Option<FloatingTransferFrame>,
        transfer_source: Option<FloatingTransferSource>,
    ) -> Option<String> {
        resume_floating_entries(
            &mut self.pending_floating_toplevels,
            &mut self.floating_toplevels,
            connection,
            public_id,
            (rect, content_rect),
            transfer_frame,
            transfer_source,
        )
    }

    pub(crate) fn dismiss_floating_toplevel(&mut self, connection: u64, public_id: &str) -> bool {
        dismiss_floating_entries(
            &mut self.pending_floating_toplevels,
            &mut self.floating_toplevels,
            connection,
            public_id,
        )
    }

    pub(crate) fn dismiss_floating_connection(&mut self, connection: u64) -> bool {
        let pending_before = self.pending_floating_toplevels.len();
        let live_before = self.floating_toplevels.len();
        self.pending_floating_toplevels
            .retain(|pending| pending.connection != connection);
        self.floating_toplevels
            .retain(|surface| surface.connection != connection);
        pending_before != self.pending_floating_toplevels.len()
            || live_before != self.floating_toplevels.len()
    }

    pub(crate) fn take_floating_toplevel_events(&mut self) -> Vec<FloatingToplevelEvent> {
        std::mem::take(&mut self.floating_toplevel_events)
    }

    pub(crate) fn is_floating_toplevel(&self, surface_id: u64) -> bool {
        self.floating_toplevels
            .iter()
            .any(|surface| surface.surface_id == surface_id)
    }

    /// Whether the focused client has committed an enabled text-input-v3
    /// session. This is the compositor's source of truth for OSK visibility;
    /// Wayland xdg-toplevels have no client-side visibility request.
    pub fn text_input_active(&self) -> bool {
        let mut active = false;
        self.seat
            .text_input()
            .with_active_text_input(|_, _| active = true);
        active
    }

    /// Build a fresh `State` with all protocol globals registered — each `*::new::<Self>` call advertises the corresponding Wayland global.
    pub fn new(loop_handle: LoopHandle<'static, Self>, display_handle: DisplayHandle) -> Self {
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        // Advertised to every client, unfiltered: hiding this global just makes a toolkit decorate itself.
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let mut seat_state = SeatState::new();
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        let mut dmabuf_state = DmabufState::new();
        let (dmabuf_device, dmabuf_global) = Self::init_dmabuf(&display_handle, &mut dmabuf_state);

        // Register a real seat with a pointer device for `apply_remote_input` to inject into; name matches the wayland.rs sibling for consistency.
        let mut seat = seat_state.new_wl_seat(&display_handle, "quick-desktop");
        seat.add_pointer();
        let initial_keyboard_index = Self::init_seat_keyboard(&mut seat);
        // A touchscreen can't reach a client without this — chrome uses the synthesised pointer stream, but real multi-touch needs `wl_touch`.
        seat.add_touch();
        // Text fields are public; the input method and virtual keyboard are
        // not. Sandboxed apps only see the public Wayland socket. Even a
        // process spoofing `dev.qos.osk` there cannot bind either privileged
        // global: authorization is immutable per-connection state set by the
        // dedicated socket acceptor.
        let text_input_manager_state = TextInputManagerState::new::<Self>(&display_handle);
        let input_method_manager_state =
            InputMethodManagerState::new::<Self, _>(&display_handle, is_osk_client);
        let virtual_keyboard_manager_state =
            VirtualKeyboardManagerState::new::<Self, _>(&display_handle, is_osk_client);
        // Advertise `wp_color_manager_v1`, before the state literal below since `display_handle` is moved into the struct.
        let color_manager = ColorManagerState::new::<Self>(&display_handle);
        // Advertise `wp_fractional_scale_manager_v1` + `wp_viewporter`
        // together, letting HiDPI clients render at a fractional scale
        // (e.g. 1.5×) and have the compositor composite the correct logical size; the preferred scale is pushed per-surface on commit.
        let fractional_scale_manager = FractionalScaleManagerState::new::<Self>(&display_handle);
        let viewporter = ViewporterState::new::<Self>(&display_handle);
        // Advertise `ext_session_lock_manager_v1` for standard-protocol locking; the compositor still owns the lock UI + authentication.
        let session_lock_state =
            SessionLockManagerState::new::<Self, _>(&display_handle, |_client| true);
        // Read the persisted display scale from the shared settings store so the compositor advertises what the Settings app wrote.
        let scale = load_display_scale();
        // Advertise touchpad gestures for a bare-TTY session with no host compositor; constructed before the struct literal moves `display_handle`.
        let pointer_gestures_state = PointerGesturesState::new::<Self>(&display_handle);
        // Clipboard + primary selection + drag-and-drop, advertised before
        // the struct literal (which moves `display_handle`). Selection
        // *focus* is pushed alongside keyboard focus every tick — see `client_input::sync_keyboard_focus` — since smithay gates `set_selection` on it.
        let selection = SelectionState::new(&display_handle);

        let mut state = Self {
            loop_handle,
            display_handle,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            shm_state,
            dmabuf_state,
            dmabuf_global,
            dmabuf_device,
            seat_state,
            text_input_manager_state,
            input_method_manager_state,
            virtual_keyboard_manager_state,
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
            surface_handles: HashMap::new(),
            active_sessions: HashMap::new(),
            legacy_stream_publisher: None,
            color_manager,
            fractional_scale_manager,
            viewporter,
            scale,
            output_layout: Vec::new(),
            output_sizes: HashMap::new(),
            output_rects: HashMap::new(),
            surface_scales: HashMap::new(),
            fullscreen_group: None,
            fullscreen_output: None,
            output_presentation_surfaces: Vec::new(),
            pending_presentation_outputs: HashMap::new(),
            output_presentation_focus: None,
            output_presentation_sequences: HashMap::new(),
            output_size: (0, 0),
            session_lock_state,
            session_lock: SessionLockHooks::default(),
            pointer_gestures_state,
            client_input: crate::compositor::client_input::ClientInput::default(),
            popups: Vec::new(),
            input_method_popups: Vec::new(),
            osk_surface: None,
            pending_floating_toplevels: Vec::new(),
            floating_toplevels: Vec::new(),
            floating_toplevel_events: Vec::new(),
        };
        state.activate_keyboard_source(initial_keyboard_index);
        state
    }

    /// Advertise `zwp_linux_dmabuf_v1` when the probed render node is
    /// composable, publishing the decision either way so the import side can rely on it. Split out of [`Self::new`] to keep that constructor under the line-count budget.
    fn init_dmabuf(
        display_handle: &DisplayHandle,
        dmabuf_state: &mut DmabufState,
    ) -> (Result<RenderNode, ProbeError>, Option<DmabufGlobal>) {
        // `zwp_linux_dmabuf_v1`. Two things have to be right for a GPU client:
        //
        // 1. **A main device.** Mesa dropped `wl_drm`, so
        //    `dmabuf_feedback.main_device` (protocol version 4) is the only
        //    channel telling it which GPU to allocate on — a version-3
        //    global leaves it with none, and EGL dies before mapping a toplevel.
        // 2. **Only formats we can composite.** `foreign::import_dmabuf_rgba`
        //    CPU-maps the plane fd — correct only for single-plane RGBA under `Modifier::Linear`, so that's what goes on the wire.
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
        let dmabuf_device = crate::compositor::render_node::probe();
        let dmabuf_global =
            match &dmabuf_device {
                Ok(node) => {
                    match DmabufFeedbackBuilder::new(node.dev, dmabuf_formats.clone()).build() {
                        Ok(feedback) => {
                            // Publish the decision for the import side: buffers clients hand us were allocated on this GPU, so it must read them back.
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
                                display_handle,
                                &feedback,
                            ))
                        }
                        Err(e) => {
                            // The format table lives in a sealed memfd; failing to create it means no correct feedback — the broken state this block avoids.
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
        (dmabuf_device, dmabuf_global)
    }

    /// Add a keyboard device to `seat` from the persisted XKB settings,
    /// falling back to a bare "us" layout if the saved sources fail to
    /// compile. Returns the settings' `active_index` (or `0` on fallback)
    /// for [`Self::activate_keyboard_source`]. Split out of [`Self::new`] to keep that constructor under the line-count budget.
    fn init_seat_keyboard(seat: &mut Seat<Self>) -> usize {
        // A keyboard on the seat is required for the on-screen keyboard's
        // `virtual-keyboard-v1` client (and any future IME) to deliver keys
        // to the focused surface. Guarded so a keymap-load failure degrades
        // gracefully. Auto-repeat comes from the persisted `input.key_repeat` preference rather than a hard-coded pair.
        let (repeat_delay, repeat_rate) =
            crate::compositor::backend::drm::input_config::InputSettings::load()
                .key_repeat_params();
        let keyboard_settings =
            crate::compositor::backend::drm::input_config::KeyboardSettings::load();
        // Neither side of a repeat rate/delay can realistically reach i32::MAX; clamp rather than let a pathological config value wrap.
        let repeat_delay_ms = i32::try_from(repeat_delay.as_millis()).unwrap_or(i32::MAX);
        let repeat_rate_hz = i32::try_from(repeat_rate).unwrap_or(i32::MAX);
        match seat.add_keyboard(
            XkbConfig {
                layout: &keyboard_settings.layouts,
                variant: &keyboard_settings.variants,
                options: keyboard_settings.options.clone(),
                ..Default::default()
            },
            repeat_delay_ms,
            repeat_rate_hz,
        ) {
            Ok(_) => keyboard_settings.active_index,
            Err(error) => {
                tracing::warn!(?error, "seat: saved XKB sources failed; falling back to US");
                if let Err(fallback_error) = seat.add_keyboard(
                    XkbConfig {
                        layout: "us",
                        ..Default::default()
                    },
                    repeat_delay_ms,
                    repeat_rate_hz,
                ) {
                    tracing::warn!(
                        ?fallback_error,
                        "seat: fallback keymap failed (text injection disabled)"
                    );
                }
                0
            }
        }
    }

    fn activate_keyboard_source(&mut self, index: usize) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        keyboard.with_xkb_state(self, |mut context| {
            context.set_layout(Layout(u32::try_from(index).unwrap_or(0)));
        });
    }

    /// Recompile the Wayland seat keymap from per-user settings and notify
    /// every focused client. A bad/missing XKB symbols file leaves the old
    /// working keymap in place.
    pub fn reload_keyboard_settings(&mut self) {
        let settings = crate::compositor::backend::drm::input_config::KeyboardSettings::load();
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let config = XkbConfig {
            layout: &settings.layouts,
            variant: &settings.variants,
            options: settings.options.clone(),
            ..Default::default()
        };
        match keyboard.set_xkb_config(self, config) {
            Ok(()) => {
                self.activate_keyboard_source(settings.active_index);
                let (delay, rate) =
                    crate::compositor::backend::drm::input_config::InputSettings::load()
                        .key_repeat_params();
                // Neither side of a repeat rate/delay can realistically reach i32::MAX; clamp rather than let a pathological config value wrap.
                keyboard.change_repeat_info(
                    i32::try_from(rate).unwrap_or(i32::MAX),
                    i32::try_from(delay.as_millis()).unwrap_or(i32::MAX),
                );
                tracing::info!(
                    sources = ?settings.source_ids,
                    active = settings.active_index,
                    "Wayland seat: applied per-user XKB sources"
                );
            }
            Err(error) => tracing::warn!(
                sources = ?settings.source_ids,
                %error,
                "Wayland seat: requested XKB keymap did not compile; preserving the working keymap"
            ),
        }
    }

    /// Mirror the backend's output table.
    ///
    /// Called at startup and on every hotplug. Dropping the per-surface
    /// scale cache alongside it is deliberate: the layout the cached values were derived from is gone, so keeping them would hand clients the scale of a monitor that moved.
    pub fn set_output_layout(&mut self, outputs: &[crate::compositor::backend::Output]) {
        self.output_layout = outputs
            .iter()
            .map(|o| (o.position, o.size, f64::from(o.scale)))
            .collect();
        self.output_sizes = outputs
            .iter()
            .filter_map(|output| {
                let width = i32::try_from(output.size.0).ok()?;
                let height = i32::try_from(output.size.1).ok()?;
                (width > 0 && height > 0).then_some((output.id, (width, height)))
            })
            .collect();
        self.output_rects = outputs
            .iter()
            .filter_map(|output| {
                Self::output_rect(outputs, output.id).map(|rect| (output.id, rect))
            })
            .collect();
        self.surface_scales.clear();
        if self
            .fullscreen_output
            .is_some_and(|wanted| !outputs.iter().any(|o| o.id == wanted))
        {
            // The target was unplugged. Keep the fullscreen request alive but
            // select the first viable output; this is the protocol equivalent
            // of a window manager moving a fullscreen client off a removed
            // monitor rather than leaving an invisible surface behind.
            self.fullscreen_output = outputs
                .iter()
                .find(|output| output.size.0 > 0 && output.size.1 > 0)
                .map(|output| output.id);
        }
        self.rehome_output_presentation_surfaces(outputs);
    }

    fn output_rect(
        outputs: &[crate::compositor::backend::Output],
        id: crate::compositor::backend::OutputId,
    ) -> Option<Rect> {
        let output = outputs
            .iter()
            .find(|output| output.id == id && output.size.0 > 0 && output.size.1 > 0)?;
        Some(Rect::new(
            f64::from(output.position.0),
            f64::from(output.position.1),
            f64::from(output.position.0) + f64::from(output.size.0),
            f64::from(output.position.1) + f64::from(output.size.1),
        ))
    }

    /// Rehome an audience surface deterministically when its chosen output
    /// disappears.  The first usable backend output is also the normal
    /// single-display fallback; no stale desktop-coordinate rect survives a
    /// modeset.
    fn rehome_output_presentation_surfaces(
        &mut self,
        outputs: &[crate::compositor::backend::Output],
    ) {
        for surface in &mut self.output_presentation_surfaces {
            if Self::output_rect(outputs, surface.output).is_none() {
                // Never rehome the audience onto an arbitrary remaining
                // output: in the common unplug case that is the local
                // presenter display. Hide until the client explicitly picks
                // a distinct output or falls back to its single-window mode.
                surface.rect = Rect::ZERO;
                crate::compositor::foreign::store().note_placement(surface.surface_id, Rect::ZERO);
                continue;
            }
            if let Some(rect) = Self::output_rect(outputs, surface.output) {
                surface.rect = rect;
                crate::compositor::foreign::store().note_placement(surface.surface_id, rect);
            }
        }
    }

    fn promote_output_presentation_toplevel(
        &mut self,
        surface_id: SurfaceId,
        app_id: String,
    ) -> bool {
        if !output_presentation_owner_is_live(&self.workspace_tree, surface_id, &app_id) {
            tracing::warn!(surface_id, %app_id, "rejected unpaired audience presentation surface");
            return false;
        }
        if self
            .output_presentation_surfaces
            .iter()
            .any(|surface| surface.surface_id == surface_id)
        {
            return true;
        }
        let requested = self
            .pending_presentation_outputs
            .remove(&surface_id)
            .flatten();
        let output = requested
            .filter(|id| self.output_sizes.contains_key(id))
            .or_else(|| self.output_sizes.keys().min_by_key(|id| id.0).copied());
        let Some(output) = output else {
            // Keep the ordinary tab when there is genuinely no output yet;
            // `app_id_changed` will be called again only on a client change,
            // so an eventual output update promotes through fullscreen.
            self.pending_presentation_outputs
                .insert(surface_id, requested);
            return false;
        };
        let Some((width, height)) = self.output_sizes.get(&output).copied() else {
            return false;
        };
        let rect = self.output_rects.get(&output).copied().unwrap_or_else(|| {
            // A size without a position exists only in bare unit fixtures; a
            // real output update replaces this before the first frame.
            Rect::new(0.0, 0.0, f64::from(width), f64::from(height))
        });
        self.client_input.promote_to_overlay(surface_id);
        crate::compositor::foreign::store().note_placement(surface_id, rect);
        self.output_presentation_surfaces
            .push(OutputPresentationSurface {
                surface_id,
                app_id,
                output,
                rect,
            });
        true
    }

    pub(crate) fn is_output_presentation_surface(&self, surface_id: SurfaceId) -> bool {
        self.output_presentation_surfaces
            .iter()
            .any(|surface| surface.surface_id == surface_id)
    }

    pub(crate) fn output_presentation_rect(&self, surface_id: SurfaceId) -> Option<Rect> {
        self.output_presentation_surfaces
            .iter()
            .find(|surface| surface.surface_id == surface_id)
            .map(|surface| surface.rect)
    }

    pub(crate) fn output_presentation_hit(
        &self,
        point: (f64, f64),
    ) -> Option<crate::compositor::client_input::TileHit> {
        self.output_presentation_hit_unchecked(point)
            .filter(|hit| self.client_input.surface(hit.surface_id).is_some())
    }

    fn output_presentation_hit_unchecked(
        &self,
        point: (f64, f64),
    ) -> Option<crate::compositor::client_input::TileHit> {
        let point = vello::kurbo::Point::new(point.0, point.1);
        self.output_presentation_surfaces
            .iter()
            .rev()
            .find(|surface| surface.rect.contains(point))
            .map(|surface| crate::compositor::client_input::TileHit {
                surface_id: surface.surface_id,
                strip: crate::shell::state::StripId::default(),
                group: 0,
                rect: surface.rect,
            })
    }

    pub(crate) fn focus_output_presentation_surface(&mut self, surface_id: SurfaceId) {
        self.output_presentation_focus = self
            .is_output_presentation_surface(surface_id)
            .then_some(surface_id);
    }

    pub(crate) fn clear_output_presentation_focus(&mut self) {
        self.output_presentation_focus = None;
    }

    fn remove_output_presentation_surface(&mut self, surface_id: SurfaceId) -> bool {
        let before = self.output_presentation_surfaces.len();
        self.output_presentation_surfaces
            .retain(|candidate| candidate.surface_id != surface_id);
        self.pending_presentation_outputs.remove(&surface_id);
        if self.output_presentation_focus == Some(surface_id) {
            self.output_presentation_focus = None;
        }
        before != self.output_presentation_surfaces.len()
    }

    pub(crate) fn focused_output_presentation_surface(&self) -> Option<SurfaceId> {
        self.output_presentation_focus
            .filter(|id| self.is_output_presentation_surface(*id))
            .filter(|id| self.client_input.surface(*id).is_some())
    }

    pub(crate) fn output_presentation_placements(&self) -> Vec<(SurfaceId, (f64, f64))> {
        self.output_presentation_surfaces
            .iter()
            .map(|surface| (surface.surface_id, (surface.rect.x0, surface.rect.y0)))
            .collect()
    }

    pub(crate) fn output_presentation_remote_tiles(&self) -> Vec<(SurfaceId, Rect)> {
        self.output_presentation_remote_tiles_for(|surface_id| {
            self.client_input.surface(surface_id).is_some()
        })
    }

    fn output_presentation_remote_tiles_for(
        &self,
        known: impl Fn(SurfaceId) -> bool,
    ) -> Vec<(SurfaceId, Rect)> {
        self.output_presentation_surfaces
            .iter()
            .filter(|surface| known(surface.surface_id))
            .map(|surface| (surface.surface_id, surface.rect))
            .collect()
    }

    /// Direct surfaces are outside the widget tree, so their foreign-frame
    /// sequence is an explicit repaint edge rather than an animation-frame
    /// request from `ForeignSurfaceWidget`.
    pub(crate) fn output_presentation_frame_changed(&mut self) -> bool {
        let mut changed = false;
        let live = self
            .output_presentation_surfaces
            .iter()
            .map(|surface| surface.surface_id)
            .collect::<Vec<_>>();
        self.output_presentation_sequences
            .retain(|surface_id, _| live.contains(surface_id));
        for surface in &self.output_presentation_surfaces {
            let seq = crate::compositor::foreign::store().frame_seq(surface.surface_id);
            if self
                .output_presentation_sequences
                .insert(surface.surface_id, seq)
                != Some(seq)
            {
                changed = true;
            }
        }
        changed
    }

    /// Resolve the live output rectangle used for a fullscreen configure.
    /// `None` means no usable output is currently known, so the client keeps
    /// its last configured size rather than receiving a fabricated zero size.
    pub fn fullscreen_output_size(&self) -> Option<(i32, i32)> {
        let target = self.fullscreen_output?;
        self.output_sizes.get(&target).copied()
    }

    /// The scale a surface whose tile sits at `origin` should render at.
    /// Falls back to the session-wide [`Self::scale`] when no output table
    /// has arrived — a hosted/headless run has one display and no per-output answer to give.
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

    /// The preferred scale for one client surface: its output's scale, or the session-wide value if not yet placed.
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
    /// value actually moved generate wire traffic — recomputing and calling unconditionally would walk every surface's state lock for nothing.
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
        // Surfaces no longer placed lose their cached scale, so a re-placed one is recomputed, not resurrected at its last value.
        let live: Vec<u64> = placements.iter().map(|(id, _)| *id).collect();
        self.surface_scales.retain(|id, _| live.contains(id));
    }

    /// Install the non-blocking network bridge used by live legacy streams.
    pub fn set_legacy_stream_publisher(
        &mut self,
        publisher: Arc<dyn Fn(crate::network::NeighbourMsg) -> bool + Send + Sync>,
    ) {
        self.legacy_stream_publisher = Some(publisher);
    }

    /// Register a share session so [`CompositorHandler::commit`] pumps
    /// `encode_next_frame` on every commit; replaces any prior session and
    /// announces the new stream before its first frame.
    pub fn register_share_session(&mut self, session: Arc<SharedFlatpakSession>) {
        let surface = session.surface_id;
        if let Some(previous) = self.active_sessions.get(&surface)
            && let Some(publish) = &self.legacy_stream_publisher
        {
            let _ = publish(previous.stream_end_msg());
        }
        if let Some(publish) = &self.legacy_stream_publisher {
            let _ = publish(session.stream_start_msg());
        }
        self.active_sessions.insert(surface, session);
    }

    /// Deregister a share session by surface id (no-op if unregistered) — called on share end or surface destroy.
    pub fn unregister_share_session(&mut self, surface: SurfaceId) {
        if let Some(session) = self.active_sessions.remove(&surface)
            && let Some(publish) = &self.legacy_stream_publisher
        {
            let _ = publish(session.stream_end_msg());
        }
    }

    /// Reconcile encoder lifetimes and viewer permissions with the active
    /// workspace's replicated `shared_surfaces` map. A newly shared static
    /// window is seeded from the compositor's already captured foreign frame,
    /// so it does not have to damage itself again before the first packet.
    pub fn reconcile_legacy_share_sessions(
        &mut self,
        host_peer: MsgPeerId,
        desired: &HashMap<SurfaceId, (String, std::collections::HashSet<MsgPeerId>)>,
        config: &crate::config::FlatpakShareConfig,
    ) {
        let stale = self
            .active_sessions
            .keys()
            .copied()
            .filter(|surface| !desired.contains_key(surface))
            .collect::<Vec<_>>();
        for surface in stale {
            self.unregister_share_session(surface);
        }

        for (&surface, (app_id, recipients)) in desired {
            let recipients = recipients
                .iter()
                .copied()
                .filter(|peer| *peer != host_peer)
                .collect::<std::collections::HashSet<_>>();
            if recipients.is_empty() {
                self.unregister_share_session(surface);
                continue;
            }
            let Some((width, height)) = crate::compositor::foreign::store().frame_size(surface)
            else {
                continue;
            };
            let reusable = self.active_sessions.get(&surface).is_some_and(|session| {
                session.app_id == *app_id && session.dimensions() == (width, height)
            });
            if reusable {
                if let Some(session) = self.active_sessions.get(&surface) {
                    let removed = session
                        .viewer_ids()
                        .into_iter()
                        .filter(|peer| !recipients.contains(peer))
                        .collect::<Vec<_>>();
                    let added = recipients
                        .iter()
                        .copied()
                        .filter(|peer| session.viewer_share(*peer).is_none())
                        .collect::<Vec<_>>();
                    if let Some(publish) = &self.legacy_stream_publisher {
                        for peer in removed {
                            let _ = publish(crate::network::NeighbourMsg::PermissionGrant {
                                peer,
                                capability: format!("workspace-control:{surface}:revoked"),
                            });
                        }
                        for peer in &added {
                            let _ = publish(crate::network::NeighbourMsg::PermissionGrant {
                                peer: *peer,
                                capability: format!("workspace-control:{surface}:granted"),
                            });
                        }
                        if !added.is_empty() {
                            let _ = publish(session.stream_start_msg());
                        }
                    }
                    session.sync_viewers(&recipients);
                }
                continue;
            }

            self.unregister_share_session(surface);
            let Ok(session) =
                SharedFlatpakSession::start(host_peer, app_id, surface, width, height, config)
            else {
                tracing::warn!(
                    surface,
                    app_id,
                    width,
                    height,
                    "legacy share encoder failed to start"
                );
                continue;
            };
            let session = Arc::new(session);
            session.sync_viewers(&recipients);
            if let Some(publish) = &self.legacy_stream_publisher {
                for peer in &recipients {
                    let _ = publish(crate::network::NeighbourMsg::PermissionGrant {
                        peer: *peer,
                        capability: format!("workspace-control:{surface}:granted"),
                    });
                }
            }
            self.register_share_session(Arc::clone(&session));

            if let Some(frame) = crate::compositor::foreign::store().snapshot(surface) {
                session.deposit_captured_rgba(frame.rgba);
                if let Some(frame) = session.encode_next_frame(std::time::Instant::now())
                    && let Some(publish) = &self.legacy_stream_publisher
                {
                    let _ = publish(frame.into_neighbour_msg());
                }
            }
        }
    }

    /// Stage the buffer a foreign (external-client) toplevel just committed
    /// so its tab can paint the client's actual pixels.
    ///
    /// Split out of [`CompositorHandler::commit`] to keep the hook a plain
    /// dispatch. `wl_shm` and dmabuf are both handled by
    /// [`crate::compositor::foreign::ForeignFrameStore::capture_commit`]; a buffer neither path can decode is recorded as an error the tile renders, never a silently stale frame.
    fn capture_foreign_surface(surface: &WlSurface, surface_id: u64) {
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

    /// The exact `commit` capture gate, kept named so output-owned surfaces
    /// cannot silently regress back to “only workspace tabs have pixels”.
    fn should_capture_foreign_surface(&self, surface_id: SurfaceId) -> bool {
        tab_binding::group_id_for_surface(&self.workspace_tree, surface_id).is_some()
            || self.osk_surface == Some(surface_id)
            || self.is_floating_toplevel(surface_id)
            || self.is_output_presentation_surface(surface_id)
            || self.is_transient_surface(surface_id)
    }

    /// Pump the share-session encoder for `surface_id` on a fresh commit.
    /// Called from [`CompositorHandler::commit`] after
    /// [`SurfaceTracker::on_commit`] has staged the buffer bytes.
    ///
    /// `false` when no session is registered or the tracker slot is empty
    /// (damage-only commit); `true` when the deposit was accepted and the
    /// encode tick ran — NOT a guarantee a frame was emitted. Split out so integration tests can drive the tick path without a real `WlSurface`.
    pub fn tick_share_session_for_surface(&mut self, surface_id: SurfaceId) -> bool {
        let Some(session) = self.active_sessions.get(&surface_id).cloned() else {
            return false;
        };
        // DMABUF-direct path takes priority when the commit hook stashed a
        // Dmabuf this tick — `take_latest_dmabuf` drains it so a re-tick
        // without a fresh commit finds none. Falling back to SHM covers the mixed-mode case (a client briefly attaching SHM between DMABUF commits).
        if let Some(dmabuf) = self.surface_tracker.take_latest_dmabuf(surface_id) {
            session.deposit_captured_dmabuf(dmabuf);
            let emitted = session.encode_next_frame(std::time::Instant::now());
            if let Some(frame) = emitted {
                if let Some(publish) = &self.legacy_stream_publisher {
                    let _ = publish(frame.into_neighbour_msg());
                }
                // Encoder actually consumed the DMABUF. Only *now* do we bump
                // the tracker's `dmabuf_direct_count` — the metric reflects
                // real zero-copy, not "DMABUF-shaped but rejected and fell back to SHM".
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
        if let Some(frame) = session.encode_next_frame(std::time::Instant::now())
            && let Some(publish) = &self.legacy_stream_publisher
        {
            let _ = publish(frame.into_neighbour_msg());
        }
        true
    }

    /// Mint a fresh `Serial` for input events — monotonic, and never wraps in practice (32-bit at 1kHz is 49 days of traffic).
    fn next_serial(&mut self) -> Serial {
        self.serial_counter = self.serial_counter.wrapping_add(1);
        Serial::from(self.serial_counter)
    }

    /// Apply a remote-input event from `peer` targeting `surface` — called
    /// from [`crate::network::NeighbourMsg::RemoteInput`], the only door
    /// between the network and the seat. Reads top to bottom:
    ///
    /// 1. host overrides — kill switch, session lock, physical-input priority;
    /// 2. the per-share `read_only` flag;
    /// 3. surface ownership — is there a share for this pair at all;
    /// 4. region — is the coordinate inside the shared surface's rect;
    /// 5. z-order — is that surface really on top at that point;
    /// 6. keyboard scrubber — system chords, and the focus guard;
    /// 7. rate limit;
    /// 8. inject into the smithay seat.
    ///
    /// Steps 4-7 live in [`crate::network::input_firewall`]. Returns
    /// whether the event reached the seat; [`Self::review_remote_input`] gives the refusal reason.
    pub fn apply_remote_input(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: InputEvent,
    ) -> bool {
        self.review_remote_input(peer, surface, event).is_ok()
    }

    /// Mirror the active share session's latest viewer permission into the
    /// compositor firewall, then review and inject the event. This closes the
    /// gap between dynamic `set_read_only` changes and the state-level gate.
    pub fn apply_network_remote_input(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: InputEvent,
    ) -> bool {
        let share = self
            .active_sessions
            .get(&surface)
            .and_then(|session| session.viewer_share(peer));
        if let Some(share) = share {
            self.share_registry.insert(peer, surface, share);
        } else {
            self.share_registry.remove(peer, surface);
        }
        self.apply_remote_input(peer, surface, event)
    }

    /// [`Self::apply_remote_input`], reporting which rule refused the event rather than collapsing every refusal to `false`.
    ///
    /// # Errors
    ///
    /// Returns the [`Denial`] naming the first rule that refused the event:
    /// a host override (kill switch, session lock, physical-input
    /// priority), the share's `read_only` flag, the peer having no share
    /// for `surface` at all, or the region/z-order/keyboard-scrubber/rate-limit checks in [`RemoteInputFirewall::review`].
    #[allow(
        clippy::needless_pass_by_value,
        reason = "event is only ever borrowed here, but this is the public review API; dozens \
                  of call sites in network/input_firewall.rs and compositor/client_input.rs pass \
                  it by value and are out of scope for this change"
    )]
    pub fn review_remote_input(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: InputEvent,
    ) -> Result<(), Denial> {
        let now = std::time::Instant::now();

        // The host's own machine wins, unconditionally, before even the per-peer budget — a flooding peer can't delay the panic key.
        if let Some(denial) = self.remote_input_firewall.host_override(now) {
            return Err(self.refuse_remote_input(peer, surface, &event, denial));
        }

        // Read-only, then ownership — `None` covers a peer inventing a surface id it was never shared.
        match self.share_registry.get(peer, surface) {
            Some(share) if share.read_only => {
                return Err(self.refuse_remote_input(peer, surface, &event, Denial::ReadOnly));
            }
            Some(_) => {}
            None => {
                return Err(self.refuse_remote_input(peer, surface, &event, Denial::NotShared));
            }
        }

        // The app has one keyboard focus, but each collaborator must first
        // focus this particular shared view. Otherwise an authenticated peer
        // could inject keys into an active surface without ever interacting
        // with its PeerView.
        if matches!(event, InputEvent::Key { .. })
            && self
                .remote_seats
                .seat(peer)
                .is_none_or(|seat| seat.focused_surface != Some(surface))
        {
            return Err(self.refuse_remote_input(peer, surface, &event, Denial::FocusMuted));
        }

        // Region, z-order, scrubber, rate limit — what comes back is the event to inject, coordinates already moved into output space.
        let injectable = match self
            .remote_input_firewall
            .review(peer, surface, &event, now)
        {
            Ok(ev) => ev,
            Err(denial) => return Err(self.refuse_remote_input(peer, surface, &event, denial)),
        };

        // Can't pass `self` twice (`&ShareRegistry` and `&mut RemoteInputSink`), so the routing decision is inlined above and the sink call made directly on ourselves.
        RemoteInputSink::apply(self, peer, surface, injectable);
        Ok(())
    }

    /// Record a refused remote-input event and hand the reason back.
    /// Every denial goes through here so a dropped event is visible in
    /// production logs with the rule that dropped it, and `last_remote_input_denial` can be asserted on.
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
    /// once and stays stopped, all held keys are released, and every live
    /// stream/share is terminated.
    ///
    /// Flipping the shares as well as latching the firewall is what makes
    /// this survive a restart of the input path — a latch alone would be
    /// undone by anything that rebuilt the firewall. Returns the number of shares that lost write access, so the caller can tell the user what happened.
    pub fn engage_remote_input_kill_switch(&mut self) -> usize {
        self.remote_input_firewall.engage_kill_switch();
        let revoked = self.share_registry.revoke_all_input();

        // Release keys before dropping attribution. Otherwise a peer which
        // vanished while holding a modifier could leave the app-facing seat
        // stuck after the emergency stop.
        let time = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
        for peer in self.remote_seats.peer_ids() {
            for keycode in self.remote_seats.release_peer_keys(peer) {
                if let Some(keyboard) = self.seat.get_keyboard() {
                    let serial = self.next_serial();
                    let _: Option<()> = keyboard.input(
                        self,
                        keycode.saturating_add(8).into(),
                        KeyState::Released,
                        serial,
                        time,
                        |_, _, _| smithay::input::keyboard::FilterResult::<()>::Forward,
                    );
                }
            }
            self.remote_seats.drop_peer(peer);
        }

        let surfaces = self.active_sessions.keys().copied().collect::<Vec<_>>();
        for surface in surfaces {
            self.unregister_share_session(surface);
        }
        self.share_registry.clear();
        tracing::warn!(
            target: "shell::compositor::state",
            revoked,
            "remote-input kill switch engaged; shares and streams terminated"
        );
        revoked
    }

    /// Inject a single `org.freedesktop.impl.portal.RemoteDesktop`
    /// [`RemoteAction`] into the real seat, targeting whatever surface holds seat focus.
    ///
    /// The *local-app* counterpart to [`Self::apply_remote_input`]
    /// (peer-sync, gated by the share registry). `RemoteDesktop` consent is
    /// mediated by the portal grant + device selection, so an action
    /// reaching here has already cleared consent and drives the seat directly.
    ///
    /// `true` when it reached a live seat device, `false` otherwise (headless, or an unresolvable keysym).
    pub fn apply_remote_desktop_action(&mut self, action: RemoteAction) -> bool {
        // A portal session can run for hours; clamp rather than let the millisecond count wrap.
        let time = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
        let serial = self.next_serial();
        match action {
            RemoteAction::PointerMotionAbsolute { x, y, .. } => {
                self.remote_pointer_motion_absolute(x, y, serial, time)
            }
            RemoteAction::PointerMotionRelative { dx, dy } => {
                self.remote_pointer_motion_relative(dx, dy, serial, time)
            }
            RemoteAction::PointerButton { button, pressed } => {
                self.remote_pointer_button(button, pressed, serial, time)
            }
            RemoteAction::PointerAxis { dx, dy } => self.remote_pointer_axis(dx, dy, time),
            RemoteAction::PointerAxisDiscrete { axis, steps } => {
                self.remote_pointer_axis_discrete(axis, steps, time)
            }
            RemoteAction::KeyboardKeycode { keycode, pressed } => {
                // A negative evdev keycode can only come from a malformed/hostile portal message; treat it as "no such key" rather than wrap.
                self.inject_seat_key(u32::try_from(keycode).unwrap_or(0), pressed, serial, time)
            }
            RemoteAction::KeyboardKeysym { keysym, pressed } => {
                let keysym = u32::try_from(keysym).unwrap_or(0);
                if let Some(kc) = self.keysym_to_keycode(keysym) {
                    self.inject_seat_key(kc, pressed, serial, time)
                } else {
                    tracing::debug!(
                        keysym,
                        "RemoteDesktop: keysym has no keycode in the active layout; dropped"
                    );
                    false
                }
            }
        }
    }

    fn remote_pointer_motion_absolute(
        &mut self,
        x: f64,
        y: f64,
        serial: Serial,
        time: u32,
    ) -> bool {
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

    fn remote_pointer_motion_relative(
        &mut self,
        dx: f64,
        dy: f64,
        serial: Serial,
        time: u32,
    ) -> bool {
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
        // Also surface the raw delta for clients consuming `wp_relative_pointer` (games, pointer-locked apps).
        pointer.relative_motion(
            self,
            None,
            &RelativeMotionEvent {
                delta: (dx, dy).into(),
                delta_unaccel: (dx, dy).into(),
                utime: u64::from(time) * 1000,
            },
        );
        pointer.frame(self);
        true
    }

    fn remote_pointer_button(
        &mut self,
        button: i32,
        pressed: bool,
        serial: Serial,
        time: u32,
    ) -> bool {
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
                // A negative evdev button code can only come from a malformed/hostile portal message.
                button: u32::try_from(button).unwrap_or(0),
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
        true
    }

    fn remote_pointer_axis(&mut self, dx: f64, dy: f64, time: u32) -> bool {
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let mut frame =
            AxisFrame::new(time).source(smithay::backend::input::AxisSource::Continuous);
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

    fn remote_pointer_axis_discrete(&mut self, axis: u32, steps: i32, time: u32) -> bool {
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let ax = if axis == 1 {
            smithay::backend::input::Axis::Horizontal
        } else {
            smithay::backend::input::Axis::Vertical
        };
        // One wheel detent == 120 in the `v120` convention; legacy `value`
        // carries a 15px-per-step fallback for pre-v120 clients.
        // `steps` rides in from the portal wire, so a hostile/buggy sender
        // sending a huge value must saturate rather than overflow.
        let frame = AxisFrame::new(time)
            .source(smithay::backend::input::AxisSource::Wheel)
            .v120(ax, steps.saturating_mul(120))
            .value(ax, f64::from(steps) * 15.0);
        pointer.axis(self, frame);
        pointer.frame(self);
        true
    }

    /// Deliver a raw keycode press/release to the seat keyboard. Shared by
    /// the `RemoteDesktop` keycode + keysym paths; passed straight through to
    /// smithay's `KeyboardHandle::input`, the same convention `network::wayland::WaylandServer::handle_input` and [`Self::apply_remote_input`]'s `Key` arm use.
    fn inject_seat_key(&mut self, keycode: u32, pressed: bool, serial: Serial, time: u32) -> bool {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let key_state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let _: Option<()> =
            keyboard.input(self, keycode.into(), key_state, serial, time, |_, _, _| {
                smithay::input::keyboard::FilterResult::<()>::Forward
            });
        true
    }

    /// Reverse-lookup a keysym to a keycode in the seat keyboard's active
    /// layout, so `NotifyKeyboardKeysym` can be delivered even though
    /// smithay's injection API is keycode-oriented. Scans the evdev/xkb
    /// keycode range for a key whose level-0 symbol matches; `None` when the layout can't produce the keysym (caller should fall back to `NotifyKeyboardKeycode`).
    fn keysym_to_keycode(&mut self, target: u32) -> Option<u32> {
        use smithay::input::keyboard::{Keycode, Keysym};
        let keyboard = self.seat.get_keyboard()?;
        let target = Keysym::new(target);
        keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().ok()?;
            let layout = xkb.active_layout();
            // xkb keycodes start at 8 (evdev 0 == xkb 8); 8..=255 covers a standard keyboard's key range.
            for raw in 8u32..=255 {
                let kc: Keycode = raw.into();
                if xkb.raw_syms_for_key_in_layout(kc, layout).contains(&target) {
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
// Pointer drives the real smithay `Seat<Self>` via `PointerHandle::motion`/
// `button`, matching `network::wayland::WaylandServer::handle_input`.
//
// Keyboard: `InputEvent::Key { keysym, pressed, modifiers }` fires
// `KeyboardHandle::input`; the wire `keysym` is a raw keycode. Modifier
// state flows via explicit modifier press/release events, maintained by
// smithay's `KeyboardInternal::key_input` from the keycode stream; the wire `modifiers` bitfield is diagnostics only.
//
// Focus: the active tab owns the single `Seat<State>`'s keyboard/text-input
// focus. Pointer enter/leave updates per-peer attribution only; pointer motion
// resolves the target `WlSurface` and its tile origin for normal Wayland
// enter/motion delivery.
impl RemoteInputSink for State {
    fn apply(&mut self, peer: MsgPeerId, surface: SurfaceId, event: InputEvent) {
        tracing::debug!(
            target: "shell::compositor::state",
            peer, surface, ?event,
            "remote-input applied"
        );
        // Mirror the event into `last_remote_input` for legacy/test callers and per-user "typing" overlays to find the origin peer id.
        self.last_remote_input = Some((peer, surface, event.clone()));

        // A peer's session can run for hours; clamp rather than let the millisecond count wrap.
        let time = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
        let serial = self.next_serial();
        match event {
            InputEvent::PointerMotion { x, y } => {
                if let Some(pointer) = self.seat.get_pointer() {
                    // The firewall translated the point to output space. Give
                    // smithay the same surface + tile origin as local input so
                    // it emits enter/motion in surface-local coordinates. A
                    // `None` focus here is a Wayland leave, which made real
                    // GTK/Qt/LibreOffice controls impossible to hover/click.
                    let focus =
                        self.surface_handles
                            .get(&surface)
                            .cloned()
                            .and_then(|wl_surface| {
                                self.remote_input_firewall
                                    .view()
                                    .rect(surface)
                                    .map(|rect| (wl_surface, (rect.x0, rect.y0).into()))
                            });
                    pointer.motion(
                        self,
                        focus,
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
                // Preserve compatibility with pre-KeyboardFocus viewers,
                // which used PointerEnter as their focus handshake. New
                // viewers send an explicit click-derived transition too.
                let seat = self.remote_seats.seat_mut(peer);
                let _ = seat.pointer_enter(target);
                let _ = seat.enter(target);
            }
            InputEvent::PointerLeave { surface: _ } => {
                // Leaving ends pointer attribution only. In particular, do
                // not release keys or deactivate text-input-v3: real apps
                // keep their caret after the pointer moves elsewhere.
                let _ = self.remote_seats.seat_mut(peer).pointer_leave();
            }
            InputEvent::KeyboardFocus {
                surface: target,
                focused,
            } => {
                if focused {
                    let _ = self.remote_seats.seat_mut(peer).enter(target);
                    return;
                }
                let _ = self.remote_seats.seat_mut(peer).leave();
                let releases = self.remote_seats.release_peer_keys(peer);
                for keycode in releases {
                    if let Some(keyboard) = self.seat.get_keyboard() {
                        let release_serial = self.next_serial();
                        let _: Option<()> = keyboard.input(
                            self,
                            keycode.saturating_add(8).into(),
                            KeyState::Released,
                            release_serial,
                            time,
                            |_, _, _| smithay::input::keyboard::FilterResult::<()>::Forward,
                        );
                    }
                }
            }
            InputEvent::Key {
                keysym,
                pressed,
                modifiers: _modifiers,
            } => {
                // The peer's wire `keysym` is treated as a raw keycode —
                // see the `InputEvent` doc comment; `KeyboardHandle::input`
                // delivers into the focused client via the seat's xkbcommon state.
                //
                // Modifier state is *not* force-set: smithay's
                // `key_input` maintains it from the keycode stream, so peers send Shift-press → A-press → A-release → Shift-release; the wire `modifiers` bitfield is diagnostics only.
                if self.remote_seats.key_transition(peer, keysym, pressed)
                    && let Some(keyboard) = self.seat.get_keyboard()
                {
                    let key_state = if pressed {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    };
                    // The compatibility wire's historical field name is
                    // `keysym`, but the value is an evdev keycode. Smithay's
                    // keyboard API consumes XKB keycodes (evdev + 8), exactly
                    // like the local-client path.
                    let _: Option<()> = keyboard.input(
                        self,
                        keysym.saturating_add(8).into(),
                        key_state,
                        serial,
                        time,
                        |_, _, _| smithay::input::keyboard::FilterResult::<()>::Forward,
                    );
                }
            }
        }
    }
}

/// Per-client state stored in the `wayland-server` `Client` user-data slot.
/// `CompositorHandler::client_compositor_state` must hand back a
/// `&CompositorClientState`; smithay's convention is to stash one inside a `ClientData` impl so it's lifetime-tied to the client.
pub struct ClientState {
    /// Smithay's per-client compositor bookkeeping (transaction queue, scale).
    pub compositor_state: CompositorClientState,
    /// A number unique to this connection for the compositor's lifetime.
    ///
    /// A Wayland object id is only unique *within* a client: two clients each
    /// get id 10 for their first `wl_surface`. The compositor keys tabs, share
    /// sessions and input routing off a flat `SurfaceId` space, so a bare
    /// protocol id would collide across clients. [`surface_id`] composes this
    /// index with the protocol id to keep that space flat. Starts at 1: index 0 is reserved for "the client could not be determined" (an already-dead resource).
    pub index: u32,
    /// Whether this connection arrived on the compositor's **privileged**
    /// socket rather than the public one.
    ///
    /// The only thing it gates is the clipboard-manager protocol pair
    /// (`zwlr_data_control_manager_v1`/`ext_data_control_manager_v1`), which
    /// hands its holder every selection on the seat with no focus requirement
    /// — see [`crate::compositor::selection`]. A *socket* privilege, not a flag
    /// a client can ask for: the sandbox launcher bind-mounts only the public socket into a sandbox.
    pub privileged: bool,
    /// Connection accepted on the compositor-owned OSK socket. This is the
    /// only credential accepted by the input-method and virtual-keyboard
    /// global filters; an app-id string is never authority.
    pub osk: bool,
}

/// Hand-written, not derived, so every construction path gets its own [`ClientState::index`] — a derived `Default` would hand every client index 0.
impl Default for ClientState {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Self {
            compositor_state: CompositorClientState::default(),
            index: NEXT.fetch_add(1, Ordering::Relaxed),
            privileged: false,
            osk: false,
        }
    }
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl ClientState {
    /// Convenience constructor returning an `Arc` so callers can pass it to `DisplayHandle::insert_client` directly.
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

    /// Per-client state for connections accepted on the private OSK socket.
    pub fn osk_arc() -> Arc<Self> {
        Arc::new(Self {
            osk: true,
            ..Self::default()
        })
    }
}

fn is_osk_client(client: &Client) -> bool {
    client.get_data::<ClientState>().is_some_and(|d| d.osk)
}

/// The compositor's globally unique id for `surface`.
///
/// A Wayland object id is unique only within one client's connection, so
/// two clients' first surfaces share the same `protocol_id`. Every place
/// the compositor stores a surface — tabs, the input router, share
/// registry, frame tracker — keys off one flat `SurfaceId`, avoiding a
/// second client's window overwriting the first's. The id is the client's
/// connection index in the high 32 bits, protocol id in the low 32 — both stable, so it can be recomputed rather than remembered, staying a plain `u64` for the peer-sync wire protocol.
pub fn surface_id(surface: &WlSurface) -> u64 {
    let index = surface
        .client()
        .and_then(|c| c.get_data::<ClientState>().map(|d| d.index))
        // A surface whose client is gone can't collide with a live one; reserving 0 keeps it out of every live client's range.
        .unwrap_or(0);
    (u64::from(index) << 32) | u64::from(surface.id().protocol_id())
}

// Handler trait impls.

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every accepted client is initialised with a `ClientState` in
        // `native::run`, so this is never expected to miss. Still, `shell`
        // runs with `panic = "abort"`: a panic here would end every
        // collaborator's session, not just this client's, so a missing
        // slot degrades to a shared fallback instead.
        static FALLBACK: std::sync::OnceLock<CompositorClientState> = std::sync::OnceLock::new();
        client.get_data::<ClientState>().map_or_else(
            || FALLBACK.get_or_init(CompositorClientState::default),
            |d| &d.compositor_state,
        )
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        self.surface_handles.insert(
            crate::compositor::state::surface_id(surface),
            surface.clone(),
        );
    }

    fn commit(&mut self, surface: &WlSurface) {
        // HiDPI: push this surface's preferred `wp_fractional_scale` — the
        // scale of the *output it is on*, not one session-wide number,
        // since a docked laptop has two different right answers. Runs for
        // every surface so any HiDPI-aware client with a `wp_fractional_scale_v1` object learns its scale. `set_preferred_scale` is idempotent, only emitting a wire event on real change, and stores the value even before the client creates the object (smithay replays it on `get_fractional_scale`).
        let scale = self.surface_scale(crate::compositor::state::surface_id(surface));
        wl_compositor::with_states(surface, |states| {
            fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });

        // Pull the surface's buffer into [`SurfaceTracker`] iff shared on any [`WorkspaceTree`] workspace; most aren't, so `is_surface_shared` short-circuits first.
        let surface_id = crate::compositor::state::surface_id(surface);
        let is_shared = self
            .workspace_tree
            .workspaces
            .iter()
            .any(|ws| ws.is_surface_shared(surface_id))
            || self.active_sessions.contains_key(&surface_id);
        // Foreign-surface compositing: an adopted toplevel's buffer is captured every commit for `ForeignSurfaceWidget` to paint — independent of the share path above.
        if self.should_capture_foreign_surface(surface_id) {
            Self::capture_foreign_surface(surface, surface_id);
        }
        let floating_has_new_buffer = self.is_floating_toplevel(surface_id)
            && wl_compositor::with_states(surface, |states| {
                let mut attrs = states
                    .cached_state
                    .get::<wl_compositor::SurfaceAttributes>();
                matches!(
                    &attrs.current().buffer,
                    Some(wl_compositor::BufferAssignment::NewBuffer(_))
                )
            });
        if floating_has_new_buffer {
            self.mark_floating_toplevel_presented(surface_id);
        }
        if !is_shared {
            return;
        }
        // Pull the most recent buffer assignment: `NewBuffer(b)` is freshly-attached; `Removed` means detach — drop the tracker slot.
        wl_compositor::with_states(surface, |states| {
            let mut attrs = states
                .cached_state
                .get::<wl_compositor::SurfaceAttributes>();
            match &attrs.current().buffer {
                Some(wl_compositor::BufferAssignment::NewBuffer(buffer)) => {
                    // Path detection: DMABUF takes the direct path (capture-path counter, encoder pulls the fd); SHM falls to readback + swizzle.
                    let (path, result) = self
                        .surface_tracker
                        .on_commit_with_path_detection(surface_id, buffer);
                    match result {
                        Ok(_) => {}
                        Err(
                            crate::compositor::surface_readback::ReadbackError::DmaBufUnsupported,
                        ) => {
                            // The tentative path is `DmabufDirect`. Pull the
                            // Dmabuf off the wl_buffer via smithay's dmabuf-role
                            // lookup and stash it on the tracker —
                            // `tick_share_session_for_surface` drains it via
                            // `take_latest_dmabuf` the same tick.
                            //
                            // If `get_dmabuf` fails despite detection saying
                            // "not shm" (an unrecognised buffer type, e.g. `wl_drm`), the tracker slot stays empty this tick — the frame is dropped and no counter bumped, correct for "we don't know how to handle this buffer at all".
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
                    // Nothing attached this commit — preserve the previously-staged frame so a damage-only re-commit doesn't wipe the tracker.
                }
            }
        });

        // Colorimetry passthrough: whatever the client committed with
        // `wp_color_management_surface_v1.set_image_description` becomes
        // the wire-tagged colorimetry the share session reports on stream_start; unset clears the override so the config-driven default wins again.
        if let Some(session) = self.active_sessions.get(&surface_id).cloned() {
            match crate::wayland::color_management::surface_current_colorimetry(surface) {
                Some((color, hdr)) => session.set_color_override(color, hdr),
                None => session.clear_color_override(),
            }
        }

        // Share-session tick hook: if this surface has an active Flatpak
        // share session, deposit the freshly-captured RGBA and pump the
        // encoder the same tick — without this the encoder waits for the
        // render loop's next damage sweep, a full-frame stall. Delegated to `tick_share_session_for_surface` to keep the pump logic testable without a real `WlSurface`.
        let _ = self.tick_share_session_for_surface(surface_id);
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        let id = crate::compositor::state::surface_id(surface);
        self.drop_transients_for_surface(id);
        self.surface_handles.remove(&id);
        self.surface_tracker.forget(id);
        self.unregister_share_session(id);
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
        // buffer?" gate, and the last point a client can be told *no* in a
        // way it can act on: a failed import fails `create`, a toolkit's
        // cue to renegotiate or drop to `wl_shm`. Accepting a buffer we
        // can't composite would instead leave the client submitting into a
        // tile showing an error string with no protocol-level signal. So
        // the gate is exactly the importer's own precondition —
        // single-plane, linear (or implicit), RGBA-family — shared with `foreign::import_dmabuf_rgba` so the two can't drift.
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
        // [`smithay::wayland::dmabuf::get_dmabuf`]. We don't stash the
        // Dmabuf here since we don't yet know which surface it will land
        // on — a client can import DMABUFs eagerly and attach them later.
        // Per-surface caches are populated at commit time (`foreign::ForeignFrameStore::capture_commit` for tiles, `SurfaceTracker::note_dmabuf_committed` for share sessions).
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

/// Both halves are required. The immutable socket credential prevents a
/// public client from gaining overlay/input authority by copying the package
/// id; the exact id prevents another helper on the private connection from
/// accidentally being promoted.
fn trusted_osk_identity(surface: &WlSurface, app_id: Option<&str>) -> bool {
    osk_identity_is_trusted(
        app_id,
        surface
            .client()
            .and_then(|client| client.get_data::<ClientState>().map(|data| data.osk))
            .unwrap_or(false),
    )
}

fn osk_identity_is_trusted(app_id: Option<&str>, socket_authorized: bool) -> bool {
    socket_authorized && app_id == Some(OSK_APP_ID)
}

fn floating_capability_token(app_id: &str) -> Option<&str> {
    let token = app_id.strip_prefix(ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX)?;
    (token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(token)
}

/// Map the client-bound `wl_output` object back to the stable backend id.
/// `OutputGlobals` names its Smithay outputs `qOS-<OutputId>`; parsing only
/// that compositor-owned prefix rejects arbitrary/foreign output objects.
fn requested_output_id(output: Option<&WlOutput>) -> Option<crate::compositor::backend::OutputId> {
    let output = SmithayOutput::from_resource(output?)?;
    output
        .name()
        .strip_prefix("qOS-")?
        .parse::<u32>()
        .ok()
        .map(crate::compositor::backend::OutputId)
}

/// Calculate a popup's protocol geometry against the visible part of its
/// parent coordinate space.  Smithay implements the xdg flip → slide → resize
/// order here; keeping the target in parent coordinates is the part a shell
/// must supply.
fn constrained_popup_geometry(
    positioner: PositionerState,
    parent_origin: (i32, i32),
    output: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let target = Rectangle::new(
        (
            output.loc.x.saturating_sub(parent_origin.0),
            output.loc.y.saturating_sub(parent_origin.1),
        )
            .into(),
        output.size,
    );
    positioner.get_unconstrained_geometry(target)
}

/// Keep an already-sized transient (the input-method protocol supplies its
/// size through the committed buffer) fully visible when it can fit on the
/// output.  Oversized candidates retain their size so the client can decide
/// how to scroll or reflow instead of silently losing content.
fn constrain_transient_rect(mut rect: Rect, bounds: Rect) -> Rect {
    if rect.width() <= bounds.width() {
        let dx = (bounds.x0 - rect.x0).max(0.0) - (rect.x1 - bounds.x1).max(0.0);
        rect = rect.with_origin((rect.x0 + dx, rect.y0));
    }
    if rect.height() <= bounds.height() {
        let dy = (bounds.y0 - rect.y0).max(0.0) - (rect.y1 - bounds.y1).max(0.0);
        rect = rect.with_origin((rect.x0, rect.y0 + dy));
    }
    rect
}

impl State {
    fn popup_parent_rect(&self, id: SurfaceId, depth: usize) -> Option<Rect> {
        if depth > 16 {
            // A client cannot legitimately form a cycle, but never let a
            // malformed transient graph turn input or painting into recursion.
            return None;
        }
        if let Some(rect) = crate::compositor::foreign::store().placement(id) {
            return Some(rect);
        }
        let popup = self
            .popups
            .iter()
            .find(|popup| surface_id(popup.surface.wl_surface()) == id)?;
        let parent = self.popup_parent_rect(popup.parent, depth.saturating_add(1))?;
        Some(Rect::new(
            parent.x0 + f64::from(popup.geometry.loc.x),
            parent.y0 + f64::from(popup.geometry.loc.y),
            parent.x0 + f64::from(popup.geometry.loc.x + popup.geometry.size.w),
            parent.y0 + f64::from(popup.geometry.loc.y + popup.geometry.size.h),
        ))
    }

    fn popup_constraint_output(&self, parent: SurfaceId) -> Rectangle<i32, Logical> {
        let parent_rect = self.popup_parent_rect(parent, 0);
        let output = parent_rect.and_then(|rect| {
            self.output_rects
                .values()
                .find(|output| output.contains(rect.center()))
                .copied()
        });
        let output = output.unwrap_or_else(|| {
            Rect::new(
                0.0,
                0.0,
                f64::from(self.output_size.0.max(0)),
                f64::from(self.output_size.1.max(0)),
            )
        });
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "output geometry is bounded by the i32 Wayland logical coordinate space"
        )]
        Rectangle::new(
            (output.x0.round() as i32, output.y0.round() as i32).into(),
            (
                output.width().round().max(0.0) as i32,
                output.height().round().max(0.0) as i32,
            )
                .into(),
        )
    }

    fn configure_popup(
        &self,
        popup: &PopupSurface,
        parent: SurfaceId,
        positioner: PositionerState,
    ) -> Rectangle<i32, Logical> {
        let parent_origin = self
            .popup_parent_rect(parent, 0)
            .map(|rect| {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_possible_wrap,
                    reason = "Wayland logical coordinates are i32 by protocol"
                )]
                (rect.x0.round() as i32, rect.y0.round() as i32)
            })
            .unwrap_or_default();
        let geometry = constrained_popup_geometry(
            positioner,
            parent_origin,
            self.popup_constraint_output(parent),
        );
        popup.with_pending_state(|state| state.geometry = geometry);
        geometry
    }

    /// Visible transient rectangles, in paint order.  Children follow their
    /// parents and therefore naturally paint/hit-test above them.
    pub(crate) fn popup_surfaces(&self) -> Vec<(SurfaceId, Rect)> {
        self.popups
            .iter()
            .filter(|popup| popup.surface.alive())
            .filter_map(|popup| {
                let parent = self.popup_parent_rect(popup.parent, 0)?;
                let geometry = popup.geometry;
                Some((
                    surface_id(popup.surface.wl_surface()),
                    Rect::new(
                        parent.x0 + f64::from(geometry.loc.x),
                        parent.y0 + f64::from(geometry.loc.y),
                        parent.x0 + f64::from(geometry.loc.x + geometry.size.w),
                        parent.y0 + f64::from(geometry.loc.y + geometry.size.h),
                    ),
                ))
            })
            .collect()
    }

    /// Candidate windows provided by the privileged input method.  They use
    /// the focused text field as their parent and are clipped to its output,
    /// so a candidate strip cannot escape a multi-monitor desktop.
    pub(crate) fn input_method_popup_surfaces(&self) -> Vec<(SurfaceId, Rect)> {
        self.input_method_popups
            .iter()
            .filter(|popup| popup.alive())
            .filter_map(|popup| {
                let parent = popup.get_parent()?;
                let parent_id = surface_id(&parent.surface);
                let parent_rect = self.popup_parent_rect(parent_id, 0)?;
                let (width, height) = crate::compositor::foreign::store()
                    .frame_size(surface_id(popup.wl_surface()))?;
                let location = popup.location();
                let rect = Rect::new(
                    parent_rect.x0 + f64::from(location.x),
                    parent_rect.y0 + f64::from(location.y),
                    parent_rect.x0 + f64::from(location.x) + f64::from(width),
                    parent_rect.y0 + f64::from(location.y) + f64::from(height),
                );
                let output = self.popup_constraint_output(parent_id);
                let bounds = Rect::new(
                    f64::from(output.loc.x),
                    f64::from(output.loc.y),
                    f64::from(output.loc.x + output.size.w),
                    f64::from(output.loc.y + output.size.h),
                );
                Some((
                    surface_id(popup.wl_surface()),
                    constrain_transient_rect(rect, bounds),
                ))
            })
            .collect()
    }

    pub(crate) fn is_transient_surface(&self, id: SurfaceId) -> bool {
        self.popups
            .iter()
            .any(|popup| surface_id(popup.surface.wl_surface()) == id)
            || self
                .input_method_popups
                .iter()
                .any(|popup| surface_id(popup.wl_surface()) == id)
    }

    fn drop_transients_for_surface(&mut self, surface: SurfaceId) {
        let mut removed = Vec::new();
        let mut frontier = vec![surface];
        while let Some(parent) = frontier.pop() {
            for popup in &self.popups {
                let id = surface_id(popup.surface.wl_surface());
                if (id == parent || popup.parent == parent) && !removed.contains(&id) {
                    removed.push(id);
                    frontier.push(id);
                }
            }
        }
        for popup in &self.input_method_popups {
            let id = surface_id(popup.wl_surface());
            let parent = popup.get_parent().map(|parent| surface_id(&parent.surface));
            if id == surface || parent == Some(surface) {
                removed.push(id);
            }
        }
        removed.sort_unstable();
        removed.dedup();
        self.popups
            .retain(|popup| !removed.contains(&surface_id(popup.surface.wl_surface())));
        self.input_method_popups
            .retain(|popup| !removed.contains(&surface_id(popup.wl_surface())));
        for id in removed {
            self.client_input.unregister_transient(id);
            crate::compositor::foreign::store().forget(id);
        }
    }

    fn grabbed_popup(&self) -> Option<SurfaceId> {
        self.popups
            .iter()
            .rev()
            .find(|popup| popup.grabbed && popup.surface.alive())
            .map(|popup| surface_id(popup.surface.wl_surface()))
    }

    /// Honour xdg-popup grabs by dismissing the topmost grabbing popup when a
    /// fresh click lands outside its transient chain.  The client owns the
    /// actual destroy; `popup_done` is the protocol request to do so.
    pub(crate) fn dismiss_popup_grab_outside(&mut self, hit: Option<SurfaceId>) -> bool {
        let Some(popup_id) = self.grabbed_popup() else {
            return false;
        };
        let is_inside_chain = hit.is_some_and(|hit| {
            let mut cursor = Some(hit);
            for _ in 0..16 {
                let Some(current) = cursor else { return false };
                if current == popup_id {
                    return true;
                }
                cursor = self
                    .popups
                    .iter()
                    .find(|popup| surface_id(popup.surface.wl_surface()) == current)
                    .map(|popup| popup.parent);
            }
            false
        });
        if is_inside_chain {
            return false;
        }
        if let Some(popup) = self
            .popups
            .iter_mut()
            .find(|popup| surface_id(popup.surface.wl_surface()) == popup_id)
        {
            popup.grabbed = false;
            popup.surface.send_popup_done();
        }
        true
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    /// A client has created an `xdg_toplevel`. Adopt it into the active
    /// workspace's tab tree, and **do not configure it yet**.
    ///
    /// A window here is a tab, so its size is its tab's size, and nobody
    /// knows that yet: a configure sent before layout could only carry no
    /// size ("you choose" in xdg-shell), and the client would commit at a
    /// size the strip never asked for, snapping to the right one a frame later.
    ///
    /// The handshake finishes one pass later, in
    /// [`crate::compositor::foreign::flush_configures`], once layout gives
    /// this tab a rect. Bounded by a grace period for a tab that never gets laid out.
    ///
    /// `app_id` may be unset here; falls back to `anon-<id>`, rewritten by [`Self::app_id_changed`].
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        let app_id = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        // Register the toplevel with the input router for local key/pointer delivery once focused; the chrome-side tab splices in next tick.
        self.client_input.register_toplevel(
            surface_id,
            surface.wl_surface().clone(),
            app_id.clone(),
        );

        let presentation = app_id.as_deref().is_some_and(is_output_presentation_app_id)
            && self.promote_output_presentation_toplevel(
                surface_id,
                app_id.clone().unwrap_or_default(),
            );
        let floating = app_id
            .as_deref()
            .is_some_and(|app_id| self.promote_floating_toplevel(surface_id, app_id));
        if trusted_osk_identity(surface.wl_surface(), app_id.as_deref()) {
            self.client_input.promote_to_overlay(surface_id);
            self.osk_surface = Some(surface_id);
        } else if !floating && !presentation {
            tab_binding::adopt_toplevel(&mut self.workspace_tree, surface_id, app_id);
        }

        // Start the clock the deferred opening configure is bounded against — adoption is when the wait becomes worth giving up on.
        crate::compositor::foreign::store().note_adopted(surface_id);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Remove the matching `Foreign` tab so the chrome stops advertising a destroyed surface.
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        if self.osk_surface == Some(surface_id) {
            self.osk_surface = None;
        }
        let _ = self.remove_output_presentation_surface(surface_id);
        if let Some(index) = self
            .floating_toplevels
            .iter()
            .position(|candidate| candidate.surface_id == surface_id)
        {
            let surface = self.floating_toplevels.remove(index);
            self.floating_toplevel_events
                .push(FloatingToplevelEvent::Destroyed {
                    connection: surface.connection,
                    public_id: surface.public_id,
                });
        }
        // If the destroyed surface held the output fullscreen, drop the transient promotion so layout restores instead of staying collapsed.
        if self.fullscreen_group
            == tab_binding::group_id_for_surface(&self.workspace_tree, surface_id)
        {
            self.fullscreen_group = None;
            self.fullscreen_output = None;
        }
        let _ = tab_binding::close_tab_for_destroyed_surface(&mut self.workspace_tree, surface_id);
        // Drop the tracker's per-surface buffer slot. The commit path's
        // `BufferAssignment::Removed` branch already forgets the slot on
        // `attach(NULL)`, but a destroy without that prior detach left the
        // `Vec<u8>` allocated forever — a small per-surface leak that accumulates across long sessions with many short-lived toplevels.
        self.surface_tracker.drop_surface(surface_id as SurfaceId);
        // Same reclaim on the foreign-compositing side: a kept entry would leak `width*height*4` bytes for the session's life.
        crate::compositor::foreign::store().forget(surface_id);
        // Drop the input router's handle so no event can target a dead surface, and queue the chrome-side tab reap for next tick.
        self.client_input.unregister_toplevel(surface_id);
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        // Clients call `set_app_id` *after* `get_toplevel` fires
        // `new_toplevel`, so the tab was adopted with a synthetic
        // `anon-<surface_id>` placeholder. Re-read the now-populated app-id and rewrite the tab in-place so the chrome (and the integration test) see the real one.
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        let new_app_id = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        if let Some(app_id) = new_app_id {
            if trusted_osk_identity(surface.wl_surface(), Some(&app_id)) {
                // `set_app_id` normally arrives after `new_toplevel`; undo the
                // temporary anonymous adoption before either tree can expose a
                // keyboard as an ordinary app window.
                let _ = tab_binding::close_tab_for_destroyed_surface(
                    &mut self.workspace_tree,
                    surface_id,
                );
                self.client_input.promote_to_overlay(surface_id);
                self.osk_surface = Some(surface_id);
                return;
            }
            if self.promote_floating_toplevel(surface_id, &app_id) {
                // `new_toplevel` may have inserted an anonymous placeholder
                // before the app-id arrived. Remove it before the next tree
                // mirror can expose a one-frame phantom tab.
                let _ = tab_binding::close_tab_for_destroyed_surface(
                    &mut self.workspace_tree,
                    surface_id,
                );
                return;
            }
            if is_output_presentation_app_id(&app_id)
                && self.promote_output_presentation_toplevel(surface_id, app_id.clone())
            {
                let provisional_group =
                    tab_binding::group_id_for_surface(&self.workspace_tree, surface_id);
                // The provisional anonymous tab must never share the
                // presenter's workspace with the clean audience surface.
                let _ = tab_binding::close_tab_for_destroyed_surface(
                    &mut self.workspace_tree,
                    surface_id,
                );
                // `set_fullscreen` is permitted to arrive before
                // `set_app_id`; undo that provisional group promotion now
                // that the toplevel has become output-scoped.
                if self.fullscreen_group == provisional_group {
                    self.fullscreen_group = None;
                    self.fullscreen_output = None;
                }
                return;
            }
            let _ = tab_binding::rename_tab_for_app_id(
                &mut self.workspace_tree,
                surface_id,
                app_id.clone(),
            );
            // Mirror the rename onto the chrome-side tab the input router mounted (or its still-pending adoption).
            self.client_input.note_app_id(surface_id, app_id);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let Some(parent_surface) = surface.get_parent_surface() else {
            // Smithay will report the protocol violation at commit time; send
            // the required initial configure so a malformed client cannot
            // wedge the server's dispatch loop beforehand.
            let _ = surface.send_configure();
            return;
        };
        let parent = surface_id(&parent_surface);
        let geometry = self.configure_popup(&surface, parent, positioner);
        self.client_input.register_transient(
            surface_id(surface.wl_surface()),
            surface.wl_surface().clone(),
        );
        self.popups.push(TrackedPopup {
            surface: surface.clone(),
            parent,
            geometry,
            grabbed: false,
        });
        if let Err(error) = surface.send_configure() {
            tracing::warn!(
                target: "shell::compositor::popup",
                parent,
                error = %error,
                "could not send initial popup configure"
            );
        }
    }

    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        if let Some(popup) = self
            .popups
            .iter_mut()
            .find(|popup| popup.surface == surface)
        {
            popup.grabbed = true;
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let Some(parent) = self
            .popups
            .iter()
            .find(|popup| popup.surface == surface)
            .map(|popup| popup.parent)
        else {
            return;
        };
        let geometry = self.configure_popup(&surface, parent, positioner);
        if let Some(popup) = self
            .popups
            .iter_mut()
            .find(|popup| popup.surface == surface)
        {
            popup.geometry = geometry;
        }
        // `repositioned(token)` is required even if the geometry happens to
        // be unchanged after constraints.
        surface.send_repositioned(token);
    }

    /// A client asked to occupy its output fullscreen
    /// (`xdg_toplevel.set_fullscreen`). We honor it: record which window
    /// group the surface belongs to (so the native render loop promotes it,
    /// suppressing top bar/sidebar/other strips, and restores exactly on
    /// exit), and send a configure carrying `Fullscreen` + the full output
    /// size. A valid `wl_output` hint selects that physical/virtual output;
    /// an absent or stale hint falls back to the combined-session configure
    /// until the native placement path can choose a viable display.
    fn fullscreen_request(&mut self, surface: ToplevelSurface, wl_output: Option<WlOutput>) {
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        let requested = requested_output_id(wl_output.as_ref())
            .filter(|output| self.output_sizes.contains_key(output));
        let presentation = self.is_output_presentation_surface(surface_id);
        if presentation {
            if let Some(surface) = self
                .output_presentation_surfaces
                .iter_mut()
                .find(|candidate| candidate.surface_id == surface_id)
            {
                if let Some(output) = requested {
                    surface.output = output;
                }
                if let Some(rect) = self.output_rects.get(&surface.output).copied() {
                    surface.rect = rect;
                    crate::compositor::foreign::store().note_placement(surface_id, rect);
                }
            }
        } else {
            self.pending_presentation_outputs
                .insert(surface_id, requested);
            self.fullscreen_group =
                tab_binding::group_id_for_surface(&self.workspace_tree, surface_id);
            self.fullscreen_output = requested;
        }
        let (w, h) = if presentation {
            self.output_presentation_surfaces
                .iter()
                .find(|candidate| candidate.surface_id == surface_id)
                .and_then(|candidate| self.output_sizes.get(&candidate.output).copied())
                .unwrap_or(self.output_size)
        } else {
            self.fullscreen_output_size().unwrap_or(self.output_size)
        };
        surface.with_pending_state(|state| {
            state.states.set(
                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
            );
            // Size the surface to the whole output when known; otherwise leave it untouched so the client keeps its own extent.
            if w > 0 && h > 0 {
                state.size = Some((w, h).into());
            }
        });
        surface.send_configure();
    }

    /// A client dropped out of fullscreen (`unset_fullscreen`), or Esc / a
    /// compositor exit flowed back through the UI. Clear the transient
    /// promotion (the native loop restores exact prior tiling) and send a configure clearing `Fullscreen` + the imposed size.
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        if self.is_output_presentation_surface(surface_id) {
            let _ = self.remove_output_presentation_surface(surface_id);
        }
        // Only clear if THIS surface was the fullscreen one — a stale request from a background toplevel must not steal another's.
        if self.fullscreen_group
            == tab_binding::group_id_for_surface(&self.workspace_tree, surface_id)
        {
            self.fullscreen_group = None;
            self.fullscreen_output = None;
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

/// Who draws the frame around a toplevel. On qOS, the compositor —
/// always, and to every client that asks.
///
/// States the shell's model, not a preference: a window lives in a
/// tab, so title/controls/border are the compositor's to draw.
/// `ServerSide` is the answer regardless of what was requested.
///
/// The sharper reason is size: a client that decorates itself
/// subtracts a title bar from its given size — configured 670x668,
/// every winit app committed 670x633, a 35px band of empty tab under every window.
impl XdgDecorationHandler for State {
    /// The client has asked for a decoration object — the first point the
    /// compositor can state a mode. Stating it here matters: this configure
    /// reaches the client before its first buffer, so the size it renders is the one the compositor asked for.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        Self::decorate(&toplevel);
    }

    /// The client has stated a preference; answered with the same mode as always, including `ClientSide` — the request this exists to refuse.
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        tracing::debug!(
            target: "shell::compositor::decoration",
            surface_id = crate::compositor::state::surface_id(toplevel.wl_surface()),
            requested = ?mode,
            "client stated a decoration preference; answering ServerSide",
        );
        Self::decorate(&toplevel);
    }

    /// The client has withdrawn its preference and left the choice to the compositor — it was never anywhere else.
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        Self::decorate(&toplevel);
    }
}

impl State {
    /// Put `ServerSide` in the toplevel's pending state, and send it if the
    /// client has already had its opening configure.
    ///
    /// Smithay only puts a decoration configure on the wire when the mode
    /// changed or the initial one wasn't sent, so this costs one event per real change.
    ///
    /// Before the opening configure, the answer is staged and *not* sent —
    /// a toolkit negotiates decorations in the same round trip it creates
    /// the toplevel, and a configure here would be the client's *first*,
    /// carrying decoration mode and no size, exactly what [`XdgShellHandler::new_toplevel`] stopped sending.
    fn decorate(toplevel: &ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
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

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: InputMethodPopupSurface) {
        let id = surface_id(surface.wl_surface());
        self.client_input
            .register_transient(id, surface.wl_surface().clone());
        self.input_method_popups
            .retain(|popup| popup.wl_surface() != surface.wl_surface());
        self.input_method_popups.push(surface);
    }

    fn dismiss_popup(&mut self, surface: InputMethodPopupSurface) {
        let id = surface_id(surface.wl_surface());
        self.input_method_popups
            .retain(|popup| popup.wl_surface() != surface.wl_surface());
        self.client_input.unregister_transient(id);
        crate::compositor::foreign::store().forget(id);
    }

    fn popup_repositioned(&mut self, surface: InputMethodPopupSurface) {
        // The object stores its new text-input rectangle itself.  Retaining a
        // single current handle makes the next frame pick up the new location
        // without allocating a second overlay or leaving stale candidates.
        let id = surface_id(surface.wl_surface());
        self.input_method_popups
            .retain(|popup| surface_id(popup.wl_surface()) != id);
        self.input_method_popups.push(surface);
    }

    fn parent_geometry(
        &self,
        parent: &WlSurface,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        let rect = self
            .popup_parent_rect(surface_id(parent), 0)
            .unwrap_or_else(|| {
                Rect::new(
                    0.0,
                    0.0,
                    f64::from(self.output_size.0.max(0)),
                    f64::from(self.output_size.1.max(0)),
                )
            });
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "Wayland parent geometry is represented as i32 logical coordinates"
        )]
        Rectangle::new(
            (rect.x0.round() as i32, rect.y0.round() as i32).into(),
            (
                rect.width().round().max(0.0) as i32,
                rect.height().round().max(0.0) as i32,
            )
                .into(),
        )
    }
}

impl OutputHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // A client just created a `wp_fractional_scale_v1` for this
        // surface. Seed it with the output's scale (or session-wide if not
        // yet placed) immediately so it doesn't wait for its first commit; `set_preferred_scale` is idempotent and safe before any buffer is attached.
        let scale = self.surface_scale(crate::compositor::state::surface_id(&surface));
        wl_compositor::with_states(&surface, |states| {
            fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });
    }
}

// Delegate macros — wire each protocol's `Dispatch` impls onto `State`.

delegate_compositor!(State);
delegate_xdg_shell!(State);
// `zxdg_decoration_manager_v1`: how a client is told the compositor draws chrome, rather than left to guess. See `XdgDecorationHandler` above.
delegate_xdg_decoration!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_seat!(State);
delegate_text_input_manager!(State);
delegate_input_method_manager!(State);
delegate_virtual_keyboard_manager!(State);
delegate_pointer_gestures!(State);
delegate_output!(State);
delegate_fractional_scale!(State);
delegate_viewporter!(State);

// Selection: clipboard, primary selection, drag-and-drop. Handler impls live in `compositor::selection`; only dispatch wiring belongs here.
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_data_control!(State);
delegate_ext_data_control!(State);

// `ext_session_lock_v1`: a client can lock the session; while locked the
// compositor hides all normal client surfaces + routes input only to the
// lock surface (enforced by the native run loop, which owns the
// authoritative lock + overlay). qOS renders its own lock UI rather than a
// client lock surface, so the handler bridges protocol lock/unlock into that one lock state via [`SessionLockHooks`].
impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Raise the request for the run loop to lock + paint the overlay; stash confirmation so `.lock()` runs only once content is hidden.
        self.session_lock.lock_requested = true;
        self.session_lock.pending_locker = Some(confirmation);
    }

    fn unlock(&mut self) {
        // The client tore down its lock (having authenticated on its side).
        self.session_lock.unlock_requested = true;
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        // qOS renders its own lock overlay, so a client lock surface is accepted (no error/hang) but not composited; configured to the output size.
        let (w, h) = self.output_size;
        if w > 0 && h > 0 {
            // Already checked positive above, so this never hits the fallback in practice.
            let width = u32::try_from(w).unwrap_or(0);
            let height = u32::try_from(h).unwrap_or(0);
            surface.with_pending_state(|state| {
                state.size = Some((width, height).into());
            });
            surface.send_configure();
        }
    }
}
delegate_session_lock!(State);

// `AsRef<ColorManagerState>` lets the color-management `GlobalDispatch` impl reach the advertised-capability list without a public getter on `State`.
impl AsRef<ColorManagerState> for State {
    fn as_ref(&self) -> &ColorManagerState {
        &self.color_manager
    }
}

// `wp_color_manager_v1` + friends live outside smithay's delegate wrappers, hand-rolled in `wayland::color_management`; this macro wires their `Dispatch`/`GlobalDispatch`.
crate::delegate_color_management!(State);

#[cfg(test)]
mod popup_geometry_tests {
    use super::*;
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
        Anchor, ConstraintAdjustment, Gravity,
    };
    use ui::offscreen::OffscreenRenderer;
    use vello::Scene;
    use vello::kurbo::Affine;
    use vello::peniko::{Color, Fill};

    fn right_anchored_positioner(size: (i32, i32), anchor: (i32, i32)) -> PositionerState {
        PositionerState {
            rect_size: size.into(),
            anchor_rect: Rectangle::new(anchor.into(), (1, 1).into()),
            anchor_edges: Anchor::Right,
            gravity: Gravity::Right,
            constraint_adjustment: ConstraintAdjustment::SlideX,
            ..Default::default()
        }
    }

    fn popup_screen_rect(parent: Rect, geometry: Rectangle<i32, Logical>) -> Rect {
        Rect::new(
            parent.x0 + f64::from(geometry.loc.x),
            parent.y0 + f64::from(geometry.loc.y),
            parent.x0 + f64::from(geometry.loc.x + geometry.size.w),
            parent.y0 + f64::from(geometry.loc.y + geometry.size.h),
        )
    }

    fn draw_rect(scene: &mut Scene, rect: Rect, color: Color) {
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    }

    fn assert_rect_within_viewport(rect: Rect, viewport: Rect) {
        assert!(
            rect.x0 >= viewport.x0
                && rect.y0 >= viewport.y0
                && rect.x1 <= viewport.x1
                && rect.y1 <= viewport.y1,
            "{rect:?} extends outside {viewport:?}"
        );
    }

    #[test]
    fn popup_constraints_are_relative_to_the_parent_not_global_origin() {
        let positioner = PositionerState {
            rect_size: (80, 20).into(),
            anchor_rect: Rectangle::new((95, 10).into(), (1, 1).into()),
            anchor_edges: Anchor::Right,
            gravity: Gravity::Right,
            constraint_adjustment: ConstraintAdjustment::SlideX,
            ..Default::default()
        };
        // The parent begins at x=300 on a 400px output.  The visible target
        // is therefore -300..100 in parent coordinates, not 0..400: using
        // global bounds here would let this menu escape off the output.
        let geometry = constrained_popup_geometry(
            positioner,
            (300, 0),
            Rectangle::new((0, 0).into(), (400, 200).into()),
        );
        assert_eq!(geometry.loc.x, 20);
        assert_eq!(geometry.size.w, 80);
        assert_eq!(geometry.loc.y, 0);
    }

    #[test]
    fn popup_constraints_preserve_requested_size_when_slide_can_fit_it() {
        let positioner = right_anchored_positioner((40, 30), (95, 5));
        let geometry = constrained_popup_geometry(
            positioner,
            (0, 0),
            Rectangle::new((0, 0).into(), (100, 100).into()),
        );
        assert_eq!(geometry.loc.x, 60);
        assert_eq!(geometry.size, (40, 30).into());
    }

    #[test]
    fn popup_and_ime_geometry_fixture_renders_normal_and_scaled_edges()
    -> Result<(), Box<dyn std::error::Error>> {
        let output = Rectangle::new((0, 0).into(), (800, 500).into());
        let parent = Rect::new(280.0, 130.0, 640.0, 370.0);
        let popup = popup_screen_rect(
            parent,
            constrained_popup_geometry(
                right_anchored_positioner((190, 110), (350, 72)),
                (280, 130),
                output,
            ),
        );
        let nested = popup_screen_rect(
            popup,
            constrained_popup_geometry(
                right_anchored_positioner((130, 76), (185, 54)),
                (
                    i32::try_from(popup.x0.round() as i64)?,
                    i32::try_from(popup.y0.round() as i64)?,
                ),
                output,
            ),
        );
        let candidate = constrain_transient_rect(
            Rect::new(
                parent.x1 - 18.0,
                parent.y1 - 22.0,
                parent.x1 + 142.0,
                parent.y1 + 30.0,
            ),
            Rect::new(0.0, 0.0, 800.0, 500.0),
        );
        let normal_viewport = Rect::new(0.0, 0.0, 800.0, 500.0);
        assert_rect_within_viewport(popup, normal_viewport);
        assert_rect_within_viewport(nested, normal_viewport);
        assert_rect_within_viewport(candidate, normal_viewport);

        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/popup-ime-geometry-fixtures");
        std::fs::create_dir_all(&out)?;
        let mut normal = Scene::new();
        draw_rect(
            &mut normal,
            Rect::new(0.0, 0.0, 800.0, 500.0),
            Color::from_rgb8(0x0b, 0x10, 0x1d),
        );
        draw_rect(&mut normal, parent, Color::from_rgb8(0x45, 0x72, 0xb8));
        draw_rect(&mut normal, popup, Color::from_rgb8(0x36, 0xbc, 0xa6));
        draw_rect(&mut normal, nested, Color::from_rgb8(0xf0, 0xa0, 0x45));
        draw_rect(&mut normal, candidate, Color::from_rgb8(0xdc, 0x66, 0x9c));
        let mut renderer = OffscreenRenderer::new(800, 500);
        renderer.render_png(&normal, Color::BLACK, out.join("normal.png"));

        // The compositor positions in logical space.  Reserve a small work-area
        // inset so an edge-constrained transient is visibly inside the physical
        // framebuffer after the 1.5x logical-to-physical transform, rather than
        // appearing cropped by the raster edge.
        let logical_output = Rectangle::new((16, 12).into(), (568, 336).into());
        let logical_parent = Rect::new(340.0, 86.0, 580.0, 266.0);
        let logical_popup = popup_screen_rect(
            logical_parent,
            constrained_popup_geometry(
                right_anchored_positioner((160, 90), (230, 50)),
                (340, 86),
                logical_output,
            ),
        );
        let logical_nested = popup_screen_rect(
            logical_popup,
            constrained_popup_geometry(
                right_anchored_positioner((110, 64), (155, 36)),
                (
                    i32::try_from(logical_popup.x0.round() as i64)?,
                    i32::try_from(logical_popup.y0.round() as i64)?,
                ),
                logical_output,
            ),
        );
        let logical_candidate = constrain_transient_rect(
            Rect::new(550.0, 246.0, 670.0, 292.0),
            Rect::new(16.0, 12.0, 584.0, 348.0),
        );
        let scale = 1.5;
        let scale_rect = |rect: Rect| {
            Rect::new(
                rect.x0 * scale,
                rect.y0 * scale,
                rect.x1 * scale,
                rect.y1 * scale,
            )
        };
        let physical_viewport = Rect::new(0.0, 0.0, 900.0, 540.0);
        let physical_work_area = scale_rect(Rect::new(16.0, 12.0, 584.0, 348.0));
        assert_rect_within_viewport(scale_rect(logical_popup), physical_viewport);
        assert_rect_within_viewport(scale_rect(logical_nested), physical_viewport);
        assert_rect_within_viewport(scale_rect(logical_candidate), physical_viewport);
        // Assert the stronger contract too: all constrained surfaces retain
        // the physical 24px right-edge safe margin, not merely a non-clipped
        // endpoint at the framebuffer boundary.
        assert_rect_within_viewport(scale_rect(logical_popup), physical_work_area);
        assert_rect_within_viewport(scale_rect(logical_nested), physical_work_area);
        assert_rect_within_viewport(scale_rect(logical_candidate), physical_work_area);
        let mut scaled = Scene::new();
        draw_rect(
            &mut scaled,
            Rect::new(0.0, 0.0, 900.0, 540.0),
            Color::from_rgb8(0x0b, 0x10, 0x1d),
        );
        draw_rect(
            &mut scaled,
            scale_rect(logical_parent),
            Color::from_rgb8(0x45, 0x72, 0xb8),
        );
        draw_rect(
            &mut scaled,
            scale_rect(logical_popup),
            Color::from_rgb8(0x36, 0xbc, 0xa6),
        );
        draw_rect(
            &mut scaled,
            scale_rect(logical_nested),
            Color::from_rgb8(0xf0, 0xa0, 0x45),
        );
        draw_rect(
            &mut scaled,
            scale_rect(logical_candidate),
            Color::from_rgb8(0xdc, 0x66, 0x9c),
        );
        let mut scaled_renderer = OffscreenRenderer::new(900, 540);
        scaled_renderer.render_png(
            &scaled,
            Color::BLACK,
            out.join("edge-constrained-scale-150.png"),
        );
        Ok(())
    }
}

#[cfg(test)]
mod osk_identity_tests {
    use super::*;

    #[test]
    fn only_private_socket_plus_exact_app_id_is_trusted() {
        assert!(osk_identity_is_trusted(Some(OSK_APP_ID), true));
        assert!(!osk_identity_is_trusted(Some(OSK_APP_ID), false));
        assert!(!osk_identity_is_trusted(Some("dev.qos.lookalike"), true));
        assert!(!osk_identity_is_trusted(None, true));
    }

    #[test]
    fn public_clients_never_receive_the_osk_global_credential() {
        assert!(!ClientState::default().osk);
        assert!(ClientState::osk_arc().osk);
    }

    #[test]
    fn floating_overlay_identity_requires_a_full_random_capability() {
        let token = "5e8f16a2a2e24bf7ad86758f2f28e316";
        let app_id = format!(
            "{}{}",
            ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
            token
        );
        assert_eq!(floating_capability_token(&app_id), Some(token));
        assert_eq!(
            floating_capability_token(ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX),
            None
        );
        assert_eq!(
            floating_capability_token("dev.qos.shell-float.predictable"),
            None
        );
        assert_eq!(floating_capability_token("dev.qos.chat"), None);
    }
}

#[cfg(test)]
mod floating_lifecycle_tests {
    use super::*;

    fn live_surface() -> FloatingToplevel {
        FloatingToplevel {
            surface_id: 41,
            token: "5e8f16a2a2e24bf7ad86758f2f28e316".into(),
            connection: 7,
            public_id: "maps-navigation:active".into(),
            app_id: "dev.qos.maps".into(),
            content_rect: vello::kurbo::Rect::new(100.0, 80.0, 420.0, 260.0),
            rect: vello::kurbo::Rect::new(92.0, 72.0, 428.0, 268.0),
            transfer_frame: None,
            transfer_source: None,
            suspended: false,
            presented: true,
        }
    }

    #[test]
    fn suspend_and_resume_reuses_the_exact_authenticated_surface() {
        let mut pending = Vec::new();
        let mut live = vec![live_surface()];
        assert!(suspend_floating_entries(
            &mut pending,
            &mut live,
            7,
            "maps-navigation:active"
        ));
        assert!(live[0].suspended);

        let next_window = vello::kurbo::Rect::new(692.0, 52.0, 1_028.0, 248.0);
        let next_content = vello::kurbo::Rect::new(700.0, 60.0, 1_020.0, 240.0);
        let token = resume_floating_entries(
            &mut pending,
            &mut live,
            7,
            "maps-navigation:active",
            (next_window, next_content),
            None,
            None,
        );
        assert_eq!(token.as_deref(), Some("5e8f16a2a2e24bf7ad86758f2f28e316"));
        assert_eq!(live[0].surface_id, 41);
        assert_eq!(live[0].rect, next_window);
        assert!(!live[0].suspended);
        assert!(!live[0].presented, "resume waits for fresh pixels");
    }

    #[test]
    fn return_proxy_survives_suspend_until_primary_destination_is_presented() {
        let mut pending = Vec::new();
        let mut live = vec![live_surface()];
        let return_rect = live[0].rect;
        live[0].transfer_frame = Some(FloatingTransferFrame {
            rgba: std::sync::Arc::new(vec![0xff; 4 * 4 * 4]),
            width: 4,
            height: 4,
        });
        live[0].transfer_source = Some(FloatingTransferSource { rect: return_rect });

        assert!(suspend_floating_entries(
            &mut pending,
            &mut live,
            7,
            "maps-navigation:active"
        ));
        assert!(live[0].suspended);
        assert_eq!(live[0].transfer_source.unwrap().rect, return_rect);
        assert!(live[0].transfer_frame.is_some());
        assert!(
            !clear_floating_source_entries(&mut pending, &mut live, 7, "maps-navigation:active"),
            "a late detach acknowledgement cannot tear down the newer return proxy"
        );
        assert!(live[0].transfer_source.is_some());

        assert!(complete_floating_return_entries(
            &mut pending,
            &mut live,
            7,
            "maps-navigation:active"
        ));
        assert!(live[0].transfer_source.is_none());
        assert!(live[0].transfer_frame.is_none());
        assert!(
            !complete_floating_return_entries(&mut pending, &mut live, 7, "maps-navigation:active"),
            "the destination acknowledgement is idempotent"
        );
    }

    #[test]
    fn capability_cannot_be_resumed_from_another_connection() {
        let mut pending = Vec::new();
        let mut live = vec![live_surface()];
        assert!(
            resume_floating_entries(
                &mut pending,
                &mut live,
                99,
                "maps-navigation:active",
                (vello::kurbo::Rect::ZERO, vello::kurbo::Rect::ZERO),
                None,
                None,
            )
            .is_none()
        );
        assert_eq!(live[0].connection, 7);
        assert!(live[0].presented);
    }

    #[test]
    fn permanent_dismiss_cannot_be_resumed() {
        let mut pending = Vec::new();
        let mut live = vec![live_surface()];
        assert!(dismiss_floating_entries(
            &mut pending,
            &mut live,
            7,
            "maps-navigation:active"
        ));
        assert!(live.is_empty());
        assert!(
            resume_floating_entries(
                &mut pending,
                &mut live,
                7,
                "maps-navigation:active",
                (vello::kurbo::Rect::ZERO, vello::kurbo::Rect::ZERO),
                None,
                None,
            )
            .is_none()
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic compositor tick geometry is the behavior under test"
)]
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

    fn out(
        id: u32,
        position: (i32, i32),
        size: (u32, u32),
        scale: f32,
    ) -> crate::compositor::backend::Output {
        crate::compositor::backend::Output {
            id: crate::compositor::backend::OutputId(id),
            position,
            size,
            scale,
            transform: settings_store::hardware::OutputTransform::Normal,
            color_profile: None,
        }
    }

    /// Before any output table arrives (hosted/headless run), the session-wide scale is the only answer there is.
    #[test]
    fn without_an_output_table_the_scale_is_the_session_wide_one() {
        let mut state = make_state();
        state.scale = 1.75;
        assert_eq!(state.scale_for_origin((0.0, 0.0)), 1.75);
        assert_eq!(state.surface_scale(7), 1.75);
    }

    #[test]
    fn packed_nonzero_client_surface_id_keeps_its_own_scale() {
        let mut state = make_state();
        let id = (17_u64 << 32) | 42;
        state.surface_scales.insert(id, 1.5);
        assert_eq!(state.surface_scale(id), 1.5);
        assert_ne!(state.surface_scale(42), 1.5);
    }

    #[test]
    fn audience_identity_is_narrow_and_does_not_match_the_presenter() {
        assert!(is_output_presentation_app_id("dev.qos.slides.audience"));
        assert!(!is_output_presentation_app_id("dev.qos.slides"));
        assert!(!is_output_presentation_app_id(".audience"));
    }

    #[test]
    fn audience_role_requires_an_exact_same_client_presenter() {
        let mut tree = WorkspaceTree::bootstrap_default();
        tab_binding::adopt_toplevel(&mut tree, (9_u64 << 32) | 1, Some("dev.qos.slides".into()));
        assert!(output_presentation_owner_is_live(
            &tree,
            (9_u64 << 32) | 2,
            "dev.qos.slides.audience"
        ));
        assert!(!output_presentation_owner_is_live(
            &tree,
            (10_u64 << 32) | 2,
            "dev.qos.slides.audience"
        ));
        assert!(!output_presentation_owner_is_live(
            &tree,
            (9_u64 << 32) | 2,
            "dev.qos.evil.audience"
        ));
    }

    #[test]
    fn audience_surface_hides_when_its_distinct_output_is_unplugged() {
        let mut state = make_state();
        state.set_output_layout(&[
            out(1, (0, 0), (1280, 720), 1.0),
            out(2, (1280, 0), (1920, 1080), 1.0),
        ]);
        let presenter_groups = state.workspace_tree.active_workspace().unwrap().strips[0]
            .window_groups
            .len();
        state
            .output_presentation_surfaces
            .push(OutputPresentationSurface {
                surface_id: 77,
                app_id: "dev.qos.slides.audience".into(),
                output: crate::compositor::backend::OutputId(2),
                rect: Rect::ZERO,
            });
        state.set_output_layout(&[out(1, (0, 0), (1280, 720), 1.0)]);
        let audience = &state.output_presentation_surfaces[0];
        assert_eq!(audience.output, crate::compositor::backend::OutputId(2));
        assert_eq!(audience.rect, Rect::ZERO);
        assert_eq!(
            state.workspace_tree.active_workspace().unwrap().strips[0]
                .window_groups
                .len(),
            presenter_groups,
            "hotplug never moves the presenter into the audience layer"
        );
    }

    #[test]
    fn audience_teardown_releases_its_output_and_focus_only() {
        let mut state = make_state();
        state
            .output_presentation_surfaces
            .push(OutputPresentationSurface {
                surface_id: 88,
                app_id: "dev.qos.slides.audience".into(),
                output: crate::compositor::backend::OutputId(2),
                rect: Rect::new(100.0, 0.0, 200.0, 100.0),
            });
        state.output_presentation_focus = Some(88);
        state
            .pending_presentation_outputs
            .insert(88, Some(crate::compositor::backend::OutputId(2)));
        assert!(state.remove_output_presentation_surface(88));
        assert!(state.output_presentation_surfaces.is_empty());
        assert!(state.output_presentation_focus.is_none());
        assert!(!state.pending_presentation_outputs.contains_key(&88));
    }

    #[test]
    fn commit_capture_gate_includes_audience_surface_without_a_workspace_tab() {
        let mut state = make_state();
        let id = (31_u64 << 32) | 9;
        state
            .output_presentation_surfaces
            .push(OutputPresentationSurface {
                surface_id: id,
                app_id: "dev.qos.slides.audience".into(),
                output: crate::compositor::backend::OutputId(2),
                rect: Rect::new(100.0, 0.0, 200.0, 100.0),
            });
        assert!(state.should_capture_foreign_surface(id));
        assert!(tab_binding::group_id_for_surface(&state.workspace_tree, id).is_none());
    }

    #[test]
    fn unpaired_audience_is_rejected_by_the_actual_promotion_gate() {
        let mut state = make_state();
        state.set_output_layout(&[out(2, (0, 0), (1920, 1080), 1.0)]);
        assert!(!state.promote_output_presentation_toplevel(
            (44_u64 << 32) | 2,
            "dev.qos.evil.audience".into()
        ));
        assert!(state.output_presentation_surfaces.is_empty());
    }

    #[test]
    fn audience_hit_and_focus_are_scoped_before_workspace_rows() {
        let mut state = make_state();
        state
            .output_presentation_surfaces
            .push(OutputPresentationSurface {
                surface_id: (8_u64 << 32) | 5,
                app_id: "dev.qos.slides.audience".into(),
                output: crate::compositor::backend::OutputId(2),
                rect: Rect::new(1280.0, 0.0, 1920.0, 720.0),
            });
        let hit = state
            .output_presentation_hit_unchecked((1400.0, 100.0))
            .expect("audience output hit takes precedence");
        assert_eq!(hit.surface_id, (8_u64 << 32) | 5);
        state.focus_output_presentation_surface(hit.surface_id);
        assert_eq!(state.output_presentation_focus, Some(hit.surface_id));
        assert!(
            state
                .output_presentation_hit_unchecked((100.0, 100.0))
                .is_none()
        );
    }

    #[test]
    fn audience_remote_view_includes_only_live_direct_surface_tiles() {
        let mut state = make_state();
        let live = (3_u64 << 32) | 7;
        state
            .output_presentation_surfaces
            .push(OutputPresentationSurface {
                surface_id: live,
                app_id: "dev.qos.slides.audience".into(),
                output: crate::compositor::backend::OutputId(2),
                rect: Rect::new(1280.0, 0.0, 1920.0, 720.0),
            });
        state
            .output_presentation_surfaces
            .push(OutputPresentationSurface {
                surface_id: live + 1,
                app_id: "dev.qos.slides.audience".into(),
                output: crate::compositor::backend::OutputId(3),
                rect: Rect::new(1920.0, 0.0, 2560.0, 720.0),
            });
        assert_eq!(
            state.output_presentation_remote_tiles_for(|id| id == live),
            vec![(live, Rect::new(1280.0, 0.0, 1920.0, 720.0))]
        );
    }

    /// The docked-laptop case: a dense built-in panel beside a sparse external monitor — one session-wide number is wrong for one of them.
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

    #[test]
    fn fullscreen_target_uses_requested_output_dimensions_and_rehomes_on_hotplug() {
        let mut state = make_state();
        state.set_output_layout(&[
            out(10, (0, 0), (1920, 1080), 1.0),
            out(20, (1920, 0), (3840, 2160), 1.0),
        ]);
        state.fullscreen_output = Some(crate::compositor::backend::OutputId(20));
        assert_eq!(state.fullscreen_output_size(), Some((3840, 2160)));

        state.set_output_layout(&[out(10, (0, 0), (1920, 1080), 1.0)]);
        assert_eq!(
            state.fullscreen_output,
            Some(crate::compositor::backend::OutputId(10)),
            "unplugging the target must rehome rather than retaining a dead output id"
        );
        assert_eq!(state.fullscreen_output_size(), Some((1920, 1080)));
    }

    /// A window dragged to the external monitor has to re-render at its density — the cache is what makes that observable to the commit hook.
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

    /// A surface no longer placed must not keep a stale scale — it would be handed back unrecomputed on re-placement.
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

    /// A hotplug invalidates every cached scale, since the layout the values were derived from is what moved.
    #[test]
    fn a_new_output_layout_drops_the_cached_scales() {
        let mut state = make_state();
        state.scale = 1.0;
        state.set_output_layout(&[out(0, (0, 0), (1920, 1080), 2.0)]);
        state.update_surface_scales(&[(42, (10.0, 10.0))]);
        assert_eq!(state.surface_scale(42), 2.0);
        // The external monitor is unplugged; the laptop panel is now the only display, at a different scale.
        state.set_output_layout(&[out(0, (0, 0), (1920, 1080), 1.5)]);
        assert_eq!(
            state.surface_scale(42),
            1.0,
            "a stale per-surface scale must not survive the layout it came from"
        );
        state.update_surface_scales(&[(42, (10.0, 10.0))]);
        assert_eq!(state.surface_scale(42), 1.5);
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "w.max(1)/h.max(1) guarantee non-zero divisors"
    )]
    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(w.saturating_mul(h).saturating_mul(4) as usize);
        for y in 0..h {
            for x in 0..w {
                // `x < w` and `y < h` always hold, so each channel is in 0..=255 and the `u8` conversion never hits its fallback.
                v.push(u8::try_from(x.saturating_mul(255) / w.max(1)).unwrap_or(u8::MAX));
                v.push(u8::try_from(y.saturating_mul(255) / h.max(1)).unwrap_or(u8::MAX));
                v.push(0x80);
                v.push(0xFF);
            }
        }
        v
    }

    /// Two commits with a share session registered must produce two encoded frames — `encode_next_frame` pumps once per commit into the per-subscriber queue.
    #[test]
    fn two_commits_produce_two_encoded_frames() {
        const W: u32 = 64;
        const H: u32 = 64;
        const HOST: PeerId = 0xAAAA;
        const VIEWER: PeerId = 0xBBBB;
        const SURFACE: SurfaceId = 42;

        let session = Arc::new(
            SharedFlatpakSession::start(HOST, "app", SURFACE, W, H, &cfg()).expect("session"),
        );
        session.add_viewer(VIEWER, false);

        let mut state = make_state();
        state.register_share_session(session.clone());

        // Simulate the sequence a real commit would produce: `on_commit`
        // deposits bytes (mocked via `deposit_for_test`), then the commit
        // handler calls `tick_share_session_for_surface` directly — no real `WlSurface` needed.
        state
            .surface_tracker
            .deposit_for_test(SURFACE, gradient(W, H), W, H);
        let ran = state.tick_share_session_for_surface(SURFACE);
        assert!(ran, "first commit must fire the tick");
        // Nudge the throttle clock by re-depositing and pumping again — the
        // throttle is time-based and two back-to-back ticks land inside the
        // 33ms window in tests. Sleep the interval; `should_encode` reads `Instant::now()` and accepts the second tick.
        std::thread::sleep(std::time::Duration::from_millis(40));
        state
            .surface_tracker
            .deposit_for_test(SURFACE, gradient(W, H), W, H);
        let ran2 = state.tick_share_session_for_surface(SURFACE);
        assert!(ran2, "second commit must fire the tick");

        // Both encoded frames land in the subscriber's substream queue with monotonic sequence numbers.
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

    /// A commit for a surface with no registered share session is a no-op (no encoder pumped, no substream traffic).
    #[test]
    fn commit_without_active_session_is_noop() {
        let mut state = make_state();
        state
            .surface_tracker
            .deposit_for_test(999, vec![0; 64], 4, 4);
        assert!(!state.tick_share_session_for_surface(999));
    }

    /// A commit with a registered session but no staged frame returns `false` — the damage-only path (cursor motion, null-attach).
    #[test]
    fn commit_without_staged_frame_returns_false() {
        const HOST: PeerId = 0xAAAA;
        const SURFACE: SurfaceId = 55;

        let session = Arc::new(
            SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
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
            SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
        );
        let mut state = make_state();
        state.register_share_session(session);
        state
            .surface_tracker
            .deposit_for_test(SURFACE, vec![0xFFu8; 32 * 32 * 4], 32, 32);
        assert!(state.tick_share_session_for_surface(SURFACE));
        state.unregister_share_session(SURFACE);
        // Post-unregister the tick is a no-op even with a staged frame.
        state
            .surface_tracker
            .deposit_for_test(SURFACE, vec![0xFFu8; 32 * 32 * 4], 32, 32);
        assert!(!state.tick_share_session_for_surface(SURFACE));
    }

    /// The `ext_session_lock_v1` bridge reports each lock/unlock request as a one-shot edge: `true` once, then `false`, so the loop can't re-fire it.
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

    /// A freshly constructed state advertises the session-lock global and starts with no client holding a lock.
    #[test]
    fn fresh_state_advertises_session_lock_and_starts_unlocked() {
        let mut state = make_state();
        // Constructing the state ran `SessionLockManagerState::new` (global advertised) without panic; no client holds a lock yet.
        assert!(!state.session_lock.take_lock_requested());
        assert!(!state.session_lock.take_unlock_requested());
        assert!(!state.session_lock.has_pending_locker());
    }
}
