//! Smithay `State` struct: the single value threaded through the calloop
//! event loop, following smithay's "one big state" convention — every
//! protocol global attaches via the matching `delegate_*!` macro, expanding
//! into `Dispatch`/`GlobalDispatch` impls routed to the `*Handler` traits.
//! References: smithay 0.7 `examples/minimal.rs`, `examples/compositor.rs`.
//!
//! [`State`] lives for the compositor's lifetime; [`ClientState`] is
//! per-client data attached at `insert_client` time. Trait impls and `delegate_*!` macros follow.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy)]
enum ForeignCommitCapture {
    NoBuffer,
    Failed,
    Removed,
    Shm(u64),
    Dmabuf(u64),
}

#[derive(Default)]
struct ForeignCommitCaptures(Mutex<VecDeque<ForeignCommitCapture>>);

/// Set once the surface queued an SHM capture, which may have left a base in
/// the frame store for its next commit's damage. A commit the capture does
/// not see clears that base only while this is set, so the many surfaces
/// never captured do not take the frame store's lock on every commit.
#[derive(Default)]
struct ShmCaptureBase(std::sync::atomic::AtomicBool);

/// Record that the surface queued an SHM capture.
fn mark_shm_capture_base(states: &wl_compositor::SurfaceData) {
    states
        .data_map
        .insert_if_missing_threadsafe(ShmCaptureBase::default);
    if let Some(base) = states.data_map.get::<ShmCaptureBase>() {
        base.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A commit the capture never sees still changes the surface: a later SHM
/// commit's damage is relative to it, not to the last one captured.
fn forget_uncaptured_shm_base(surface: &WlSurface, surface_id: u64) {
    let had_base = wl_compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<ShmCaptureBase>()
            .is_some_and(|base| base.0.swap(false, std::sync::atomic::Ordering::Relaxed))
    });
    if had_base {
        crate::compositor::foreign::store().reset_shm_base(surface_id);
    }
}

/// After any captured commit but an SHM buffer (or a commit that changes
/// nothing), the next SHM commit's damage is not relative to the frame the
/// SHM capture built: it copies whole.
fn settle_shm_base(surface_id: u64, association: &ForeignCommitCapture) {
    if !matches!(
        association,
        ForeignCommitCapture::Shm(_) | ForeignCommitCapture::NoBuffer
    ) {
        crate::compositor::foreign::store().reset_shm_base(surface_id);
    }
}

/// Buffer damage as the frame store records it.
fn buffer_damage(rects: &[BufferRect]) -> Vec<vello::kurbo::Rect> {
    rects
        .iter()
        .map(|rect| {
            vello::kurbo::Rect::from_origin_size(
                (f64::from(rect.loc.x), f64::from(rect.loc.y)),
                (f64::from(rect.size.w), f64::from(rect.size.h)),
            )
        })
        .collect()
}

/// Buffer damage as the whole pixels an SHM capture copies, clipped to the
/// buffer's non-negative quadrant.
fn shm_regions(rects: &[BufferRect]) -> Vec<crate::compositor::surface_readback::ShmRegion> {
    rects
        .iter()
        .filter_map(|rect| {
            let x = rect.loc.x.max(0);
            let y = rect.loc.y.max(0);
            let right = rect.loc.x.saturating_add(rect.size.w);
            let bottom = rect.loc.y.saturating_add(rect.size.h);
            Some(crate::compositor::surface_readback::ShmRegion {
                x: usize::try_from(x).ok()?,
                y: usize::try_from(y).ok()?,
                width: usize::try_from(right.saturating_sub(x)).ok()?,
                height: usize::try_from(bottom.saturating_sub(y)).ok()?,
            })
        })
        .collect()
}

type BufferRect = smithay::utils::Rectangle<i32, smithay::utils::Buffer>;

/// The commit's damage in buffer pixels. Buffer-coordinate rectangles
/// (`wl_surface.damage_buffer`) are taken as they are. Surface-coordinate
/// ones (`wl_surface.damage`) cover `buffer_scale` buffer pixels per
/// surface pixel while no viewport (`viewported`) crops or scales the
/// buffer; with one, or with a transformed or offset buffer, or no damage,
/// the whole buffer is damaged rather than guessing the conversion.
fn pending_buffer_rects(
    attrs: &wl_compositor::SurfaceAttributes,
    viewported: bool,
) -> Option<Vec<BufferRect>> {
    if attrs.buffer_transform
        != smithay::reexports::wayland_server::protocol::wl_output::Transform::Normal
        || attrs
            .buffer_delta
            .is_some_and(|delta| delta.x != 0 || delta.y != 0)
        || attrs.damage.is_empty()
        || attrs.damage.len() > 128
    {
        return None;
    }
    attrs
        .damage
        .iter()
        .map(|damage| {
            let rect = match damage {
                wl_compositor::Damage::Buffer(rect) => *rect,
                wl_compositor::Damage::Surface(rect) if !viewported => {
                    surface_rect_in_buffer(*rect, attrs.buffer_scale)?
                }
                wl_compositor::Damage::Surface(_) => return None,
            };
            (rect.size.w > 0 && rect.size.h > 0).then_some(rect)
        })
        .collect()
}

/// Whether the commit's viewport crops or scales the buffer: surface-local
/// damage then has no plain conversion to buffer pixels.
fn pending_viewport(states: &wl_compositor::SurfaceData) -> bool {
    let mut viewport = states
        .cached_state
        .get::<smithay::wayland::viewporter::ViewportCachedState>();
    let pending = viewport.pending();
    let set = pending.src.is_some() || pending.dst.is_some();
    drop(viewport);
    set
}

/// A surface-local rectangle in the pixels of an untransformed buffer drawn
/// at `scale` buffer pixels per surface pixel. Clients damage "everything"
/// as `i32::MAX`-sized rectangles, so the scaling saturates.
fn surface_rect_in_buffer(
    rect: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    scale: i32,
) -> Option<BufferRect> {
    if scale < 1 {
        return None;
    }
    let scaled = |value: i32| value.saturating_mul(scale);
    Some(BufferRect::new(
        (scaled(rect.loc.x), scaled(rect.loc.y)).into(),
        (scaled(rect.size.w), scaled(rect.size.h)).into(),
    ))
}

#[cfg(test)]
mod commit_damage_tests {
    use super::{BufferRect, pending_buffer_rects, wl_compositor};
    use smithay::utils::{Logical, Rectangle};

    fn commit(damage: Vec<wl_compositor::Damage>, scale: i32) -> wl_compositor::SurfaceAttributes {
        let mut attrs = wl_compositor::SurfaceAttributes::default();
        attrs.damage = damage;
        attrs.buffer_scale = scale;
        attrs
    }

    fn surface(x: i32, y: i32, w: i32, h: i32) -> wl_compositor::Damage {
        wl_compositor::Damage::Surface(Rectangle::<i32, Logical>::new((x, y).into(), (w, h).into()))
    }

    fn buffer(x: i32, y: i32, w: i32, h: i32) -> BufferRect {
        BufferRect::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn surface_damage_covers_its_buffer_pixels_at_the_buffer_scale() {
        let attrs = commit(
            vec![
                surface(10, 20, 30, 4),
                wl_compositor::Damage::Buffer(buffer(1, 2, 3, 4)),
            ],
            2,
        );
        assert_eq!(
            pending_buffer_rects(&attrs, false),
            Some(vec![buffer(20, 40, 60, 8), buffer(1, 2, 3, 4)])
        );
    }

    #[test]
    fn surface_damage_under_a_viewport_damages_the_whole_buffer() {
        let attrs = commit(vec![surface(10, 20, 30, 4)], 1);
        assert_eq!(pending_buffer_rects(&attrs, true), None);
        // Buffer damage needs no conversion, viewport or not.
        let attrs = commit(vec![wl_compositor::Damage::Buffer(buffer(1, 2, 3, 4))], 1);
        assert_eq!(
            pending_buffer_rects(&attrs, true),
            Some(vec![buffer(1, 2, 3, 4)])
        );
    }

    #[test]
    fn damaging_everything_in_surface_pixels_saturates() {
        let attrs = commit(vec![surface(0, 0, i32::MAX, i32::MAX)], 2);
        assert_eq!(
            pending_buffer_rects(&attrs, false),
            Some(vec![buffer(0, 0, i32::MAX, i32::MAX)])
        );
    }
}

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Format, Modifier};
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, find_popup_root_surface,
};
use smithay::input::keyboard::{Layout, XkbConfig};
use smithay::input::pointer::{AxisFrame, ButtonEvent, Focus, MotionEvent, RelativeMotionEvent};

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
use smithay::wayland::content_type::ContentTypeState;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::tablet_manager::TabletManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::wayland::input_method::{
    InputMethodHandler, InputMethodManagerState, PopupSurface as InputMethodPopupSurface,
};
use smithay::wayland::text_input::{TextInputManagerState, TextInputSeat};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::dialog::{XdgDialogHandler, XdgDialogState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ShellClient, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::{
    delegate_compositor, delegate_content_type, delegate_cursor_shape, delegate_data_control,
    delegate_data_device, delegate_dmabuf, delegate_ext_data_control, delegate_fractional_scale,
    delegate_idle_inhibit, delegate_keyboard_shortcuts_inhibit,
    delegate_output, delegate_pointer_constraints, delegate_pointer_gestures,
    delegate_primary_selection, delegate_relative_pointer,
    delegate_session_lock, delegate_shm,
    delegate_viewporter, delegate_xdg_decoration,
    delegate_xdg_activation, delegate_xdg_dialog, delegate_xdg_shell,
};

use crate::collab::flatpak_share::SharedFlatpakSession;
use crate::compositor::output_management::OutputManagementState;
use crate::compositor::remote_seat::RemoteSeats;
use crate::compositor::render_node::{ProbeError, RenderNode};
use crate::compositor::selection::SelectionState;
use crate::compositor::surface_readback::SurfaceTracker;
use crate::compositor::tab_binding;
use crate::network::input_firewall::{Denial, RemoteInputFirewall};
use crate::network::protocol::{PeerId as MsgPeerId, SurfaceId};
use crate::network::streaming::{InputEvent, RemoteAction, RemoteInputSink, ShareRegistry};
use crate::wayland::color_management::ColorManagerState;

use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
use smithay::backend::input::KeyState;
use std::collections::{BTreeSet, HashMap, HashSet};
use vello::kurbo::Rect;

/// Stable identity from `apps/osk/tilo-app.toml`. It is necessary but never
/// sufficient for promotion: the connection must also be accepted on the
/// compositor's private OSK socket.
pub use crate::shell::floating::OSK_APP_ID;

/// A deliberately narrow app-id convention for the clean, audience-facing
/// side of a presentation.  It is a normal xdg toplevel, but Tilo owns its
/// placement so it cannot collapse the presenter's workspace group.
pub const OUTPUT_PRESENTATION_APP_ID_SUFFIX: &str = ".audience";

fn is_output_presentation_app_id(app_id: &str) -> bool {
    app_id
        .strip_suffix(OUTPUT_PRESENTATION_APP_ID_SUFFIX)
        .is_some_and(|base| !base.is_empty())
}

/// `.audience` is not a privilege a random client can claim. The matching
/// presenter must already be a live, ordinary toplevel from the *same*
/// Wayland connection, with the exact base app-id. Returns that presenter.
fn output_presentation_owner(
    tree: &WorkspaceTree,
    audience_surface: SurfaceId,
    audience_app_id: &str,
) -> Option<SurfaceId> {
    let base = audience_app_id.strip_suffix(OUTPUT_PRESENTATION_APP_ID_SUFFIX)?;
    let client = audience_surface >> 32;
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            crate::shell::state::TileContent::Foreign { surface_id, app_id }
                if *surface_id != audience_surface
                    && (*surface_id >> 32) == client
                    && app_id == base =>
            {
                Some(*surface_id)
            }
            _ => None,
        })
}

mod presenter_screen;

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

pub(crate) use crate::shell::floating::*;

/// The persisted `HiDPI` display scale in a settings document.
///
/// The Settings app writes a `"display"` object carrying a `"scale"` number
/// (1.0=100%, …); the compositor follows the same document through its live
/// settings snapshot. An absent or out-of-range value falls back to `1.0`.
pub fn display_scale_from(document: &serde_json::Map<String, serde_json::Value>) -> f64 {
    match document
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
/// so `locked` fires only once content is hidden. Releasing the protocol
/// object never unlocks Tilo: the compositor-owned password verifier is the
/// sole authority that can clear the session lock. Same-thread, so no
/// `Arc`/lock.
#[derive(Default)]
pub struct SessionLockHooks {
    lock_requested: bool,
    pending_locker: Option<SessionLocker>,
}

impl SessionLockHooks {
    /// Take the "a client asked to lock" edge (clears it; true at most once per protocol `lock` request).
    pub fn take_lock_requested(&mut self) -> bool {
        std::mem::take(&mut self.lock_requested)
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

/// One output's `(position, size, scale)` in the logical desktop — see
/// [`State::output_layout`]. Factored into a named
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

/// Everything the capture protection of transient surfaces is derived
/// from, as of the last [`State::sync_capture_protection`] pass. Compared
/// field by field on each wakeup without allocating, taking the tree lock
/// or the store's protection lock; any difference re-derives, so a newly
/// mapped popup, a reparented one, a secret field gaining focus, an app
/// asking for protection or a renamed tab is never missed.
#[derive(Default, PartialEq)]
pub(crate) struct CaptureProtectionInputs {
    /// The focused text field declares its content secret.
    secret_field: bool,
    osk: Option<SurfaceId>,
    /// The workspace tree's write generation and the store's protection
    /// epoch: which app each tiled surface belongs to, and which apps and
    /// surfaces are protected.
    versions: (u64, u64),
    input_method_popups: Vec<(SurfaceId, Option<SurfaceId>)>,
    popups: Vec<(SurfaceId, bool, SurfaceId)>,
    /// Floating toplevels and their apps.
    floating: Vec<(SurfaceId, String)>,
}

impl CaptureProtectionInputs {
    fn capture(state: &State, secret_field: bool, versions: (u64, u64)) -> Self {
        Self {
            secret_field,
            osk: state.osk_surface,
            versions,
            input_method_popups: state.capture_protection_input_method_popups().collect(),
            popups: state.capture_protection_popups().collect(),
            floating: state
                .floating_toplevels
                .iter()
                .map(|toplevel| (toplevel.surface_id, toplevel.app_id.clone()))
                .collect(),
        }
    }

    fn matches(&self, state: &State, secret_field: bool, versions: (u64, u64)) -> bool {
        self.secret_field == secret_field
            && self.osk == state.osk_surface
            && self.versions == versions
            && self
                .input_method_popups
                .iter()
                .copied()
                .eq(state.capture_protection_input_method_popups())
            && self
                .popups
                .iter()
                .copied()
                .eq(state.capture_protection_popups())
            && self
                .floating
                .iter()
                .map(|(surface, app_id)| (*surface, app_id.as_str()))
                .eq(state
                    .floating_toplevels
                    .iter()
                    .map(|toplevel| (toplevel.surface_id, toplevel.app_id.as_str())))
    }
}

/// [`State::sync_capture_protection`]'s memory between wakeups.
#[derive(Default)]
pub(crate) struct CaptureProtectionSync {
    last: Option<CaptureProtectionInputs>,
    #[cfg(test)]
    passes: usize,
}

#[derive(Clone)]
pub(crate) struct FullscreenTarget {
    pub surface_id: u64,
    pub requested_output: Option<WlOutput>,
    pub surface: ToplevelSurface,
    pub configured_size: Option<(i32, i32)>,
    /// Set when the compositor, not the client, put this surface on one
    /// output: the presenter view beside an audience surface. It outranks
    /// `requested_output`, and the client is sized to the output without
    /// being told it is fullscreen, so its own fullscreen intent stays its own.
    pub pinned_output: Option<crate::compositor::backend::OutputId>,
}

#[derive(Debug, Clone)]
struct HostLaunchActivation {
    desktop_id: String,
    display_name: String,
    placement: crate::compositor::tab_binding::Placement,
    issued: std::time::Instant,
}

/// The compositor's process-wide state.
///
/// The single value the calloop event loop hands to every callback, so every Wayland global gets unsynchronised mutable access without `Rc`/`Arc` plumbing.
#[derive(Debug, Default)]
struct PersonalFabricInputHolders {
    keys: HashMap<crate::shell::personal_continuity::DeviceId, HashSet<u32>>,
    key_counts: HashMap<u32, usize>,
    buttons: HashMap<crate::shell::personal_continuity::DeviceId, HashSet<u32>>,
    button_counts: HashMap<u32, usize>,
}

impl PersonalFabricInputHolders {
    fn transition(
        holders: &mut HashMap<crate::shell::personal_continuity::DeviceId, HashSet<u32>>,
        counts: &mut HashMap<u32, usize>,
        controller: &crate::shell::personal_continuity::DeviceId,
        code: u32,
        pressed: bool,
    ) -> bool {
        if pressed {
            let held = holders.entry(controller.clone()).or_default();
            if !held.insert(code) {
                return false;
            }
            let count = counts.entry(code).or_default();
            *count = count.saturating_add(1);
            *count == 1
        } else {
            let empty = {
                let Some(held) = holders.get_mut(controller) else {
                    return false;
                };
                if !held.remove(&code) {
                    return false;
                }
                held.is_empty()
            };
            if empty {
                holders.remove(controller);
            }
            let Some(count) = counts.get_mut(&code) else {
                return false;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&code);
                true
            } else {
                false
            }
        }
    }

    fn key_transition(
        &mut self,
        controller: &crate::shell::personal_continuity::DeviceId,
        keycode: u32,
        pressed: bool,
    ) -> bool {
        Self::transition(
            &mut self.keys,
            &mut self.key_counts,
            controller,
            keycode,
            pressed,
        )
    }

    fn button_transition(
        &mut self,
        controller: &crate::shell::personal_continuity::DeviceId,
        button: u32,
        pressed: bool,
    ) -> bool {
        Self::transition(
            &mut self.buttons,
            &mut self.button_counts,
            controller,
            button,
            pressed,
        )
    }

    fn release_controller(
        &mut self,
        controller: &crate::shell::personal_continuity::DeviceId,
    ) -> (Vec<u32>, Vec<u32>) {
        let mut keys = self
            .keys
            .get(controller)
            .map(|held| held.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut buttons = self
            .buttons
            .get(controller)
            .map(|held| held.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        keys.sort_unstable();
        buttons.sort_unstable();
        let keys = keys
            .into_iter()
            .filter(|key| self.key_transition(controller, *key, false))
            .collect();
        let buttons = buttons
            .into_iter()
            .filter(|button| self.button_transition(controller, *button, false))
            .collect();
        self.keys.remove(controller);
        self.buttons.remove(controller);
        (keys, buttons)
    }

    fn controllers(&self) -> Vec<crate::shell::personal_continuity::DeviceId> {
        let mut controllers = self
            .keys
            .keys()
            .chain(self.buttons.keys())
            .cloned()
            .collect::<Vec<_>>();
        controllers.sort();
        controllers.dedup();
        controllers
    }
}

#[cfg(test)]
mod personal_fabric_input_holder_tests {
    use super::PersonalFabricInputHolders;
    use crate::shell::personal_continuity::DeviceId;

    #[test]
    fn cleanup_releases_only_the_final_controller_holding_an_input() {
        let first = DeviceId::new("first").unwrap();
        let second = DeviceId::new("second").unwrap();
        let mut held = PersonalFabricInputHolders::default();
        assert!(held.key_transition(&first, 30, true));
        assert!(held.key_transition(&first, 42, true));
        assert!(!held.key_transition(&second, 42, true));
        assert!(held.button_transition(&first, 0x110, true));
        assert!(held.button_transition(&second, 0x111, true));
        assert_eq!(held.release_controller(&first), (vec![30], vec![0x110]));
        assert_eq!(held.release_controller(&second), (vec![42], vec![0x111]));
    }
}

/// How long a toplevel asked to close keeps unthrottled frame callbacks: long
/// enough to present its last frames and exit, short enough that a client
/// ignoring the close cannot draw unseen at full speed for the rest of the
/// session.
pub(crate) const CLOSING_FRAME_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

pub struct State {
    /// Calloop loop handle — for registering input devices, session signals, libinput sources, etc.
    pub loop_handle: LoopHandle<'static, Self>,
    /// `wayland-server` display handle. Cloneable; used to insert new clients when the listening socket accepts a connection.
    pub display_handle: DisplayHandle,
    /// `wl_compositor` + `wl_subcompositor` global state.
    pub compositor_state: CompositorState,
    pub presentation_state: smithay::wayland::presentation::PresentationState,
    /// Original timestamp of the currently dispatched trusted physical key.
    pub physical_key_input_time_ns: Option<u64>,
    pub presentation_feedback: crate::compositor::presentation::PresentationFeedback,

    /// `xdg_wm_base` global state — toplevels and popups.
    pub xdg_shell_state: XdgShellState,
    /// `xdg_activation_v1` token store. Client-minted tokens enter it only
    /// after [`XdgActivationHandler::token_created`] proves they came from a
    /// recent serial on this seat and a focused source surface.
    pub xdg_activation_state: XdgActivationState,
    /// `xdg_wm_dialog_v1` — the modal hint on transient toplevels, which
    /// decides whether a sheet blocks its parent.
    pub xdg_dialog_state: XdgDialogState,
    /// Metadata for compositor-created activation tokens passed to one host
    /// launch. Kept separate from client token data: host app-ids need not
    /// match their desktop-file id.
    host_launch_activations: HashMap<String, HostLaunchActivation>,
    /// Canonical labels claimed by a compositor-issued host-launch token.
    /// A late client `set_app_id` may update routing identity but must not
    /// replace this user-selected host application name.
    host_launch_surface_labels: HashMap<u64, String>,
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
    /// Legacy windows remote viewers closed, for the compositor to close.
    remote_closes: Vec<SurfaceId>,
    /// `wl_output` + xdg-output manager; concrete outputs are added separately.
    pub output_manager_state: OutputManagerState,
    /// Persisted workspace tree; foreign-client toplevels adopt into it as [`crate::shell::state::TileContent::Foreign`] tabs.
    pub workspace_tree: WorkspaceTreeHandle,
    /// Peer-sync share registry. Tracks which peers have been granted
    /// streaming + input access to which surfaces, and the per-peer
    /// `read_only` flag. Inbound [`crate::network::NeighbourMsg::RemoteInput`] events are filtered through this before reaching the seat.
    pub share_registry: ShareRegistry,
    /// The rest of the remote-input firewall: region, z-order, keyboard
    /// scrubber, rate limit, physical-input priority and the kill switch.
    /// The share registry decides *whether* a peer may drive a surface;
    /// this decides what, where and how fast. Fed a view of the screen once per tick — until then it refuses everything.
    pub remote_input_firewall: RemoteInputFirewall,
    /// Approved `GlobalShortcuts` grants. Client routing asks it before a
    /// focused client sees a key, so a granted chord fires over any app.
    pub global_shortcuts: crate::portal::global_shortcuts::ShellGlobalShortcutsService,
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
    /// Recently issued seat serials and their issue time. The activation
    /// protocol carries a serial but no timestamp for the underlying input;
    /// retaining this short bounded history lets us reject fabricated and
    /// stale serials instead of treating the opaque integer as authority.
    pub activation_serials: VecDeque<(Serial, u64, std::time::Instant)>,
    /// Wayland clients (by connection index) the person has sent input to.
    /// A window of one of these may have been closed from inside the app;
    /// a window of any other that goes away unasked was lost.
    pub(crate) clients_given_input: std::collections::HashSet<u32>,
    /// Wall-clock anchor for input timestamps; pointer events ship `time: u32` in milliseconds since this instant.
    pub start_time: std::time::Instant,
    /// Per-peer virtual-seat attribution. Click focus scopes which shared
    /// surface may receive that peer's keys, while pointer leave preserves
    /// the caret and the active tab retains the one app-facing
    /// keyboard/text-input focus. See
    /// [`crate::compositor::remote_seat::RemoteSeats`].
    pub remote_seats: RemoteSeats,
    personal_fabric_input: PersonalFabricInputHolders,
    /// Every live client surface, keyed by the stable collaboration id.
    /// Native output bookkeeping uses this to emit `wl_surface.enter`, and
    /// remote pointer motion uses it to resolve the focused surface.
    pub surface_handles: HashMap<SurfaceId, WlSurface>,
    /// Active share sessions keyed by their target `SurfaceId`.
    ///
    /// [`CompositorHandler::commit`] consults this map on every surface
    /// commit — when the surface is in the map, `encode_next_frame` is
    /// pumped the same tick a fresh buffer lands, avoiding a per-frame
    /// stall waiting on the next damage pass. Held as `Arc` since
    /// [`crate::collab::flatpak_share::SharedFlatpakSession`]'s `encode_next_frame` takes `&self`.
    pub active_sessions: HashMap<SurfaceId, Arc<SharedFlatpakSession>>,
    /// Apps that can stream their window's page as a display list, by app
    /// id (`shell-ui`'s `PaintCapable`). Their windows reach web members as
    /// paint streams instead of video.
    pub paint_apps: HashMap<String, PaintApp>,
    /// Non-blocking bridge from surface commits into the collaboration
    /// transport, given the topic of the room the window's tab is in when
    /// known. Installed by the native compositor when peer sync is on;
    /// absent in isolated tests and offline sessions.
    legacy_stream_publisher: Option<LegacyStreamPublisher>,
    /// The workspace topic each streamed window's traffic goes to: its tab's
    /// room, which need not be the workspace this device shows.
    legacy_stream_routes: HashMap<SurfaceId, u64>,
    /// Windows streamed to the person's own other devices, by viewer: those
    /// devices are the person, so their focus moves this seat's keyboard.
    personal_stream_viewers: std::collections::HashSet<(MsgPeerId, SurfaceId)>,
    /// A streamed window the person focused from another device. It holds
    /// this seat's keyboard until they use this device's own input again.
    personal_keyboard_focus: Option<SurfaceId>,
    /// Workspace hosts controlling this headless session's streamed windows.
    headless_host_viewers: std::collections::HashSet<(MsgPeerId, SurfaceId)>,
    headless_keyboard_focus: Option<(MsgPeerId, SurfaceId)>,
    /// Another device of the person last moved this seat's pointer, to a
    /// window it shows. That moves no cursor here: the host's cursor stays
    /// where its own input left it and keeps its own image, and its next
    /// input takes the pointer back.
    personal_pointer_hold: bool,
    /// The themed placeholder colour a protected window is streamed as —
    /// see [`crate::shell::capture_protection`]. Kept current by the frame
    /// loop, which owns the theme.
    capture_placeholder_fill: [u8; 4],
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
    /// `zwp_idle_inhibit_manager_v1` — how a video player keeps the screen on.
    pub idle_inhibit_manager: IdleInhibitManagerState,
    /// Live idle inhibitors and the registry holds their visible windows earn; see [`crate::compositor::idle_inhibit`].
    pub idle_inhibitors: crate::compositor::idle_inhibit::IdleInhibitors,
    /// `xdg_toplevel_icon_manager_v1` — a window naming or drawing its own
    /// icon for its tab; see [`crate::compositor::toplevel_icon`].
    pub toplevel_icon_manager: crate::compositor::toplevel_icon::ToplevelIconManager,
    /// Current display scale factor (`HiDPI`), sent to clients as the
    /// preferred `wp_fractional_scale`. Follows the shared settings
    /// (`display.scale`, see [`display_scale_from`]), so it tracks the Settings app's "Display scale" control. Defaults to `1.0` when unset or absent.
    pub scale: f64,
    /// Every output as `(position, size, scale)` in the logical desktop,
    /// mirrored from the backend.
    ///
    /// [`Self::scale`] is one number for the whole session, only right on a
    /// single-display machine — a docked laptop has a dense panel and a sparse one, and the wrong factor renders soft. This is what lets the preferred `wp_fractional_scale` be per-surface.
    pub output_layout: Vec<OutputPlacement>,
    /// Each output's logical size — its mode over its scale, rotated — keyed
    /// by the stable backend output identity: what a fullscreen configure
    /// asks a client for, in the same units as a tiled configure.
    output_sizes: HashMap<crate::compositor::backend::OutputId, (i32, i32)>,
    output_rects: HashMap<crate::compositor::backend::OutputId, Rect>,
    /// The backend's output table as last mirrored, identities included —
    /// what the presentation placement ranks screens by.
    output_table: Vec<crate::compositor::backend::Output>,
    /// The presenter view moved onto its own screen while an audience
    /// surface holds another; see [`presenter_screen`].
    presenter_pin: Option<presenter_screen::PresenterPin>,
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
    pub(crate) fullscreen_target: Option<FullscreenTarget>,
    /// The XKB sources the seat keymap was compiled from, so a settings
    /// reload that only moves the active source switches groups instead of
    /// recompiling and re-sending the keymap to every client.
    applied_keyboard: crate::compositor::backend::drm::input_config::KeyboardSettings,
    /// Whether clients' own key repeat is off because a remote participant
    /// types: a release crossing the network late must not let a client
    /// repeat the key meanwhile. The sender repeats a held key instead, and
    /// local keyboard input turns client repeat back on.
    remote_typing: bool,
    /// Combined output size in physical pixels, mirrored from the backend
    /// (`(0, 0)` until the first output is known). Used as the size in a
    /// fullscreen `xdg_toplevel` configure; `(0, 0)` leaves the size unspecified so the client keeps its own.
    pub output_size: (i32, i32),
    /// `ext_session_lock_v1` global state (tracks which outputs a client lock covered), letting `loginctl lock-session` drive the lock via protocol.
    pub session_lock_state: SessionLockManagerState,
    /// Bridge from the protocol handler to the native run loop's authoritative lock (see [`SessionLockHooks`]).
    pub session_lock: SessionLockHooks,
    /// `zwlr_output_manager_v1` + `zwlr_output_power_manager_v1`: display
    /// arrangement and panel power for external tools. Privileged clients
    /// only, like session-lock, since either can black out every screen.
    pub output_management: OutputManagementState,
    /// `zwp_pointer_gestures_v1` global state — advertises touchpad
    /// pinch/swipe/hold gestures to clients. On a bare-TTY (DRM) session
    /// Mutter isn't around to expose gestures, so the compositor does; winit
    /// clients bind it per pointer. Delivering to the focused surface is
    /// gated on routing local libinput input through the seat (today local input drives shell chrome, not client surfaces).
    pub pointer_gestures_state: PointerGesturesState,
    /// `wp_cursor_shape_manager_v1`: clients name a cursor and the
    /// compositor draws it, instead of each shipping its own theme.
    pub cursor_shape_state: CursorShapeManagerState,
    /// The cursor the pointer-focused client asked for.
    pub client_cursor: crate::compositor::client_cursor::ClientCursor,
    /// `zwp_relative_pointer_manager_v1`: unclamped motion deltas for games.
    pub relative_pointer_state: RelativePointerManagerState,
    /// `zwp_pointer_constraints_v1`: lock or confine the pointer, granted
    /// only as [`crate::compositor::pointer_lock`] allows.
    pub pointer_constraints_state: PointerConstraintsState,
    pub pointer_lock: crate::compositor::pointer_lock::PointerLock,
    /// `zwp_tablet_manager_v2`: pens with pressure and tilt.
    pub tablet_manager_state: TabletManagerState,
    pub tablet_clients: crate::compositor::tablet::TabletClients,
    /// `zwp_keyboard_shortcuts_inhibit_manager_v1`, granted as
    /// [`crate::compositor::shortcut_inhibit`] allows.
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub shortcut_inhibit: crate::compositor::shortcut_inhibit::ShortcutInhibit,
    /// `wp_content_type_manager_v1`: a surface's hint that it shows video or
    /// a game.
    pub content_type_state: ContentTypeState,
    /// Local-input → client-surface routing state: the live foreign-surface
    /// registry, seat focus bookkeeping, and the queue of foreign toplevels
    /// waiting to be mirrored into the shell chrome's tab tree. Every method lives in [`crate::compositor::client_input`].
    pub client_input: crate::compositor::client_input::ClientInput,
    /// XDG popups are separate surfaces, not tabs.  They are captured and
    /// composed above their parent through this registry.
    pub(crate) popups: Vec<TrackedPopup>,
    /// Answers to `xdg_wm_base.ping`, per client: how a screenshot after
    /// input learns the clients in it finished answering the input.
    pub shell_pongs: crate::compositor::client_settle::Pongs,
    pub(crate) popup_manager: PopupManager,
    /// Input-method candidate surfaces are similarly transient.  They stay
    /// outside the app tree so a candidate list can never become a tab.
    pub(crate) input_method_popups: Vec<InputMethodPopupSurface>,
    /// Which trusted input method currently owns the seat; read by the
    /// input-method and virtual-keyboard global filters.
    seat_input_method: crate::compositor::input_method::SeatInputMethod,
    /// Live connections accepted on a trusted input method's private
    /// display, so the previous owner's can be cut when the seat moves.
    trusted_input_method_clients: Vec<(
        ClientId,
        crate::compositor::input_method::TrustedInputMethod,
    )>,
    /// Every text input's declared content type, for telling a secret
    /// field from an ordinary one.
    pub(crate) text_fields: crate::compositor::text_commit::TextFields,
    pub(crate) private_text_input: crate::compositor::private_text_input::PrivateTextInput,
    /// Text the shell is committing into the focused field.
    pub(crate) text_commit: crate::compositor::text_commit::TextCommit,
    /// The trusted OSK's xdg-toplevel surface, promoted out of the workspace
    /// tree and positioned by [`crate::compositor::frame::FrameBuilder`].
    pub osk_surface: Option<u64>,
    /// Text commits the on-screen keyboard has made into focused fields this
    /// session: what a person typed on it, counted but never kept.
    pub osk_commits: u64,
    /// Whether one of the shell's own text fields holds the seat's
    /// text-input focus (see `compositor::shell_text_input`).
    pub(crate) shell_field_active: bool,
    /// Editing keys a virtual keyboard sent while a shell field was served,
    /// as chrome events, until the loop delivers them.
    pub(crate) shell_field_keys: Vec<winit::event::WindowEvent>,
    /// Each virtual keyboard's current keymap, to read its keys as keysyms.
    pub(crate) virtual_keymaps: std::collections::HashMap<
        smithay::reexports::wayland_server::backend::ObjectId,
        smithay::input::keyboard::xkb::Keymap,
    >,
    /// Toplevels the compositor has asked to close, each with the moment its
    /// frame callbacks stop going unthrottled. See
    /// [`Self::ask_toplevel_to_close`].
    closing_toplevels: HashMap<u64, std::time::Instant>,
    /// One-time capabilities issued through the authenticated shell-ui
    /// service. A public Wayland client is promoted only when its app-id
    /// carries an exact token from this set.
    pub(crate) pending_floating_toplevels: Vec<PendingFloatingToplevel>,
    /// Live app-owned secondary surfaces, outside the normal workspace tree.
    pub(crate) floating_toplevels: Vec<FloatingToplevel>,
    pub(crate) floating_toplevel_events: Vec<FloatingToplevelEvent>,
    /// What transient surfaces' capture protection was last derived from.
    capture_protection_sync: CaptureProtectionSync,
}

pub(crate) fn foreign_root(surface: &WlSurface) -> Option<WlSurface> {
    use smithay::wayland::compositor::get_parent;

    let mut current = surface.clone();
    let mut found_parent = false;
    for _ in 0..64 {
        let Some(parent) = get_parent(&current) else {
            return found_parent.then_some(current);
        };
        current = parent;
        found_parent = true;
    }
    None
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

    /// Ask `toplevel` to close. Until [`CLOSING_FRAME_GRACE`] has passed its
    /// frame callbacks fire as soon as it commits, whether or not it is
    /// shown: the tile is already gone, and a client whose swapchain waits on
    /// a frame callback before its next present (Mesa's FIFO) would otherwise
    /// block on its last frame, and so never finish closing.
    pub(crate) fn ask_toplevel_to_close(&mut self, toplevel: &ToplevelSurface) {
        let id = surface_id(toplevel.wl_surface());
        eprintln!(
            "window-close-requested: surface={id} launch={}",
            crate::shell::launcher::launch_key_for_surface(id)
                .as_deref()
                .unwrap_or("-")
        );
        let now = std::time::Instant::now();
        let until = now.checked_add(CLOSING_FRAME_GRACE).unwrap_or(now);
        self.closing_toplevels.insert(id, until);
        if let Some(pid) = toplevel
            .wl_surface()
            .client()
            .and_then(|client| client.get_data::<ClientState>().and_then(|state| state.pid))
            && !self
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .any(|other| {
                    other.wl_surface() != toplevel.wl_surface()
                        && !self
                            .closing_toplevels
                            .contains_key(&crate::compositor::state::surface_id(other.wl_surface()))
                        && other.wl_surface().client().and_then(|client| {
                            client.get_data::<ClientState>().and_then(|state| state.pid)
                        }) == Some(pid)
                })
        {
            crate::shell::launcher::note_native_writer_retiring(pid);
        }
        toplevel.send_close();
        self.release_frame_callbacks(toplevel.wl_surface());
    }

    /// Whether `surface_id` was asked to close and its grace has not run out.
    pub(crate) fn is_closing_toplevel(&self, surface_id: u64) -> bool {
        self.closing_toplevels
            .get(&surface_id)
            .is_some_and(|until| std::time::Instant::now() < *until)
    }

    /// A commit from a toplevel asked to close: its frame is done now.
    fn release_closing_frame_callbacks(&self, surface: &WlSurface, surface_id: u64) {
        if self.is_closing_toplevel(surface_id) {
            self.release_frame_callbacks(surface);
        }
    }

    /// Fire every frame callback `surface` has queued. Returns whether any
    /// fired.
    pub(crate) fn release_frame_callbacks(&self, surface: &WlSurface) -> bool {
        let time_ms = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
        let mut fired = false;
        wl_compositor::with_states(surface, |states| {
            let mut attrs = states
                .cached_state
                .get::<wl_compositor::SurfaceAttributes>();
            for callback in attrs.current().frame_callbacks.drain(..) {
                callback.done(time_ms);
                fired = true;
            }
        });
        fired
    }

    /// Ask the client showing `surface_id` to close. Its tab remains available
    /// for a deferred save decision until actual destruction removes it.
    pub fn close_surface_on_request(&mut self, surface_id: u64) {
        // xdg close is a request, not admission. Destruction owns durable
        // removal so a client's save/refusal decision stays visible.
        if let Some(toplevel) = self
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| self::surface_id(toplevel.wl_surface()) == surface_id)
            .cloned()
        {
            self.ask_toplevel_to_close(&toplevel);
        }
        self.close_sheets_over(surface_id);
    }

    pub(crate) fn is_floating_toplevel(&self, surface_id: u64) -> bool {
        self.floating_toplevels
            .iter()
            .any(|surface| surface.surface_id == surface_id)
    }

    /// Whether the focused client has committed an enabled text-input-v3
    /// session. This is the compositor's source of truth for OSK visibility;
    /// Wayland xdg-toplevels have no client-side visibility request.
    /// Re-send `leave` and `enter` to the focused client's text inputs.
    ///
    /// Smithay activates the input method only when a field enables its text
    /// input. The on-screen keyboard binds its input method shortly before
    /// its surface appears; a field that was already enabled by then (the
    /// tap that focused it is often the touch that started the keyboard)
    /// would otherwise never reach it. A client re-enables an IME-allowed
    /// field on `enter`, which activates the keyboard that is now there.
    pub fn reannounce_text_input_focus(&self) {
        let text_input = self.seat.text_input();
        if text_input.focus().is_some() {
            text_input.leave();
            text_input.enter();
        }
    }

    /// Content-free focus metadata read from the live Wayland seat for the
    /// existing state-dump door. Never includes titles, text or held keys.
    pub fn seat_focus_json(&self) -> serde_json::Value {
        let describe = |surface: WlSurface| {
            let id = surface_id(&surface);
            let root = crate::compositor::foreign::store().sheet_root(id);
            let owner = self.client_input.surface(root).unwrap_or(&surface);
            let app_id = owner
                .client()
                .and_then(|client| {
                    client
                        .get_data::<ClientState>()
                        .and_then(|data| data.package_id.clone())
                })
                .or_else(|| {
                    wl_compositor::with_states(owner, |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|data| data.lock().ok()?.app_id.clone())
                    })
                });
            let role = if self.osk_surface == Some(id) {
                "on_screen_keyboard"
            } else if trusted_osk_connection(&surface) {
                "pending_on_screen_keyboard"
            } else if crate::compositor::foreign::store().is_sheet(id) {
                "dialog_sheet"
            } else if self.client_input.is_known(id) {
                "toplevel"
            } else {
                "other"
            };
            serde_json::json!({"surface": id, "root_surface": root, "app_id": app_id, "role": role})
        };
        let keyboard = self.seat.get_keyboard();
        serde_json::json!({
            "keyboard": keyboard.as_ref().and_then(smithay::input::keyboard::KeyboardHandle::current_focus).map(describe),
            "routed_keyboard_surface": self.client_input.keyboard_focus(),
            "text_input": self.seat.text_input().focus().map(describe),
            "text_input_active": self.text_input_active(),
            "on_screen_keyboard_surface": self.osk_surface,
        })
    }

    pub fn text_input_active(&self) -> bool {
        let mut active = false;
        self.seat
            .text_input()
            .with_active_text_input(|_, _| active = true);
        active
    }

    /// Build a fresh `State` with all protocol globals registered — each `*::new::<Self>` call advertises the corresponding Wayland global.
    pub fn new(loop_handle: LoopHandle<'static, Self>, display_handle: DisplayHandle) -> Self {
        Self::new_with_workspace_tree(
            loop_handle,
            display_handle,
            WorkspaceTree::bootstrap_default().shared(),
        )
    }

    /// Build a `State` around the tree restored by the native session loader.
    /// The protocol callbacks and the frame builder must share this handle:
    /// otherwise a restored local tab can remain in the rendered tree after
    /// its Wayland surface is adopted and destroyed.
    #[allow(
        clippy::too_many_lines,
        reason = "one place creates every protocol global the compositor advertises"
    )]
    pub fn new_with_workspace_tree(
        loop_handle: LoopHandle<'static, Self>,
        display_handle: DisplayHandle,
        workspace_tree: WorkspaceTreeHandle,
    ) -> Self {
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let presentation_state = smithay::wayland::presentation::PresentationState::new::<Self>(
            &display_handle,
            u32::try_from(libc::CLOCK_MONOTONIC).unwrap_or(1),
        );
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let xdg_activation_state = XdgActivationState::new::<Self>(&display_handle);
        let xdg_dialog_state = XdgDialogState::new::<Self>(&display_handle);
        // Advertised to every client, unfiltered: hiding this global just makes a toolkit decorate itself.
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let mut seat_state = SeatState::new();
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        let mut dmabuf_state = DmabufState::new();
        let (dmabuf_device, dmabuf_global) = Self::init_dmabuf(&display_handle, &mut dmabuf_state);

        // Register a real seat with a pointer device for `apply_remote_input` to inject into; name matches the wayland.rs sibling for consistency.
        let mut seat = seat_state.new_wl_seat(&display_handle, "tilo-desktop");
        seat.add_pointer();
        let applied_keyboard = Self::init_seat_keyboard(&mut seat);
        let initial_keyboard_index = applied_keyboard.active_index;
        // A touchscreen can't reach a client without this — chrome uses the synthesised pointer stream, but real multi-touch needs `wl_touch`.
        seat.add_touch();
        // Text fields are public; the input method and virtual keyboard are
        // not. Sandboxed apps only see the public Wayland socket. Even a
        // process spoofing `so.tilo.osk` there cannot bind either privileged
        // global: authorization is immutable per-connection state set by the
        // dedicated socket acceptor, and only the kind that currently owns
        // the seat (see `compositor::input_method`) sees them at all.
        let text_input_manager_state = TextInputManagerState::new::<Self>(&display_handle);
        crate::compositor::private_text_input::PrivateTextInput::register(&display_handle);
        let seat_input_method = crate::compositor::input_method::SeatInputMethod::default();
        let input_method_manager_state =
            InputMethodManagerState::new::<Self, _>(&display_handle, {
                let seat_input_method = seat_input_method.clone();
                move |client| seat_input_method.admits(trusted_input_method(client))
            });
        let virtual_keyboard_manager_state =
            VirtualKeyboardManagerState::new::<Self, _>(&display_handle, {
                let seat_input_method = seat_input_method.clone();
                move |client| seat_input_method.admits(trusted_input_method(client))
            });
        // Advertise `wp_color_manager_v1`, before the state literal below since `display_handle` is moved into the struct.
        let color_manager = ColorManagerState::new::<Self>(&display_handle);
        // Advertise `wp_fractional_scale_manager_v1` + `wp_viewporter`
        // together, letting HiDPI clients render at a fractional scale
        // (e.g. 1.5×) and have the compositor composite the correct logical size; the preferred scale is pushed per-surface on commit.
        let fractional_scale_manager = FractionalScaleManagerState::new::<Self>(&display_handle);
        let viewporter = ViewporterState::new::<Self>(&display_handle);
        let idle_inhibit_manager = IdleInhibitManagerState::new::<Self>(&display_handle);
        let toplevel_icon_manager = crate::compositor::toplevel_icon::manager(&display_handle);
        let (session_lock_state, output_management) = Self::privileged_globals(&display_handle);
        // Advertise touchpad gestures for a bare-TTY session with no host compositor; constructed before the struct literal moves `display_handle`.
        let pointer_gestures_state = PointerGesturesState::new::<Self>(&display_handle);
        let cursor_shape_state = CursorShapeManagerState::new::<Self>(&display_handle);
        let relative_pointer_state = RelativePointerManagerState::new::<Self>(&display_handle);
        let pointer_constraints_state = PointerConstraintsState::new::<Self>(&display_handle);
        let tablet_manager_state = TabletManagerState::new::<Self>(&display_handle);
        let keyboard_shortcuts_inhibit_state =
            KeyboardShortcutsInhibitState::new::<Self>(&display_handle);
        let content_type_state = ContentTypeState::new::<Self>(&display_handle);
        // Clipboard + primary selection + drag-and-drop, advertised before
        // the struct literal (which moves `display_handle`). Selection
        // *focus* is pushed alongside keyboard focus every tick — see `client_input::sync_keyboard_focus` — since smithay gates `set_selection` on it.
        let selection = SelectionState::new(&display_handle);

        let mut state = Self {
            loop_handle,
            display_handle,
            compositor_state,
            presentation_state,
            physical_key_input_time_ns: None,
            presentation_feedback: crate::compositor::presentation::PresentationFeedback::default(),
            xdg_shell_state,
            xdg_activation_state,
            xdg_dialog_state,
            host_launch_activations: HashMap::new(),
            host_launch_surface_labels: HashMap::new(),
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
            remote_closes: Vec::new(),
            output_manager_state,
            workspace_tree,
            share_registry: ShareRegistry::new(),
            remote_input_firewall: RemoteInputFirewall::new(),
            global_shortcuts: crate::portal::global_shortcuts::ShellGlobalShortcutsService::global(
            ),
            last_remote_input: None,
            last_remote_input_denial: None,
            surface_tracker: SurfaceTracker::new(),
            seat,
            serial_counter: 0,
            activation_serials: VecDeque::new(),
            clients_given_input: std::collections::HashSet::new(),
            start_time: std::time::Instant::now(),
            remote_seats: RemoteSeats::new(),
            personal_fabric_input: PersonalFabricInputHolders::default(),
            surface_handles: HashMap::new(),
            active_sessions: HashMap::new(),
            paint_apps: HashMap::new(),
            legacy_stream_publisher: None,
            legacy_stream_routes: HashMap::new(),
            personal_stream_viewers: std::collections::HashSet::new(),
            personal_keyboard_focus: None,
            headless_host_viewers: std::collections::HashSet::new(),
            headless_keyboard_focus: None,
            personal_pointer_hold: false,
            capture_placeholder_fill: crate::shell::capture_protection::placeholder_fill_rgba8(
                &ui::theme::Theme::default(),
            ),
            color_manager,
            fractional_scale_manager,
            viewporter,
            idle_inhibit_manager,
            idle_inhibitors: crate::compositor::idle_inhibit::IdleInhibitors::default(),
            toplevel_icon_manager,
            // The session sets the persisted scale from its live settings
            // ([`display_scale_from`]); a bare `State` advertises 100%.
            scale: 1.0,
            output_layout: Vec::new(),
            output_sizes: HashMap::new(),
            output_rects: HashMap::new(),
            output_table: Vec::new(),
            presenter_pin: None,
            surface_scales: HashMap::new(),
            fullscreen_group: None,
            fullscreen_output: None,
            output_presentation_surfaces: Vec::new(),
            pending_presentation_outputs: HashMap::new(),
            output_presentation_focus: None,
            output_presentation_sequences: HashMap::new(),
            fullscreen_target: None,
            applied_keyboard,
            remote_typing: false,
            output_size: (0, 0),
            session_lock_state,
            session_lock: SessionLockHooks::default(),
            output_management,
            pointer_gestures_state,
            cursor_shape_state,
            client_cursor: crate::compositor::client_cursor::ClientCursor::default(),
            relative_pointer_state,
            pointer_constraints_state,
            pointer_lock: crate::compositor::pointer_lock::PointerLock::default(),
            tablet_manager_state,
            tablet_clients: crate::compositor::tablet::TabletClients::default(),
            keyboard_shortcuts_inhibit_state,
            shortcut_inhibit: crate::compositor::shortcut_inhibit::ShortcutInhibit::default(),
            content_type_state,
            client_input: crate::compositor::client_input::ClientInput::default(),
            popups: Vec::new(),
            shell_pongs: crate::compositor::client_settle::Pongs::default(),
            popup_manager: PopupManager::default(),
            input_method_popups: Vec::new(),
            seat_input_method,
            trusted_input_method_clients: Vec::new(),
            text_fields: crate::compositor::text_commit::TextFields::default(),
            private_text_input: crate::compositor::private_text_input::PrivateTextInput::default(),
            text_commit: crate::compositor::text_commit::TextCommit::default(),
            osk_surface: None,
            osk_commits: 0,
            shell_field_active: false,
            shell_field_keys: Vec::new(),
            virtual_keymaps: std::collections::HashMap::new(),
            closing_toplevels: HashMap::new(),
            pending_floating_toplevels: Vec::new(),
            floating_toplevels: Vec::new(),
            floating_toplevel_events: Vec::new(),
            capture_protection_sync: CaptureProtectionSync::default(),
        };
        state.activate_keyboard_source(initial_keyboard_index);
        state
    }

    /// Globals only clients on the privileged socket may see. Session
    /// locking controls the compositor's authentication boundary; output
    /// management and output power can reconfigure or black out every
    /// screen. Sandboxed and ordinary host applications receive the public
    /// socket and see neither.
    fn privileged_globals(
        display_handle: &DisplayHandle,
    ) -> (SessionLockManagerState, OutputManagementState) {
        (
            SessionLockManagerState::new::<Self, _>(display_handle, is_privileged_client),
            OutputManagementState::new(display_handle),
        )
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
        //    10-bit RGB, half float and P010 are converted there too, so
        //    HDR clients (PQ swapchains, 10-bit video) can hand them over.
        let dmabuf_formats: Vec<Format> = [
            Fourcc::Argb8888,
            Fourcc::Xrgb8888,
            Fourcc::Abgr8888,
            Fourcc::Xbgr8888,
            Fourcc::Abgr2101010,
            Fourcc::Xbgr2101010,
            Fourcc::Argb2101010,
            Fourcc::Xrgb2101010,
            Fourcc::Abgr16161616f,
            Fourcc::Xbgr16161616f,
            Fourcc::P010,
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
                            crate::compositor::gpu_health::record_dmabuf(Ok(&node.path));
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
                            crate::compositor::gpu_health::record_dmabuf(Err(&e.to_string()));
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
                    if !matches!(e, ProbeError::NoRenderNodes | ProbeError::NoRenderer(_)) {
                        crate::compositor::gpu_health::record_dmabuf(Err(&e.to_string()));
                    }
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
    /// compile. Returns the settings the keymap was compiled from (the US
    /// fallback when the saved sources failed). Split out of [`Self::new`] to keep that constructor under the line-count budget.
    fn init_seat_keyboard(
        seat: &mut Seat<Self>,
    ) -> crate::compositor::backend::drm::input_config::KeyboardSettings {
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
                model: &keyboard_settings.model,
                layout: &keyboard_settings.layouts,
                variant: &keyboard_settings.variants,
                options: keyboard_settings.options.clone(),
                ..Default::default()
            },
            repeat_delay_ms,
            repeat_rate_hz,
        ) {
            Ok(_) => keyboard_settings,
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
                crate::compositor::backend::drm::input_config::KeyboardSettings::default()
            }
        }
    }

    /// Switch the seat to input source `index`; clients learn the new group
    /// through the ordinary modifiers event, and every key name the shell
    /// shows from now on follows the new layout's keycaps.
    pub(crate) fn activate_keyboard_source(&mut self, index: usize) {
        ui::keycaps::set_active(self.applied_keyboard.keycaps(index));
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
        self.apply_keyboard_settings(settings);
    }

    pub(crate) fn apply_keyboard_settings(
        &mut self,
        settings: crate::compositor::backend::drm::input_config::KeyboardSettings,
    ) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let same_keymap = self.applied_keyboard.model == settings.model
            && self.applied_keyboard.layouts == settings.layouts
            && self.applied_keyboard.variants == settings.variants
            && self.applied_keyboard.options == settings.options;
        let compiled = if same_keymap {
            // Only the active source moved: no keymap to re-send.
            Ok(())
        } else {
            keyboard.set_xkb_config(
                self,
                XkbConfig {
                    model: &settings.model,
                    layout: &settings.layouts,
                    variant: &settings.variants,
                    options: settings.options.clone(),
                    ..Default::default()
                },
            )
        };
        match compiled {
            Ok(()) => {
                tracing::info!(
                    sources = ?settings.source_ids,
                    active = settings.active_index,
                    "Wayland seat: applied per-user XKB sources"
                );
                let active = settings.active_index;
                self.applied_keyboard = settings;
                self.activate_keyboard_source(active);
                let (rate, delay) = Self::local_key_repeat();
                keyboard.change_repeat_info(if self.remote_typing { 0 } else { rate }, delay);
            }
            Err(error) => tracing::warn!(
                sources = ?settings.source_ids,
                %error,
                "Wayland seat: requested XKB keymap did not compile; preserving the working keymap"
            ),
        }
    }

    /// The configured client key repeat as `wl_keyboard.repeat_info` takes
    /// it: rate in Hz, delay in milliseconds.
    fn local_key_repeat() -> (i32, i32) {
        let (delay, rate) = crate::compositor::backend::drm::input_config::InputSettings::load()
            .key_repeat_params();
        // Neither side of a repeat rate/delay can realistically reach i32::MAX; clamp rather than let a pathological config value wrap.
        (
            i32::try_from(rate).unwrap_or(i32::MAX),
            i32::try_from(delay.as_millis()).unwrap_or(i32::MAX),
        )
    }

    /// Clients stop repeating keys on their own: a remote participant's
    /// key is held exactly as long as the sender says, and repeats only
    /// as the sender repeats it.
    fn begin_remote_typing(&mut self) {
        if std::mem::replace(&mut self.remote_typing, true) {
            return;
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.change_repeat_info(0, Self::local_key_repeat().1);
        }
    }

    /// The person at this machine types again: clients repeat held keys
    /// with the configured rate and delay.
    pub fn end_remote_typing(&mut self) {
        if !std::mem::take(&mut self.remote_typing) {
            return;
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            let (rate, delay) = Self::local_key_repeat();
            keyboard.change_repeat_info(rate, delay);
        }
    }

    /// Mirror the backend's output table.
    ///
    /// Called at startup and on every hotplug. Dropping the per-surface
    /// scale cache alongside it is deliberate: the layout the cached values were derived from is gone, so keeping them would hand clients the scale of a monitor that moved.
    pub fn set_output_layout(&mut self, outputs: &[crate::compositor::backend::Output]) {
        self.output_table = outputs.to_vec();
        // Logical, like every surface position it is looked up with.
        self.output_layout = outputs
            .iter()
            .map(|o| (o.logical_position, o.logical_size(), f64::from(o.scale)))
            .collect();
        self.output_sizes = outputs
            .iter()
            .filter_map(|output| {
                let (width, height) = output.logical_size();
                let width = i32::try_from(width).ok()?;
                let height = i32::try_from(height).ok()?;
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
        self.reconcile_presenter_pin();
    }

    fn output_rect(
        outputs: &[crate::compositor::backend::Output],
        id: crate::compositor::backend::OutputId,
    ) -> Option<Rect> {
        let output = outputs
            .iter()
            .find(|output| output.id == id && output.size.0 > 0 && output.size.1 > 0)?;
        Some(output.logical_rect())
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
        let owner = {
            let tree = self.workspace_tree.read();
            output_presentation_owner(&tree, surface_id, &app_id)
        };
        let Some(owner) = owner else {
            tracing::warn!(surface_id, %app_id, "rejected unpaired audience presentation surface");
            return false;
        };
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
        // A client that named no output (winit sets the app-id before it
        // can request fullscreen) gets the screen the app would have picked;
        // one screen is simply that screen.
        let output = requested
            .filter(|id| self.output_sizes.contains_key(id))
            .or_else(|| self.audience_fallback_output(owner))
            .or_else(|| self.output_sizes.keys().min_by_key(|id| id.0).copied());
        let Some(output) = output else {
            // Keep the ordinary tab when there is no output yet;
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
        self.reconcile_presenter_pin();
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
        let removed = before != self.output_presentation_surfaces.len();
        if removed {
            self.reconcile_presenter_pin();
        }
        removed
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

    /// Outputs the person is using right now: the one under the pointer and
    /// the one showing the focused window.
    pub fn watched_outputs(&self) -> BTreeSet<crate::compositor::backend::OutputId> {
        let output_at = |point: vello::kurbo::Point| {
            self.output_rects
                .iter()
                .find(|(_, rect)| rect.contains(point))
                .map(|(id, _)| *id)
        };
        let pointer = self.seat.get_pointer().and_then(|pointer| {
            let location = pointer.current_location();
            output_at((location.x, location.y).into())
        });
        let focus = self
            .client_input
            .keyboard_focus()
            .and_then(|surface| crate::compositor::foreign::store().placement(surface))
            .and_then(|rect| output_at(rect.center()));
        pointer.into_iter().chain(focus).collect()
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
        let live: HashSet<u64> = placements.iter().map(|(id, _)| *id).collect();
        self.surface_scales.retain(|id, _| live.contains(id));
    }

    /// The theme's capture placeholder colour, for windows streamed to
    /// collaborators while protected.
    pub fn set_capture_placeholder_fill(&mut self, fill: [u8; 4]) {
        self.capture_placeholder_fill = fill;
    }

    /// Whether shared window `surface` must reach its viewers as the
    /// placeholder instead of its pixels.
    ///
    /// The app behind a tiled surface comes from the input router's cache of
    /// the tree, without the tree lock. Until the loop folds a tree write
    /// into that cache, the tree itself is read, so a renamed or newly
    /// adopted tab is never judged by a stale app id.
    pub fn is_surface_capture_protected(&self, surface: SurfaceId) -> bool {
        let floating = || {
            self.floating_toplevels
                .iter()
                .find(|toplevel| toplevel.surface_id == surface)
                .map(|toplevel| toplevel.app_id.as_str())
        };
        let store = crate::compositor::foreign::store();
        if self
            .client_input
            .app_ids_current(self.workspace_tree.generation())
        {
            let app_id = self.client_input.app_id(surface).or_else(floating);
            return store.is_capture_protected(surface, app_id.unwrap_or_default());
        }
        let tiled = {
            let tree = self.workspace_tree.read();
            tab_binding::app_id_for_surface(&tree, surface).map(str::to_owned)
        };
        let app_id = tiled.as_deref().or_else(floating);
        store.is_capture_protected(surface, app_id.unwrap_or_default())
    }

    /// A share session's next frame: the window's pixels, or the themed
    /// placeholder at the stream's size while the window is protected.
    fn share_frame(
        &self,
        session: &SharedFlatpakSession,
        surface: SurfaceId,
        pixels: impl FnOnce() -> Option<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        if self.is_surface_capture_protected(surface) {
            let (width, height) = session.dimensions();
            return Some(crate::shell::capture_protection::placeholder_rgba(
                width,
                height,
                self.capture_placeholder_fill,
            ));
        }
        pixels()
    }

    /// Install the non-blocking network bridge used by live legacy streams.
    pub fn set_legacy_stream_publisher(&mut self, publisher: LegacyStreamPublisher) {
        self.legacy_stream_publisher = Some(publisher);
    }

    /// Send one message of `surface`'s stream on its room's topic: to its
    /// viewers alone when they are all the person's own devices.
    fn publish_legacy(&self, surface: SurfaceId, message: crate::network::NeighbourMsg) -> bool {
        let Some(publish) = self.legacy_stream_publisher.as_ref() else {
            return false;
        };
        let viewers = self
            .active_sessions
            .get(&surface)
            .map(|session| session.viewer_ids())
            .unwrap_or_default();
        let personal = viewers
            .iter()
            .all(|peer| self.personal_stream_viewers.contains(&(*peer, surface)));
        publish(
            LegacyStreamRoute {
                topic: self.legacy_stream_routes.get(&surface).copied(),
                only: (personal && !viewers.is_empty()).then_some(viewers),
            },
            message,
        )
    }

    /// Point each streamed window's traffic at its tab's room topic. Routes
    /// of windows no longer streamed are kept until their session is gone,
    /// so the end of a stream reaches the room its start went to.
    pub fn route_legacy_streams(&mut self, routes: impl IntoIterator<Item = (SurfaceId, u64)>) {
        let sessions = &self.active_sessions;
        self.legacy_stream_routes
            .retain(|surface, _| sessions.contains_key(surface));
        self.legacy_stream_routes.extend(routes);
    }

    /// Encode and send each streamed window's held-back frame that is due
    /// now. Returns how many went out.
    pub fn flush_share_sessions(&mut self) -> usize {
        let now = std::time::Instant::now();
        let mut sent = 0_usize;
        let sessions = self.active_sessions.values().cloned().collect::<Vec<_>>();
        for session in sessions {
            if session.staged_due().is_some_and(|due| due <= now)
                && let Some(frame) = session.encode_next_frame(now)
            {
                let _ = self.publish_legacy(session.surface_id, frame.into_neighbour_msg());
                sent = sent.saturating_add(1);
            }
        }
        sent
    }

    /// When the earliest held-back frame of a streamed window is due.
    pub fn legacy_stream_deadline(&self) -> Option<std::time::Instant> {
        self.active_sessions
            .values()
            .filter_map(|session| session.staged_due())
            .min()
    }

    /// Which viewers of which windows are the person's own other devices.
    pub fn set_personal_stream_viewers(
        &mut self,
        viewers: std::collections::HashSet<(MsgPeerId, SurfaceId)>,
    ) {
        self.personal_stream_viewers = viewers;
        if let Some(focus) = self.personal_keyboard_focus
            && !self.streams_to_own_device(focus)
        {
            self.personal_keyboard_focus = None;
        }
    }

    fn streams_to_own_device(&self, surface: SurfaceId) -> bool {
        self.personal_stream_viewers
            .iter()
            .any(|(_, streamed)| *streamed == surface)
    }

    /// Only headless sessions populate this from the active room's trusted
    /// workspace hosts. Ordinary desktop shares keep the local focus policy.
    pub fn set_headless_host_viewers(
        &mut self,
        viewers: std::collections::HashSet<(MsgPeerId, SurfaceId)>,
    ) {
        self.headless_host_viewers = viewers;
        if self
            .headless_keyboard_focus
            .is_some_and(|focus| !self.headless_host_viewers.contains(&focus))
        {
            self.headless_keyboard_focus = None;
        }
    }

    pub(crate) fn headless_keyboard_focus(&self) -> Option<SurfaceId> {
        self.headless_keyboard_focus.and_then(|(peer, surface)| {
            (self.headless_host_viewers.contains(&(peer, surface))
                && self.client_input.surface(surface).is_some())
            .then_some(surface)
        })
    }

    /// Called only after the share, visibility, host overrides and event
    /// budget accepted a focus packet. The trusted workspace host chooses
    /// which window its headless session types into; regular shares do not.
    fn apply_headless_host_focus(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        event: &InputEvent,
    ) {
        let InputEvent::KeyboardFocus { focused, .. } = event else {
            return;
        };
        if !focused {
            if self.headless_keyboard_focus == Some((peer, surface)) {
                self.headless_keyboard_focus = None;
            }
            return;
        }
        if !self.headless_host_viewers.contains(&(peer, surface)) {
            if self.personal_stream_viewers.contains(&(peer, surface)) {
                self.headless_keyboard_focus = None;
            }
            return;
        }
        self.personal_keyboard_focus = None;
        self.headless_keyboard_focus = Some((peer, surface));
        if !self.remote_input_firewall.view().chrome_captures_input() {
            self.set_activated_toplevel(Some(surface));
            self.set_client_keyboard_focus(Some(surface));
            // A key can follow focus in the same packet, before the next
            // frame publishes its view. Preserve all other firewall gates.
            self.remote_input_firewall.set_keyboard_focus(Some(surface));
        }
    }

    /// The streamed window the person last focused from another device,
    /// which takes this seat's keyboard ahead of the local tiles.
    pub(crate) fn personal_keyboard_focus(&self) -> Option<SurfaceId> {
        self.personal_keyboard_focus
            .filter(|surface| self.client_input.surface(*surface).is_some())
    }

    /// The person is using this device's own keyboard or pointer again.
    pub(crate) fn release_personal_keyboard_focus(&mut self) {
        self.personal_keyboard_focus = None;
        self.headless_keyboard_focus = None;
    }

    /// Whether another device of the person holds this seat's pointer, and
    /// end that hold: local input is about to take it back.
    pub(crate) fn take_personal_pointer_hold(&mut self) -> bool {
        std::mem::take(&mut self.personal_pointer_hold)
    }

    /// Where input from the person's other devices lands on a streamed
    /// window no tile here shows: a rect the size they show it at, beyond
    /// every output, so nothing local covers it and the local pointer never
    /// reaches it. `shown` are the tiles already on screen.
    pub(crate) fn offscreen_stream_tiles(
        &self,
        shown: &[(SurfaceId, vello::kurbo::Rect)],
    ) -> Vec<(SurfaceId, vello::kurbo::Rect)> {
        /// Far beyond any output arrangement, in logical pixels.
        const ORIGIN_X: f64 = 1_048_576.0;
        let mut surfaces = self
            .personal_stream_viewers
            .iter()
            .map(|(_, surface)| *surface)
            .filter(|surface| {
                !shown.iter().any(|(id, _)| id == surface)
                    && self.client_input.surface(*surface).is_some()
            })
            .collect::<Vec<_>>();
        surfaces.sort_unstable();
        surfaces.dedup();
        let mut x = ORIGIN_X;
        surfaces
            .into_iter()
            .filter_map(|surface| {
                let (width, height) = crate::compositor::foreign::store().remote_extent(surface)?;
                let rect = vello::kurbo::Rect::new(
                    x,
                    0.0,
                    x + f64::from(width.max(1)),
                    f64::from(height.max(1)),
                );
                x = rect.x1 + 64.0;
                Some((surface, rect))
            })
            .collect()
    }

    /// A stream carries the window at its buffer's size; the tile placing
    /// it here may be laid out at another (a scaled output, or a size other
    /// devices asked for). Move a viewer's pointer from the stream's pixels
    /// to the tile's.
    fn stream_point_to_tile(&self, surface: SurfaceId, event: InputEvent) -> InputEvent {
        let InputEvent::PointerMotion { x, y } = event else {
            return event;
        };
        let Some((width, height)) = self
            .active_sessions
            .get(&surface)
            .map(|session| session.encoded_dimensions())
            .filter(|(width, height)| *width > 0 && *height > 0)
        else {
            return event;
        };
        let Some(rect) = self.remote_input_firewall.view().rect(surface) else {
            return event;
        };
        InputEvent::PointerMotion {
            x: x * rect.width() / f64::from(width),
            y: y * rect.height() / f64::from(height),
        }
    }

    /// Register a share session so [`CompositorHandler::commit`] pumps
    /// `encode_next_frame` on every commit; replaces any prior session and
    /// announces the new stream before its first frame.
    pub fn register_share_session(&mut self, session: Arc<SharedFlatpakSession>) {
        let replaced = self.active_sessions.remove(&session.surface_id);
        self.install_share_session(session, replaced);
    }

    /// Put `session` in place of the window's `replaced` one. The window
    /// stays shared, so its new start alone announces the change (receivers
    /// rebuild on it): an end first would have them drop the window, and
    /// the input they send it, until the start arrives. Only viewers the
    /// new session no longer has lose their focus on it.
    fn install_share_session(
        &mut self,
        session: Arc<SharedFlatpakSession>,
        replaced: Option<Arc<SharedFlatpakSession>>,
    ) {
        let surface = session.surface_id;
        if self.legacy_stream_publisher.is_some() {
            let _ = self.publish_legacy(surface, session.stream_start_msg());
        }
        self.sync_share_authority(&session);
        let viewers = session.viewer_ids();
        self.active_sessions.insert(surface, session);
        let left = replaced
            .map(|old| old.viewer_ids())
            .unwrap_or_default()
            .into_iter()
            .filter(|peer| !viewers.contains(peer));
        for peer in left {
            self.release_remote_focus_for_surface(peer, surface);
        }
    }

    /// Mirror the stream session's actual viewer permissions into the
    /// compositor authority used by input gating and trusted disclosure.
    /// This must happen when viewing starts—not lazily on a first input
    /// packet—or a quiet read-only viewer would be invisible to the host.
    fn sync_share_authority(&mut self, session: &SharedFlatpakSession) {
        let surface = session.surface_id;
        let lost_control = session
            .viewer_ids()
            .into_iter()
            .filter(|peer| {
                self.share_registry
                    .get(*peer, surface)
                    .is_some_and(|old| !old.read_only)
                    && session.viewer_share(*peer).is_some_and(|new| new.read_only)
            })
            .collect::<Vec<_>>();
        for peer in self.share_registry.peers_for(surface) {
            self.share_registry.remove(peer, surface);
        }
        for peer in session.viewer_ids() {
            if let Some(share) = session.viewer_share(peer) {
                self.share_registry.insert(peer, surface, share);
            }
        }
        for peer in lost_control {
            self.release_remote_focus_for_surface(peer, surface);
        }
    }

    /// Deregister a share session by surface id (no-op if unregistered) — called on share end or surface destroy.
    pub fn unregister_share_session(&mut self, surface: SurfaceId) {
        // The end goes the way the stream went, so it is sent while the
        // session still names its viewers.
        if let Some(session) = self.active_sessions.get(&surface).cloned()
            && self.legacy_stream_publisher.is_some()
        {
            let _ = self.publish_legacy(surface, session.stream_end_msg());
        }
        if let Some(session) = self.active_sessions.remove(&surface) {
            let viewers = session.viewer_ids();
            for peer in viewers {
                self.share_registry.remove(peer, surface);
                self.release_remote_focus_for_surface(peer, surface);
            }
        }
    }

    /// A revoked viewer must not leave the legacy app's one seat with a held
    /// key or stale caret focus. Reference counting ensures releases still
    /// held by another collaborator are suppressed.
    fn release_remote_focus_for_surface(&mut self, peer: MsgPeerId, surface: SurfaceId) {
        if self.headless_keyboard_focus == Some((peer, surface)) {
            self.headless_keyboard_focus = None;
        }
        let (owns_focus, owns_pointer) =
            self.remote_seats.seat(peer).map_or((false, false), |seat| {
                (
                    seat.focused_surface == Some(surface),
                    seat.pointer_surface == Some(surface),
                )
            });
        if !owns_focus && !owns_pointer {
            return;
        }
        let seat = self.remote_seats.seat_mut(peer);
        if owns_focus {
            let _ = seat.leave();
        }
        if owns_pointer {
            let _ = seat.pointer_leave();
        }
        self.release_remote_inputs_for_peer(peer);
    }

    /// Retire the keys attributed to one remote participant from the shared
    /// app-facing seat. The participant's logical caret focus is deliberately
    /// left alone: switching the host's active tab temporarily mutes that
    /// collaborator, but returning to the shared tab should not require a new
    /// pointer click.
    fn release_remote_inputs_for_peer(&mut self, peer: MsgPeerId) {
        let releases = self.remote_seats.release_peer_keys(peer);
        let time = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
        for keycode in releases {
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
        self.release_remote_pointer_buttons_for_peer(peer);
    }

    fn release_remote_pointer_buttons_for_peer(&mut self, peer: MsgPeerId) {
        let time = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
        for button in self.remote_seats.release_peer_buttons(peer) {
            if let Some(pointer) = self.seat.get_pointer() {
                let serial = self.next_serial();
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: smithay::backend::input::ButtonState::Released,
                        serial,
                        time,
                    },
                );
                if let Some(pointer) = self.seat.get_pointer() {
                    pointer.frame(self);
                }
            }
        }
    }

    /// Clear remote-held keys before the single legacy Wayland seat moves
    /// away from `surface`. Otherwise Smithay includes those held keys in the
    /// focus-enter sent to the next app, making another participant's late
    /// release or modifier state spill across app boundaries.
    pub(crate) fn release_remote_keys_for_surface(&mut self, surface: SurfaceId) {
        let peers = self
            .remote_seats
            .peer_ids()
            .into_iter()
            .filter(|peer| {
                self.remote_seats
                    .seat(*peer)
                    .is_some_and(|seat| seat.focused_surface == Some(surface))
            })
            .collect::<Vec<_>>();
        for peer in peers {
            self.release_remote_inputs_for_peer(peer);
        }
    }

    /// Bring a running share up to date with its current viewer set: grant
    /// and revoke workspace control, renegotiate the codec and announce the
    /// stream again when a viewer joined or the codec switched.
    fn update_reused_share_session(
        &mut self,
        session: &SharedFlatpakSession,
        surface: SurfaceId,
        recipients: &std::collections::HashSet<MsgPeerId>,
        receivers: &[media::realtime::CodecCapabilities],
    ) {
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
        if self.legacy_stream_publisher.is_some() {
            for peer in &removed {
                let _ = self.publish_legacy(
                    surface,
                    crate::network::NeighbourMsg::PermissionGrant {
                        peer: *peer,
                        capability: format!("workspace-control:{surface}:revoked"),
                    },
                );
            }
            for peer in &added {
                let _ = self.publish_legacy(
                    surface,
                    crate::network::NeighbourMsg::PermissionGrant {
                        peer: *peer,
                        capability: format!("workspace-control:{surface}:granted"),
                    },
                );
            }
        }
        // A viewer joining or leaving (or re-advertising) can change which
        // codec everyone decodes; a switch is announced like a new stream so
        // receivers rebuild.
        let switched = session.renegotiate(receivers);
        if (switched || !added.is_empty()) && self.legacy_stream_publisher.is_some() {
            let _ = self.publish_legacy(surface, session.stream_start_msg());
        }
        session.sync_viewers(recipients);
        self.sync_share_authority(session);
        for peer in removed {
            self.release_remote_focus_for_surface(peer, surface);
        }
        // A new viewer starts at the keyframe `sync_viewers` asked for.
        if !added.is_empty() {
            self.seed_share_frame(session, surface);
        }
    }

    /// Reconcile encoder lifetimes and viewer permissions with the windows
    /// that have viewers: shares in the active workspace's replicated
    /// `shared_surfaces` map, and windows the person's other devices show. A
    /// newly shared static window is seeded from the compositor's already
    /// captured foreign frame, so it does not have to damage itself again
    /// before the first packet.
    pub fn reconcile_legacy_share_sessions(
        &mut self,
        host_peer: MsgPeerId,
        desired: &HashMap<SurfaceId, LegacyShare>,
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

        for (&surface, share) in desired {
            let LegacyShare {
                app_id,
                tab,
                viewers: recipients,
                decode_limits,
                paint,
            } = share;
            let paint = *paint && self.paint_apps.contains_key(app_id);
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
            // What each viewer advertised it can decode; a viewer that never
            // advertised is treated as H.264-only.
            let receivers = recipients
                .iter()
                .map(|peer| {
                    let mut caps = crate::network::media_caps::capabilities(*peer);
                    if let Some(limits) = decode_limits.get(peer) {
                        caps.hardware_decode.extend(limits.keys().copied());
                        caps.hardware_decode_max.clone_from(limits);
                    }
                    caps
                })
                .collect::<Vec<_>>();
            let reusable = self.active_sessions.get(&surface).is_some_and(|session| {
                session.app_id == *app_id
                    && session.tab() == *tab
                    && session.dimensions() == (width, height)
                    && session.is_paint() == paint
            });
            if reusable {
                if let Some(session) = self.active_sessions.get(&surface).cloned() {
                    self.update_reused_share_session(&session, surface, &recipients, &receivers);
                }
                continue;
            }

            // The window is still shared: its stream is replaced, not ended,
            // unless the new one cannot start.
            let replaced = self.active_sessions.remove(&surface);
            let started = if paint {
                SharedFlatpakSession::start_paint(host_peer, app_id, surface, width, height, config)
            } else {
                SharedFlatpakSession::start_with_metrics(
                    host_peer, app_id, surface, width, height, config, &receivers, None,
                )
            };
            let Ok(session) = started else {
                tracing::warn!(
                    surface,
                    app_id,
                    width,
                    height,
                    "legacy share encoder failed to start"
                );
                if let Some(replaced) = replaced {
                    self.active_sessions.insert(surface, replaced);
                    self.unregister_share_session(surface);
                }
                continue;
            };
            let session = Arc::new(session.with_tab(*tab));
            session.sync_viewers(&recipients);
            if self.legacy_stream_publisher.is_some() {
                for peer in &recipients {
                    let _ = self.publish_legacy(
                        surface,
                        crate::network::NeighbourMsg::PermissionGrant {
                            peer: *peer,
                            capability: format!("workspace-control:{surface}:granted"),
                        },
                    );
                }
            }
            self.install_share_session(Arc::clone(&session), replaced);
            if paint {
                self.seed_paint_share(&session, surface);
            } else {
                self.seed_share_frame(&session, surface);
            }
        }
    }

    /// Start a new paint share from the app's current group (its latest
    /// whole frame and what followed), and ask the app for a fresh one.
    fn seed_paint_share(&mut self, session: &SharedFlatpakSession, surface: SurfaceId) {
        let Some(app) = self.paint_apps.get_mut(&session.app_id) else {
            return;
        };
        app.keyframe_wanted = true;
        let group = app.group.clone();
        if self.legacy_stream_publisher.is_some() {
            for payload in group {
                let _ = self.publish_legacy(surface, session.paint_message(payload));
            }
        }
    }

    /// An app announced it can stream its page (`shell-ui` `PaintCapable`).
    pub fn note_paint_app(&mut self, app_id: &str, connection: u64) {
        self.paint_apps
            .entry(app_id.to_owned())
            .or_insert_with(|| PaintApp::new(connection))
            .connection = connection;
    }

    /// The app's `shell-ui` connection closed: its windows go back to video.
    pub fn drop_paint_connection(&mut self, connection: u64) {
        self.paint_apps
            .retain(|_, app| app.connection != connection);
    }

    /// One frame of an app's page stream: kept for viewers that start later
    /// and sent to every paint share of the app's windows.
    pub fn receive_paint_frame(&mut self, app_id: &str, frame: PaintFrame) {
        let PaintFrame {
            payload,
            page,
            scale,
        } = frame;
        let Some(app) = self.paint_apps.get_mut(app_id) else {
            return;
        };
        app.page = page;
        app.scale = scale;
        if ui::webview::paint::stream::wire::Payload::is_key(&payload) {
            app.group.clear();
            app.group_bytes = 0;
        }
        app.group_bytes = app.group_bytes.saturating_add(payload.len());
        let keep = app.group_bytes <= MAX_PAINT_GROUP_BYTES;
        if !keep {
            // Too long to replay: start the next viewer from a new frame.
            app.group.clear();
            app.group_bytes = 0;
            app.keyframe_wanted = true;
        }
        if self.legacy_stream_publisher.is_some() {
            let sessions = self
                .active_sessions
                .values()
                .filter(|session| session.is_paint() && session.app_id == app_id)
                .cloned()
                .collect::<Vec<_>>();
            for session in sessions {
                let _ =
                    self.publish_legacy(session.surface_id, session.paint_message(payload.clone()));
            }
        }
        if keep && let Some(app) = self.paint_apps.get_mut(app_id) {
            app.group.push(payload);
        }
    }

    /// What to tell each paint app: `(connection, true)` to start streaming
    /// from a whole frame, `(connection, false)` to stop. Watching follows
    /// the paint shares of the app's windows.
    pub fn paint_watch_changes(&mut self) -> Vec<(u64, bool)> {
        let watched = self
            .active_sessions
            .values()
            .filter(|session| session.is_paint())
            .map(|session| session.app_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut changes = Vec::new();
        for (app_id, app) in &mut self.paint_apps {
            let now = watched.contains(app_id);
            if now && (app.keyframe_wanted || !app.watched) {
                changes.push((app.connection, true));
            } else if !now && app.watched {
                changes.push((app.connection, false));
            }
            app.watched = now;
            app.keyframe_wanted = false;
        }
        changes
    }

    /// Input from a paint share's viewer is in page (CSS) pixels; the
    /// window's surface-local input is logical, with the page at `page`.
    fn paint_point_to_surface(&self, surface: SurfaceId, event: InputEvent) -> InputEvent {
        let InputEvent::PointerMotion { x, y } = event else {
            return event;
        };
        let Some(app) = self
            .active_sessions
            .get(&surface)
            .filter(|session| session.is_paint())
            .and_then(|session| self.paint_apps.get(&session.app_id))
        else {
            return event;
        };
        let [left, top, width, height] = app.page;
        InputEvent::PointerMotion {
            x: left + x.clamp(0.0, width.max(0.0)),
            y: top + y.clamp(0.0, height.max(0.0)),
        }
    }

    /// Encode and publish `surface`'s already captured frame, so a viewer
    /// can start decoding without the window having to damage itself (a
    /// static window may never do so).
    fn seed_share_frame(&self, session: &SharedFlatpakSession, surface: SurfaceId) {
        if let Some(rgba) = self.share_frame(session, surface, || {
            crate::compositor::foreign::store()
                .snapshot(surface)
                .map(|frame| frame.rgba)
        }) {
            session.deposit_captured_rgba(rgba);
            if let Some(frame) = session.encode_next_frame(std::time::Instant::now())
                && self.legacy_stream_publisher.is_some()
            {
                let _ = self.publish_legacy(surface, frame.into_neighbour_msg());
            }
        }
    }

    /// Stage the buffer a foreign (external-client) toplevel just committed
    /// so its tab can paint the client's actual pixels.
    ///
    /// Split out of [`CompositorHandler::commit`] to keep the hook a plain
    /// dispatch. `wl_shm` is copied here; a dmabuf worker result was staged by
    /// the pre-commit barrier and is activated here. A buffer neither path can
    /// decode is recorded as an error the tile renders, never a silently stale frame.
    /// Hand the frame store the window geometry an xdg toplevel's commit just
    /// applied, so the tile shows the window and not the shadow and resize
    /// border a client-side-decorating toolkit draws around it. A compositor
    /// floating surface is skipped: its soft silhouette is meant to be seen.
    fn note_foreign_window_geometry(surface: &WlSurface, surface_id: u64) {
        let toplevel_geometry = wl_compositor::with_states(surface, |states| {
            states.data_map.get::<XdgToplevelSurfaceData>()?;
            let Some(declared) = states
                .cached_state
                .get::<smithay::wayland::shell::xdg::SurfaceCachedState>()
                .current()
                .geometry
            else {
                return Some(None);
            };
            let buffer_scale = states
                .cached_state
                .get::<wl_compositor::SurfaceAttributes>()
                .current()
                .buffer_scale;
            let logical_size = states
                .cached_state
                .get::<smithay::wayland::viewporter::ViewportCachedState>()
                .current()
                .dst
                .map(|size| (size.w, size.h));
            Some(Some(crate::compositor::foreign::WindowGeometry {
                x: declared.loc.x,
                y: declared.loc.y,
                width: declared.size.w,
                height: declared.size.h,
                logical_size,
                buffer_scale,
            }))
        });
        // Only toplevels: an entry made for any other surface would never be forgotten.
        if let Some(geometry) = toplevel_geometry {
            crate::compositor::foreign::store().note_window_geometry(surface_id, geometry);
        }
    }

    fn capture_foreign_surface(surface: &WlSurface, surface_id: u64) -> ForeignCommitCapture {
        use crate::compositor::foreign::{CaptureOutcome, store};
        wl_compositor::with_states(surface, |states| {
            // SurfaceAttributes.current().buffer is intentionally aggregated
            // until a compositor clears it; therefore it may still contain a
            // previous NewBuffer on a damage-only commit. Consume the marker
            // created by this exact pre-commit hook instead of inferring the
            // commit kind from that sticky current state.
            let association = states
                .data_map
                .get::<ForeignCommitCaptures>()
                .and_then(|commits| {
                    commits
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .pop_front()
                })
                .unwrap_or(ForeignCommitCapture::NoBuffer);
            if matches!(
                association,
                ForeignCommitCapture::NoBuffer | ForeignCommitCapture::Failed
            ) {
                return association;
            }
            if matches!(association, ForeignCommitCapture::Removed) {
                store().clear_frame(surface_id);
                return association;
            }
            let mut attrs = states
                .cached_state
                .get::<wl_compositor::SurfaceAttributes>();
            match &attrs.current().buffer {
                Some(wl_compositor::BufferAssignment::NewBuffer(buffer)) => {
                    let outcome = match association {
                        ForeignCommitCapture::Dmabuf(token) => {
                            store().capture_commit_associated(surface_id, buffer, Some(token))
                        }
                        ForeignCommitCapture::Shm(generation) => {
                            if store().note_shm_committed(surface_id, generation) {
                                CaptureOutcome::Queued
                            } else {
                                CaptureOutcome::Failed("SHM commit generation is not queued".into())
                            }
                        }
                        ForeignCommitCapture::NoBuffer
                        | ForeignCommitCapture::Failed
                        | ForeignCommitCapture::Removed => {
                            return association;
                        }
                    };
                    if let CaptureOutcome::Failed(reason) = outcome {
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
            drop(attrs);
            association
        })
    }

    /// The exact `commit` capture gate, kept named so output-owned surfaces
    /// cannot silently regress back to “only workspace tabs have pixels”.
    fn should_capture_foreign_surface(&self, surface_id: SurfaceId) -> bool {
        let has_workspace_tab = {
            let tree = self.workspace_tree.read();
            tab_binding::group_id_for_surface(&tree, surface_id).is_some()
        };
        has_workspace_tab
            || self.osk_surface == Some(surface_id)
            || self.is_floating_toplevel(surface_id)
            || self.is_output_presentation_surface(surface_id)
            || self.is_transient_surface(surface_id)
            || crate::compositor::foreign::store().is_sheet(surface_id)
    }

    /// Pump the share-session encoder for `surface_id` on a fresh commit.
    /// Called from [`CompositorHandler::commit`] after
    /// [`SurfaceTracker::on_commit`] has staged the buffer bytes.
    ///
    /// `false` when no session is registered or the tracker slot is empty
    /// (damage-only commit); `true` when the deposit was accepted and the
    /// encode tick ran — NOT a guarantee a frame was emitted. Split out so integration tests can drive the tick path without a `WlSurface`.
    pub fn tick_share_session_for_surface(&mut self, surface_id: SurfaceId) -> bool {
        let Some(session) = self.active_sessions.get(&surface_id).cloned() else {
            return false;
        };
        if self.is_surface_capture_protected(surface_id) {
            // Neither the client's buffer nor a readback of it leaves the
            // machine; the commit still paces the placeholder stream.
            let fresh = self
                .surface_tracker
                .take_latest_dmabuf(surface_id)
                .is_some()
                | self.surface_tracker.take_latest(surface_id).is_some();
            if !fresh {
                return false;
            }
            let Some(rgba) = self.share_frame(&session, surface_id, || None) else {
                return false;
            };
            session.deposit_captured_rgba(rgba);
            if let Some(frame) = session.encode_next_frame(std::time::Instant::now())
                && self.legacy_stream_publisher.is_some()
            {
                let _ = self.publish_legacy(surface_id, frame.into_neighbour_msg());
            }
            return true;
        }
        // DMABUF-direct path takes priority when the commit hook stashed a
        // Dmabuf this tick — `take_latest_dmabuf` drains it so a re-tick
        // without a fresh commit finds none. Falling back to SHM covers the mixed-mode case (a client briefly attaching SHM between DMABUF commits).
        if let Some(dmabuf) = self.surface_tracker.take_latest_dmabuf(surface_id) {
            session.deposit_captured_dmabuf(dmabuf);
            let emitted = session.encode_next_frame(std::time::Instant::now());
            if let Some(frame) = emitted {
                if self.legacy_stream_publisher.is_some() {
                    let _ = self.publish_legacy(surface_id, frame.into_neighbour_msg());
                }
                // Encoder actually consumed the DMABUF. Only *now* do we bump
                // the tracker's `dmabuf_direct_count` — the metric reflects
                // zero-copy, not "DMABUF-shaped but rejected and fell back to SHM".
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
        // The stream carries the window as its tab shows it, at the size its
        // first frame (the tab's snapshot) set: without the shadow and resize
        // border a client-side-decorating toolkit draws around it.
        let rgba = crate::compositor::foreign::store().crop_to_window(
            surface_id,
            frame.rgba,
            frame.width,
            frame.height,
        );
        session.deposit_captured_rgba(rgba);
        if let Some(frame) = session.encode_next_frame(std::time::Instant::now())
            && self.legacy_stream_publisher.is_some()
        {
            let _ = self.publish_legacy(surface_id, frame.into_neighbour_msg());
        }
        true
    }

    /// Mint a fresh `Serial` for input events — monotonic, and never wraps in practice (32-bit at 1kHz is 49 days of traffic).
    pub(crate) fn next_serial(&mut self) -> Serial {
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
        let previously_controlled = self
            .share_registry
            .get(peer, surface)
            .is_some_and(|share| !share.read_only);
        let share = self
            .active_sessions
            .get(&surface)
            .and_then(|session| session.viewer_share(peer));
        let still_controls = share.as_ref().is_some_and(|share| !share.read_only);
        if let Some(current) = share {
            self.share_registry.insert(peer, surface, current);
        } else {
            self.share_registry.remove(peer, surface);
        }
        // Permission reconciliation normally performs this transition once
        // per compositor tick. The per-packet refresh above can observe a
        // downgrade or removal first, though. Retire the peer's held keys,
        // drag and caret focus before the newly read-only/not-shared gate
        // refuses its release packet, or a legacy app can be left with a
        // modifier/button held indefinitely.
        if previously_controlled && !still_controls {
            self.release_remote_focus_for_surface(peer, surface);
        }
        let event = self.paint_point_to_surface(surface, event);
        let event = if self.personal_stream_viewers.contains(&(peer, surface)) {
            if matches!(
                event,
                InputEvent::PointerMotion { .. }
                    | InputEvent::PointerButton { .. }
                    | InputEvent::PointerAxis { .. }
                    | InputEvent::PointerEnter { .. }
            ) {
                self.personal_pointer_hold = true;
            }
            if let InputEvent::KeyboardFocus { focused, .. } = event {
                if focused {
                    self.personal_keyboard_focus = Some(surface);
                } else if self.personal_keyboard_focus == Some(surface) {
                    self.personal_keyboard_focus = None;
                }
            }
            self.stream_point_to_tile(surface, event)
        } else {
            event
        };
        match self.review_remote_input(peer, surface, event) {
            Ok(()) => true,
            Err(denial) => {
                tracing::debug!(peer, surface, ?denial, "remote input refused");
                false
            }
        }
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

        let retires_input = matches!(
            event,
            InputEvent::PointerButton { pressed: false, .. }
                | InputEvent::PointerLeave { .. }
                | InputEvent::KeyboardFocus { focused: false, .. }
                | InputEvent::Key { pressed: false, .. }
        );

        // The host's own machine wins, unconditionally, before even the per-peer budget — a flooding peer can't delay the panic key.
        if let Some(denial) = self.remote_input_firewall.host_override(now) {
            // Physical priority blocks new remote actions, not key-up/button-up
            // cleanup. Rejecting those edges can leave the host's currently
            // focused legacy app with a stuck modifier or drag.
            if denial != Denial::PhysicalPriority || !retires_input {
                return Err(self.refuse_remote_input(peer, surface, &event, denial));
            }
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
        self.apply_headless_host_focus(peer, surface, &injectable);
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

        let personal_controllers = self.release_all_personal_fabric_input();

        // Release keys and pointer grabs before dropping attribution.
        // Otherwise a vanished peer could leave the app-facing seat stuck.
        for peer in self.remote_seats.peer_ids() {
            self.release_remote_inputs_for_peer(peer);
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
            personal_controllers,
            "remote-input kill switch engaged; shares and streams terminated"
        );
        revoked
    }

    /// Inject a single `org.freedesktop.impl.portal.RemoteDesktop`
    /// [`RemoteAction`] into the seat, targeting whatever surface holds seat focus.
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
            // A portal session is remote input too: it must not move a
            // pointer a local client holds locked or confined.
            RemoteAction::PointerMotionAbsolute { .. }
            | RemoteAction::PointerMotionRelative { .. }
                if self.pointer_lock.is_engaged() =>
            {
                false
            }
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

    /// Inject one event that already passed the personal-fabric identity and
    /// lease gate. Unlike an app portal session, cross-device control must
    /// still yield to lock, the kill switch, and recent physical input on the
    /// target device.
    pub fn apply_personal_fabric_input(
        &mut self,
        controller: &crate::shell::personal_continuity::DeviceId,
        event: crate::shell::device_fabric::FabricInputEvent,
    ) -> bool {
        let is_release = matches!(
            event,
            crate::shell::device_fabric::FabricInputEvent::PointerButton { pressed: false, .. }
                | crate::shell::device_fabric::FabricInputEvent::Key { pressed: false, .. }
        );
        if !is_release
            && let Some(reason) = self
                .remote_input_firewall
                .host_override(std::time::Instant::now())
        {
            tracing::debug!(?reason, "personal-fabric input yielded to local control");
            return false;
        }
        let action = match event {
            crate::shell::device_fabric::FabricInputEvent::PointerMotion { x, y } => {
                RemoteAction::PointerMotionAbsolute {
                    stream: 0,
                    x: f64::from(x),
                    y: f64::from(y),
                }
            }
            crate::shell::device_fabric::FabricInputEvent::PointerButton { button, pressed } => {
                if !self
                    .personal_fabric_input
                    .button_transition(controller, button, pressed)
                {
                    return false;
                }
                RemoteAction::PointerButton {
                    button: i32::try_from(button).unwrap_or(i32::MAX),
                    pressed,
                }
            }
            crate::shell::device_fabric::FabricInputEvent::Scroll {
                horizontal,
                vertical,
            } => RemoteAction::PointerAxis {
                dx: f64::from(horizontal),
                dy: f64::from(vertical),
            },
            crate::shell::device_fabric::FabricInputEvent::Key {
                keycode, pressed, ..
            } => {
                if !self
                    .personal_fabric_input
                    .key_transition(controller, keycode, pressed)
                {
                    return false;
                }
                RemoteAction::KeyboardKeycode {
                    keycode: i32::try_from(keycode).unwrap_or(i32::MAX),
                    pressed,
                }
            }
        };
        let applied = self.apply_remote_desktop_action(action);
        if !applied {
            match event {
                crate::shell::device_fabric::FabricInputEvent::PointerButton {
                    button,
                    pressed: true,
                } => {
                    let _ = self
                        .personal_fabric_input
                        .button_transition(controller, button, false);
                }
                crate::shell::device_fabric::FabricInputEvent::Key {
                    keycode,
                    pressed: true,
                    ..
                } => {
                    let _ = self
                        .personal_fabric_input
                        .key_transition(controller, keycode, false);
                }
                crate::shell::device_fabric::FabricInputEvent::PointerMotion { .. }
                | crate::shell::device_fabric::FabricInputEvent::PointerButton {
                    pressed: false,
                    ..
                }
                | crate::shell::device_fabric::FabricInputEvent::Scroll { .. }
                | crate::shell::device_fabric::FabricInputEvent::Key { pressed: false, .. } => {}
            }
        }
        applied
    }

    /// Release every key/button actually attributed to one personal-fabric
    /// controller. Cleanup bypasses host overrides because withholding an up
    /// edge after lock, revocation, or disconnect would leave the local seat
    /// in a stuck state.
    pub fn release_personal_fabric_input(
        &mut self,
        controller: &crate::shell::personal_continuity::DeviceId,
    ) -> bool {
        let (keys, buttons) = self.personal_fabric_input.release_controller(controller);
        let mut released = false;
        for keycode in keys {
            let time = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
            let serial = self.next_serial();
            released |= self.inject_seat_key(keycode, false, serial, time);
        }
        for button in buttons {
            let time = u32::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
            let serial = self.next_serial();
            released |= self.remote_pointer_button(
                i32::try_from(button).unwrap_or(i32::MAX),
                false,
                serial,
                time,
            );
        }
        released
    }

    pub fn release_all_personal_fabric_input(&mut self) -> usize {
        let controllers = self.personal_fabric_input.controllers();
        let count = controllers.len();
        for controller in controllers {
            let _ = self.release_personal_fabric_input(&controller);
        }
        count
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

    /// Deliver a Linux evdev keycode press/release to the seat keyboard. Shared by
    /// the `RemoteDesktop` and personal-fabric paths. Convert to Smithay's XKB
    /// keycode convention by adding eight, as the other remote input paths do.
    fn inject_seat_key(&mut self, keycode: u32, pressed: bool, serial: Serial, time: u32) -> bool {
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
            keycode.saturating_add(8).into(),
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
                    // This helper returns evdev, like the keycode portal and
                    // personal-fabric paths. Injection applies the XKB offset.
                    return Some(raw.saturating_sub(8));
                }
            }
            None
        })
    }
}

// `RemoteInputSink` impl: receives the gated events from
// `ShareRegistry::route_to`.
//
// Pointer drives the smithay `Seat<Self>` via `PointerHandle::motion`/
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
    #[allow(
        clippy::too_many_lines,
        reason = "Remote-input dispatch must keep the ordered Smithay serial, focus, and seat mutations together so a peer event cannot observe a partially-applied input state."
    )]
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
                // Motion is sufficient pointer attribution even if an older
                // or compact viewer omitted the explicit enter handshake.
                // Revocation then knows which surface owns a held drag.
                let _ = self.remote_seats.seat_mut(peer).pointer_enter(surface);
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
                                self.remote_input_firewall.view().rect(surface).map(|rect| {
                                    let origin = crate::compositor::foreign::store()
                                        .surface_origin(surface, rect);
                                    (wl_surface, origin.into())
                                })
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
                // A legacy client sees one wl_pointer, while the firewall
                // retains one cursor per participant. Another peer may have
                // moved the shared seat since this peer's last motion, so
                // restore this peer's accepted focus/location before its
                // coordinate-less button edge is delivered.
                self.restore_remote_pointer(peer, surface, serial, time);
                if self.remote_seats.button_transition(peer, button, pressed)
                    && let Some(pointer) = self.seat.get_pointer()
                {
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
                // Old viewers use hover as their focus handshake. Once a
                // viewer uses KeyboardFocus, incidental hover during strip
                // movement must only change pointer attribution.
                let seat = self.remote_seats.seat_mut(peer);
                let _ = seat.pointer_enter(target);
                if !seat.explicit_keyboard_focus {
                    let _ = seat.enter(target);
                }
            }
            InputEvent::PointerLeave { surface: _ } => {
                // Leaving ends pointer attribution only. In particular, do
                // not release keys or deactivate text-input-v3: real apps
                // keep their caret after the pointer moves elsewhere. A
                // pointer grab cannot survive without a valid coordinate,
                // though, so retire this peer's held buttons.
                let _ = self.remote_seats.seat_mut(peer).pointer_leave();
                self.release_remote_pointer_buttons_for_peer(peer);
            }
            InputEvent::KeyboardFocus {
                surface: target,
                focused,
            } => {
                self.remote_seats.seat_mut(peer).explicit_keyboard_focus = true;
                if focused {
                    if self
                        .remote_seats
                        .seat(peer)
                        .is_some_and(|seat| seat.focused_surface != Some(target))
                    {
                        self.release_remote_inputs_for_peer(peer);
                    }
                    let _ = self.remote_seats.seat_mut(peer).enter(target);
                    return;
                }
                // Streams have independent transport queues. A previous
                // window's blur may arrive after this peer focused another
                // window; it must not retire that newer focus or its keys.
                if self
                    .remote_seats
                    .seat(peer)
                    .is_some_and(|seat| seat.focused_surface == Some(target))
                {
                    let _ = self.remote_seats.seat_mut(peer).leave();
                    self.release_remote_inputs_for_peer(peer);
                }
            }
            InputEvent::PointerAxis { dx, dy } => {
                self.restore_remote_pointer(peer, surface, serial, time);
                let _ = self.remote_pointer_axis(dx, dy, time);
            }
            InputEvent::ClipboardText { text } => {
                let _ = crate::compositor::selection::set_mirror_text(
                    &self.selection_store(),
                    crate::compositor::selection::Target::Clipboard,
                    &text,
                );
            }
            InputEvent::Close => self.remote_closes.push(surface),
            InputEvent::Text { text } => {
                self.request_remote_text_commit(text, std::time::Instant::now());
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
                //
                // A press of a key the peer already holds is the sender's
                // auto-repeat: the client sees one more stroke of it. Clients
                // never repeat a remote key themselves, so a release that
                // arrives late cannot leave one typing the key meanwhile.
                let repeat = pressed && self.remote_seats.repeats(peer, keysym);
                if pressed {
                    self.begin_remote_typing();
                }
                let strokes: &[KeyState] = if repeat {
                    &[KeyState::Released, KeyState::Pressed]
                } else if self.remote_seats.key_transition(peer, keysym, pressed) {
                    if pressed {
                        &[KeyState::Pressed]
                    } else {
                        &[KeyState::Released]
                    }
                } else {
                    &[]
                };
                if let Some(keyboard) = self.seat.get_keyboard() {
                    for key_state in strokes {
                        // The compatibility wire's historical field name is
                        // `keysym`, but the value is an evdev keycode. Smithay's
                        // keyboard API consumes XKB keycodes (evdev + 8), exactly
                        // like the local-client path.
                        let _: Option<()> = keyboard.input(
                            self,
                            keysym.saturating_add(8).into(),
                            *key_state,
                            serial,
                            time,
                            |_, _, _| smithay::input::keyboard::FilterResult::<()>::Forward,
                        );
                    }
                }
            }
        }
    }
}

/// Where one message of a legacy window's stream goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyStreamRoute {
    /// The workspace topic of the room the window's tab is in; the active
    /// workspace's when unknown.
    pub topic: Option<u64>,
    /// When every viewer of the window is one of the person's own devices:
    /// exactly those, so the rest of the room receives nothing of it.
    pub only: Option<Vec<MsgPeerId>>,
}

/// Sends one message of a legacy window's stream along its route.
pub type LegacyStreamPublisher =
    Arc<dyn Fn(LegacyStreamRoute, crate::network::NeighbourMsg) -> bool + Send + Sync>;

/// One legacy window to stream: its app, the workspace tab holding it (`0`
/// when the stream names none), and the peers watching it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyShare {
    pub app_id: String,
    pub tab: u64,
    pub viewers: std::collections::HashSet<MsgPeerId>,
    /// Every viewer is a host bridging the window to web members, which
    /// draw a page streamed as a display list: an app that can stream its
    /// page ([`PaintApp`]) is shared that way rather than as video.
    pub paint: bool,
    /// The largest picture each viewer's hardware decoders take, by codec,
    /// where the viewer said: the stream is encoded no larger.
    pub decode_limits:
        HashMap<MsgPeerId, std::collections::BTreeMap<media::StreamCodec, (u32, u32)>>,
}

/// Most bytes of an app's page stream kept for viewers that start later.
const MAX_PAINT_GROUP_BYTES: usize = 8 * 1024 * 1024;

/// An app that streams its window's page as a display list.
#[derive(Debug)]
pub struct PaintApp {
    /// Its `shell-ui` connection, which the stream arrives on.
    pub connection: u64,
    /// Where the page sits in the window (logical x, y, w, h), as the app
    /// last said; viewer input is offset by it.
    pub page: [f64; 4],
    /// The window's buffer scale, for a viewer that maps buffer pixels.
    pub scale: f64,
    /// The latest whole frame and what followed it.
    group: Vec<Vec<u8>>,
    group_bytes: usize,
    /// What the app was last told: whether anyone watches.
    watched: bool,
    /// A new share started: ask for a whole frame.
    keyframe_wanted: bool,
}

impl PaintApp {
    fn new(connection: u64) -> Self {
        Self {
            connection,
            page: [0.0; 4],
            scale: 1.0,
            group: Vec::new(),
            group_bytes: 0,
            watched: false,
            keyframe_wanted: false,
        }
    }
}

/// One frame an app sent of its page stream.
pub struct PaintFrame {
    pub payload: Vec<u8>,
    pub page: [f64; 4],
    pub scale: f64,
}

impl State {
    /// Put the shared pointer back where `peer`'s last accepted motion over
    /// `surface` left it: a legacy client sees one `wl_pointer`, while the
    /// firewall keeps one cursor per participant, and another peer may have
    /// moved the seat since. Coordinate-less edges (a button, the wheel)
    /// land there.
    fn restore_remote_pointer(
        &mut self,
        peer: MsgPeerId,
        surface: SurfaceId,
        serial: Serial,
        time: u32,
    ) {
        let Some((x, y)) = self.remote_input_firewall.peer_cursor_global(peer, surface) else {
            return;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let focus = self
            .surface_handles
            .get(&surface)
            .cloned()
            .and_then(|wl_surface| {
                self.remote_input_firewall.view().rect(surface).map(|rect| {
                    let origin = crate::compositor::foreign::store().surface_origin(surface, rect);
                    (wl_surface, origin.into())
                })
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

    /// The legacy windows remote viewers closed since the last call; the
    /// compositor closes their tabs as a person's close does.
    pub fn take_remote_closes(&mut self) -> Vec<SurfaceId> {
        std::mem::take(&mut self.remote_closes)
    }

    fn refresh_foreign_subsurface_tree(&self, root: &WlSurface) {
        use smithay::wayland::compositor::{TraversalAction, with_surface_tree_upward};

        let root_id = surface_id(root);
        if !self.should_capture_foreign_surface(root_id) {
            return;
        }
        let mut children = Vec::new();
        with_surface_tree_upward(
            root,
            (0_i32, 0_i32),
            |surface, states, parent| {
                let location = if surface == root {
                    *parent
                } else {
                    let offset = states
                        .cached_state
                        .get::<wl_compositor::SubsurfaceCachedState>()
                        .current()
                        .location;
                    (
                        parent.0.saturating_add(offset.x),
                        parent.1.saturating_add(offset.y),
                    )
                };
                TraversalAction::DoChildren(location)
            },
            |surface, _, location| {
                if surface != root {
                    children.push(crate::compositor::foreign::ChildFrame {
                        surface: surface_id(surface),
                        x: location.0,
                        y: location.1,
                    });
                }
            },
            |_, _, _| true,
        );
        crate::compositor::foreign::store().set_children(root_id, children);
    }

    fn capture_committed_foreign_pixels(&mut self, surface: &WlSurface, surface_id: u64) {
        let foreign_root = foreign_root(surface);
        self.note_presentation_commit(
            surface,
            surface_id,
            self.should_capture_foreign_surface(surface_id) || foreign_root.is_some(),
        );
        if let Some(root) = foreign_root {
            self.refresh_foreign_subsurface_tree(&root);
        }
        if !self.is_floating_toplevel(surface_id) {
            Self::note_foreign_window_geometry(surface, surface_id);
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
    }

    fn note_presentation_commit(&mut self, surface: &WlSurface, surface_id: u64, capture: bool) {
        let association = if capture {
            Self::capture_foreign_surface(surface, surface_id)
        } else {
            ForeignCommitCapture::Removed
        };
        let pixels = match association {
            ForeignCommitCapture::Shm(generation) => Some(
                crate::compositor::presentation::PixelCommit::Shm(generation),
            ),
            ForeignCommitCapture::Dmabuf(token) => Some(
                crate::compositor::foreign::store()
                    .capture_seq_for_token(surface_id, token)
                    .map_or(
                        crate::compositor::presentation::PixelCommit::Unavailable,
                        crate::compositor::presentation::PixelCommit::Captured,
                    ),
            ),
            ForeignCommitCapture::Removed | ForeignCommitCapture::Failed => {
                Some(crate::compositor::presentation::PixelCommit::Unavailable)
            }
            ForeignCommitCapture::NoBuffer => None,
        };
        self.presentation_feedback
            .committed(surface, surface_id, pixels);
    }

    fn prepare_pending_foreign_capture(&self, surface: &WlSurface) {
        let surface_id = crate::compositor::state::surface_id(surface);
        // `new_toplevel` registers the protocol surface before adopting it
        // into the shared workspace tree.  A client is allowed to attach its
        // first buffer immediately after acknowledging configure, and that
        // pre-commit hook can therefore run in the narrow interval where the
        // surface is live but its tab lookup has not become observable yet.
        // Treat every registered toplevel as capture-eligible here.  Popup and
        // subsurface objects are registered through different paths, so this
        // does not turn arbitrary child commits into tiled windows.
        let captured_here = self.client_input.is_known(surface_id)
            || foreign_root(surface).is_some()
            || self.osk_surface == Some(surface_id)
            || self.is_floating_toplevel(surface_id)
            || self.is_output_presentation_surface(surface_id)
            || self.is_transient_surface(surface_id);
        if !captured_here {
            forget_uncaptured_shm_base(surface, surface_id);
            return;
        }
        let (association, capture) = wl_compositor::with_states(surface, |states| {
            let viewported = pending_viewport(states);
            let mut attrs = states
                .cached_state
                .get::<wl_compositor::SurfaceAttributes>();
            let buffer = match &attrs.pending().buffer {
                Some(wl_compositor::BufferAssignment::NewBuffer(buffer)) => buffer.clone(),
                Some(wl_compositor::BufferAssignment::Removed) => {
                    return (ForeignCommitCapture::Removed, None);
                }
                None => {
                    // Damage without a freshly captured buffer can refer to
                    // mutated pixels in an existing buffer. Do not certify the
                    // older cached image as this commit's new content.
                    let association = if attrs.pending().damage.is_empty() {
                        ForeignCommitCapture::NoBuffer
                    } else {
                        ForeignCommitCapture::Failed
                    };
                    return (association, None);
                }
            };
            let rects = pending_buffer_rects(attrs.pending(), viewported);
            drop(attrs);
            let damage = rects.as_deref().map(buffer_damage);
            let regions = rects.as_deref().map(shm_regions);
            let Some(client) = surface.client() else {
                return (ForeignCommitCapture::Failed, None);
            };
            if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(&buffer).cloned() {
                let colorimetry = states
                    .data_map
                    .get::<crate::wayland::color_management::ColorManagementSurfaceData>()
                    .and_then(crate::wayland::color_management::ColorManagementSurfaceData::current_colorimetry)
                    .map(|(colorimetry, _)| colorimetry);
                if let Some(capture) = crate::compositor::foreign::store()
                    .prepare_dmabuf_capture_with_damage(
                        surface_id,
                        client,
                        dmabuf,
                        colorimetry,
                        damage,
                    )
                {
                    // `with_states` holds Smithay's PrivateSurfaceData mutex.
                    // `add_blocker` takes that same non-reentrant mutex, so it
                    // must happen after this callback returns. Doing it here
                    // self-deadlocked the compositor on the first GPU-backed
                    // app commit (most visibly Chat).
                    return (ForeignCommitCapture::Dmabuf(capture.token), Some(capture));
                }
                return (ForeignCommitCapture::Failed, None);
            }
            match crate::compositor::foreign::store().capture_shm_commit(
                surface_id,
                &buffer,
                damage,
                regions.as_deref(),
            ) {
                Some(generation) => (ForeignCommitCapture::Shm(generation), None),
                None => (ForeignCommitCapture::Failed, None),
            }
        });
        settle_shm_base(surface_id, &association);
        if let Some(capture) = capture {
            wl_compositor::add_blocker(surface, capture.barrier);
            if let Some(fence) = capture.fence {
                self.watch_dmabuf_fence(fence, surface_id, capture.token);
            }
        }
        if matches!(
            association,
            ForeignCommitCapture::Shm(_) | ForeignCommitCapture::Dmabuf(_)
        ) {
            // A typed key's answer: the window it went to drew again. Outside
            // `with_states`, whose surface mutex `foreign_root` takes too.
            let root = foreign_root(surface).map_or(surface_id, |root| {
                crate::compositor::state::surface_id(&root)
            });
            let unanswered = crate::compositor::key_latency::tracker()
                .surface_committed(root, std::time::Instant::now());
            for app in unanswered {
                eprintln!("KEY-LATENCY app={app} unanswered=1");
            }
        }
        wl_compositor::with_states(surface, |states| {
            states
                .data_map
                .insert_if_missing_threadsafe(ForeignCommitCaptures::default);
            if let Some(commits) = states.data_map.get::<ForeignCommitCaptures>() {
                commits
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(association);
            }
            if matches!(association, ForeignCommitCapture::Shm(_)) {
                mark_shm_capture_base(states);
            }
        });
    }

    /// Take hold of the buffer the commit just applied on `surface` when the
    /// frame it published is drawn from that buffer itself. Left in the
    /// surface state, Smithay would give it back the moment the next commit
    /// applies, while a queued frame can still draw it; held here, it goes
    /// back once the last frame that drew it finished
    /// ([`crate::shell::gpu_frames`]). Runs last in the commit handler, after
    /// everything that reads the applied buffer.
    fn hold_drawn_buffer(surface: &WlSurface, surface_id: u64) {
        let store = crate::compositor::foreign::store();
        let Some(frame) = store.take_unheld_frame(surface_id) else {
            return;
        };
        let buffer = wl_compositor::with_states(surface, |states| {
            take_new_buffer(
                &mut states
                    .cached_state
                    .get::<wl_compositor::SurfaceAttributes>()
                    .current()
                    .buffer,
            )
        });
        if let Some(buffer) = buffer {
            crate::compositor::foreign::hold_buffer(buffer, &frame);
        } else {
            // Never draw from a buffer the client may reuse.
            tracing::warn!(
                target: "shell::compositor::foreign",
                surface_id,
                "a client frame's buffer was not current when its commit applied; not drawing it"
            );
            store.withdraw_unheld_frame(surface_id, &frame);
        }
    }

    /// Start the capture of commit `token` on `surface_id` when its buffer's
    /// implicit fence signals. The loop watches the fence, so no thread waits
    /// on a client's GPU and a slow client holds up only its own commits.
    fn watch_dmabuf_fence(
        &self,
        fence: smithay::backend::allocator::dmabuf::DmabufSource,
        surface_id: u64,
        token: u64,
    ) {
        let watched = self.loop_handle.insert_source(fence, move |(), _, _| {
            crate::compositor::foreign::dmabuf_fence_signalled(surface_id, token);
            Ok(())
        });
        if let Err(error) = watched {
            tracing::warn!(
                target: "shell::compositor::foreign",
                surface_id,
                error = %error.error,
                "could not watch a client buffer's fence; capturing it on the worker"
            );
            crate::compositor::foreign::dmabuf_fence_unwatched(surface_id, token);
        }
    }

    /// Re-evaluate transactions whose off-thread buffer capture completed.
    /// Called by the compositor loop on its owning thread; worker threads only
    /// flip barriers and enqueue owning client handles.
    pub fn pump_foreign_capture_barriers(&mut self, display: &DisplayHandle) -> bool {
        crate::compositor::foreign::give_back_released_buffers();
        let completed = crate::compositor::foreign::drain_completed_dmabuf_captures();
        let changed =
            crate::compositor::foreign::take_published_shm_frames() | !completed.is_empty();
        for client in completed {
            let Some(client_state) = client.get_data::<ClientState>() else {
                continue;
            };
            client_state.compositor_state.blocker_cleared(self, display);
        }
        changed
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
    /// This gates authority-bearing globals: the clipboard-manager protocol
    /// pair (`zwlr_data_control_manager_v1`/`ext_data_control_manager_v1`) and
    /// `ext_session_lock_manager_v1`. The former hands its holder every
    /// selection without focus; the latter controls the compositor's lock
    /// lifecycle. A *socket* privilege, not a flag a client can ask for: the
    /// sandbox launcher bind-mounts only the public socket into a sandbox.
    pub privileged: bool,
    /// Set when the connection was accepted on a trusted input method's
    /// compositor-owned private display. This is the only credential the
    /// input-method and virtual-keyboard global filters accept; an app-id
    /// string is never authority.
    pub input_method: Option<crate::compositor::input_method::TrustedInputMethod>,
    /// Package identity proven from `SO_PEERCRED` and the compositor's live
    /// launcher registry. `None` denotes an explicitly unconfined host client.
    pub package_id: Option<String>,
    /// Durable workspace room from the same host-owned launch record. A client
    /// cannot select or change this through Wayland protocol state.
    pub workspace_id: Option<String>,
    /// Whether package/app-id equality is part of this connection's sandbox
    /// boundary. Unconfined host launches keep their desktop compatibility ids.
    pub enforce_package_id: bool,
    /// The program behind a privileged connection, read from its peer
    /// credentials when it connected. Display changes it makes are kept
    /// without asking once the person trusts this executable.
    pub executable: Option<String>,
    /// Peer pid from `SO_PEERCRED` when the connection was accepted.
    /// Used to recognise xwayland-satellite after its last toplevel dies
    /// while leftover X clients (Wine) still hold the X socket.
    pub pid: Option<u32>,
    /// Set when the connection is a legacy VM's Wayland forwarder: every app
    /// id it announces becomes `legacy.<guest>.<app id>`. Claiming this only
    /// lowers what a window is trusted with, so it needs no proof.
    pub legacy_guest: Option<legacy_agent_protocol::GuestOs>,
    /// Whether the client asked the seat for a `wl_touch`. A touch sequence
    /// sent to a surface of a client without one reaches nothing, so such a
    /// client gets the pointer events touch is emulated as instead.
    pub touch_bound: std::sync::atomic::AtomicBool,
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
            input_method: None,
            package_id: None,
            workspace_id: None,
            enforce_package_id: false,
            executable: None,
            pid: None,
            legacy_guest: None,
            touch_bound: std::sync::atomic::AtomicBool::new(false),
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

    /// Per-client state for an ordinary app whose process ancestry was
    /// authenticated at the public socket boundary.
    pub(crate) fn attributed_arc(
        identity: crate::shell::launcher::VerifiedLaunchIdentity,
        pid: u32,
    ) -> Arc<Self> {
        Arc::new(Self {
            package_id: Some(identity.package_id),
            workspace_id: identity.workspace_id,
            enforce_package_id: identity.confined,
            pid: Some(pid),
            ..Self::default()
        })
    }

    /// Per-client data for a connection accepted on the privileged socket.
    pub fn privileged_arc() -> Arc<Self> {
        Self::privileged_tool_arc(None)
    }

    /// Per-client data for a privileged connection whose program is known.
    pub fn privileged_tool_arc(executable: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            privileged: true,
            executable,
            ..Self::default()
        })
    }

    /// Per-client state for connections accepted on a trusted input
    /// method's private display.
    pub fn input_method_arc(
        kind: crate::compositor::input_method::TrustedInputMethod,
    ) -> Arc<Self> {
        Arc::new(Self {
            input_method: Some(kind),
            ..Self::default()
        })
    }
}

/// The trusted-input-method credential of `client`, if it has one.
fn trusted_input_method(
    client: &Client,
) -> Option<crate::compositor::input_method::TrustedInputMethod> {
    client
        .get_data::<ClientState>()
        .and_then(|d| d.input_method)
}

/// Connection-provenance gate for authority-bearing Wayland protocols.
///
/// A client cannot opt into this bit: only the dedicated privileged socket
/// acceptor constructs this state, while the sandbox launcher exposes only
/// the public socket.
pub(crate) fn is_privileged_client(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|data| data.privileged)
}

/// The compositor's globally unique id for `surface`.
///
/// A Wayland object id is unique only within one client's connection, so
/// two clients' first surfaces share the same `protocol_id`. Every place
/// the compositor stores a surface — tabs, the input router, share
/// registry, frame tracker — keys off one flat `SurfaceId`, avoiding a
/// second client's window overwriting the first's. The id is the client's
/// connection index in the high 32 bits, protocol id in the low 32 — both stable, so it can be recomputed rather than remembered, staying a plain `u64` for the peer-sync wire protocol.
/// The connection index a [`surface_id`] was minted under.
#[must_use]
pub fn client_index_of(surface_id: u64) -> u32 {
    u32::try_from(surface_id >> 32).unwrap_or(u32::MAX)
}

pub fn surface_id(surface: &WlSurface) -> u64 {
    let protocol_id = u64::from(surface.id().protocol_id());
    if let Some(client) = surface.client() {
        let index = client.get_data::<ClientState>().map_or(0, |d| d.index);
        return (u64::from(index) << 32) | protocol_id;
    }
    // A client that exits is torn down after it has gone, so its surfaces
    // keep the id they were known by: that is what their tabs, input
    // routing and frames are filed under.
    if surface
        .data::<smithay::wayland::compositor::SurfaceUserData>()
        .is_some()
        && let Some(id) = wl_compositor::with_states(surface, |states| {
            states.data_map.get::<KnownSurfaceId>().map(|known| known.0)
        })
    {
        return id;
    }
    // Reserving index 0 keeps an unknown dead surface out of every live
    // client's range.
    protocol_id
}

/// The [`surface_id`] a surface had while its client was alive.
struct KnownSurfaceId(u64);

/// Record `surface`'s [`surface_id`] for after its client has gone.
fn remember_surface_id(surface: &WlSurface) -> u64 {
    let id = surface_id(surface);
    wl_compositor::with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(|| KnownSurfaceId(id));
    });
    id
}

/// The Tilo package a toplevel's client was launched as, when the launcher
/// started it confined as that package: the identity comes from the
/// authenticated launch record, never from the app id the client announces.
/// Every other client (a host app, an X11 client, a legacy guest, anything
/// started outside the launcher) has none, and its windows are legacy.
fn launched_package(client: Option<&ClientState>) -> Option<String> {
    let client = client?;
    if client.legacy_guest.is_some() || !client.enforce_package_id {
        return None;
    }
    client.package_id.clone()
}

fn authenticated_toplevel_identity(
    client: Option<&ClientState>,
    announced_app_id: Option<String>,
) -> Result<(Option<String>, Option<crate::shell::state::RoomId>), &'static str> {
    let Some(client) = client else {
        return Ok((announced_app_id, None));
    };
    if let Some(guest) = client.legacy_guest {
        return Ok((
            Some(legacy_agent_protocol::window::host_app_id(
                guest,
                announced_app_id.as_deref(),
            )),
            None,
        ));
    }
    let Some(package_id) = client.package_id.as_deref() else {
        return Ok((announced_app_id, None));
    };
    let room = client
        .workspace_id
        .as_deref()
        .and_then(crate::shell::state::RoomId::parse)
        .ok_or("authenticated app launch has no valid workspace room")?;
    if !client.enforce_package_id {
        return Ok((announced_app_id, Some(room)));
    }
    if announced_app_id
        .as_deref()
        .is_some_and(|app_id| !crate::shell::launcher::app_id_matches_package(app_id, package_id))
    {
        return Err("Wayland app id does not match the authenticated package");
    }
    Ok((Some(package_id.to_string()), Some(room)))
}

/// Take the buffer out of `slot` when it holds a newly attached one, leaving
/// any other assignment in place.
fn take_new_buffer(
    slot: &mut Option<wl_compositor::BufferAssignment>,
) -> Option<smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer> {
    match slot.take() {
        Some(wl_compositor::BufferAssignment::NewBuffer(buffer)) => Some(buffer),
        other => {
            *slot = other;
            None
        }
    }
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
        self.surface_handles
            .insert(remember_surface_id(surface), surface.clone());
        // A dmabuf remains client-owned until wl_buffer.release. Capture it
        // from pending state on a worker, and block this commit from replacing
        // (and releasing) the previous buffer until that read is done.
        wl_compositor::add_pre_commit_hook::<Self, _>(surface, |state, _, surface| {
            state.prepare_pending_foreign_capture(surface);
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        // HiDPI: push this surface's preferred `wp_fractional_scale` — the
        // scale of the *output it is on*, not one session-wide number,
        // since a docked laptop has two different right answers. Runs for
        // every surface so any HiDPI-aware client with a `wp_fractional_scale_v1` object learns its scale. `set_preferred_scale` is idempotent, only emitting a wire event on real change, and stores the value even before the client creates the object (smithay replays it on `get_fractional_scale`).
        // The cache is keyed by [`surface_id`], which includes the client
        // index. Using the bare Wayland object id worked only for the first
        // client and made a second client's surface fall back to the
        // session-wide scale after moving between monitors.
        let surface_id = crate::compositor::state::surface_id(surface);
        let scale = self.surface_scale(surface_id);
        wl_compositor::with_states(surface, |states| {
            fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });
        let session_scale = self.scale;
        self.client_cursor.surface_committed(surface, session_scale);
        self.release_closing_frame_callbacks(surface, surface_id);
        if crate::compositor::toplevel_icon::sync(surface, surface_id) {
            tab_binding::note_tab_artwork_changed(&mut self.workspace_tree.write());
        }

        // Pull the surface's buffer into [`SurfaceTracker`] iff shared on any [`WorkspaceTree`] workspace; most aren't, so `is_surface_shared` short-circuits first.
        let is_shared = {
            let tree = self.workspace_tree.read();
            tree.desktop
                .workspaces
                .iter()
                .any(|ws| ws.is_surface_shared(surface_id))
                || self.active_sessions.contains_key(&surface_id)
        };
        // Foreign-surface compositing: an adopted toplevel's buffer is captured every commit for `ForeignSurfaceWidget` to paint — independent of the share path above.
        self.capture_committed_foreign_pixels(surface, surface_id);
        if !is_shared {
            Self::hold_drawn_buffer(surface, surface_id);
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
        Self::hold_drawn_buffer(surface, surface_id);
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        let id = crate::compositor::state::surface_id(surface);
        let root = foreign_root(surface);
        self.drop_transients_for_surface(id);
        self.surface_handles.remove(&id);
        self.presentation_feedback.destroyed(id);
        self.surface_tracker.forget(id);
        self.unregister_share_session(id);
        crate::compositor::foreign::store().forget(id);
        if let Some(root) = root {
            self.refresh_foreign_subsurface_tree(&root);
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(
        &mut self,
        buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
        self.toplevel_icon_manager.buffer_destroyed(buffer);
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
        // Per-surface caches are populated around commit time (the foreign
        // tile pre-commit worker plus `capture_commit`, and
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

/// Both halves are required. The immutable socket credential prevents a
/// public client from gaining overlay/input authority by copying the package
/// id; the exact id prevents another helper on the private connection from
/// accidentally being promoted.
fn trusted_osk_identity(surface: &WlSurface, app_id: Option<&str>) -> bool {
    osk_identity_is_trusted(app_id, trusted_osk_connection(surface))
}

fn observe_toplevel_startup(surface: &ToplevelSurface) {
    if let Some(pid) = surface
        .wl_surface()
        .client()
        .and_then(|client| client.get_data::<ClientState>().and_then(|state| state.pid))
    {
        crate::shell::launcher::record_startup_window(pid);
    }
}

fn trusted_osk_connection(surface: &WlSurface) -> bool {
    surface
        .client()
        .and_then(|client| trusted_input_method(&client))
        == Some(crate::compositor::input_method::TrustedInputMethod::OnScreenKeyboard)
}

fn osk_identity_is_trusted(app_id: Option<&str>, socket_authorized: bool) -> bool {
    socket_authorized && app_id == Some(OSK_APP_ID)
}

fn floating_capability_token(app_id: &str) -> Option<&str> {
    let token = app_id.strip_prefix(ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX)?;
    (token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(token)
}

/// Map the client-bound `wl_output` object back to the stable backend id;
/// an output that is not one of the compositor's globals maps to nothing.
fn requested_output_id(output: Option<&WlOutput>) -> Option<crate::compositor::backend::OutputId> {
    crate::compositor::output_globals::output_id_of(output?)
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

/// Where a candidate window goes: directly below the text field's caret
/// rectangle (`caret` is relative to the field's surface at `parent`), left
/// edges aligned, the way every desktop input method draws it; flipped above
/// the caret when there is no room below on the output, then kept on it.
fn candidate_rect(
    parent: Rect,
    caret: Rectangle<i32, Logical>,
    (width, height): (u32, u32),
    bounds: Rect,
) -> Rect {
    let x = parent.x0 + f64::from(caret.loc.x);
    let caret_top = parent.y0 + f64::from(caret.loc.y);
    let caret_bottom = caret_top + f64::from(caret.size.h.max(0));
    let (width, height) = (f64::from(width), f64::from(height));
    let y = if caret_bottom + height > bounds.y1 && caret_top - height >= bounds.y0 {
        caret_top - height
    } else {
        caret_bottom
    };
    constrain_transient_rect(Rect::new(x, y, x + width, y + height), bounds)
}

/// Convert client-supplied logical popup geometry without allowing an extreme
/// coordinate plus size to overflow before it reaches the compositor's
/// floating-point scene space.
fn popup_rect_from_parent(parent: Rect, geometry: Rectangle<i32, Logical>) -> Rect {
    Rect::new(
        parent.x0 + f64::from(geometry.loc.x),
        parent.y0 + f64::from(geometry.loc.y),
        parent.x0 + f64::from(geometry.loc.x.saturating_add(geometry.size.w)),
        parent.y0 + f64::from(geometry.loc.y.saturating_add(geometry.size.h)),
    )
}

impl State {
    /// Where surface `id` is on the desktop: its tab or floating placement,
    /// or a popup's place beside the window that opened it.
    pub(crate) fn surface_desktop_rect(&self, id: SurfaceId) -> Option<Rect> {
        self.popup_parent_rect(id, 0)
    }

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
        Some(popup_rect_from_parent(parent, popup.geometry))
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
                    popup_rect_from_parent(parent, geometry),
                ))
            })
            .collect()
    }

    /// Mark the transient surfaces a capture must withhold. A popup is part
    /// of the window that opened it: while that window is protected, so are
    /// its menus and completions. The on-screen keyboard and the input
    /// method's candidate popups show what is being typed, so they are
    /// protected while the focused field declares its content secret (a
    /// password, a PIN or hidden text), and an input method's popup also
    /// while the window it serves is protected.
    ///
    /// Re-derived only when something it depends on changed since the last
    /// pass (see [`CaptureProtectionInputs`]); otherwise a wakeup takes
    /// neither the tree lock nor the store's protection lock.
    pub fn sync_capture_protection(&mut self) {
        let secret_field = self.active_field_content_type().is_sensitive();
        let store = crate::compositor::foreign::store();
        // Loaded before anything they guard is read: a change racing this
        // pass advances them again and is folded in on the next wakeup.
        let versions = (
            self.workspace_tree.generation(),
            store.capture_protection_epoch(),
        );
        if self
            .capture_protection_sync
            .last
            .as_ref()
            .is_some_and(|last| last.matches(self, secret_field, versions))
        {
            return;
        }
        #[cfg(test)]
        {
            self.capture_protection_sync.passes =
                self.capture_protection_sync.passes.saturating_add(1);
        }
        let mut protected = Vec::new();
        if let Some(keyboard) = self.osk_surface {
            protected.push((keyboard, secret_field));
        }
        for (popup, parent) in self.capture_protection_input_method_popups() {
            let parent_protected =
                parent.is_some_and(|parent| self.is_surface_capture_protected(parent));
            protected.push((popup, secret_field || parent_protected));
        }
        // Nested popups chain to their toplevel through every tracked popup,
        // live or not; a malformed graph is bounded exactly as
        // `popup_parent_rect` bounds it.
        let parents: HashMap<SurfaceId, SurfaceId> = self
            .popups
            .iter()
            .map(|popup| (surface_id(popup.surface.wl_surface()), popup.parent))
            .collect();
        for popup in self.popups.iter().filter(|popup| popup.surface.alive()) {
            let mut root = popup.parent;
            for _ in 0..16 {
                match parents.get(&root) {
                    Some(parent) => root = *parent,
                    None => break,
                }
            }
            protected.push((
                surface_id(popup.surface.wl_surface()),
                self.is_surface_capture_protected(root),
            ));
        }
        store.set_surfaces_capture_protected(&protected);
        self.capture_protection_sync.last = Some(CaptureProtectionInputs::capture(
            self,
            secret_field,
            versions,
        ));
    }

    /// Live input-method popups and the surface each one serves.
    fn capture_protection_input_method_popups(
        &self,
    ) -> impl Iterator<Item = (SurfaceId, Option<SurfaceId>)> + '_ {
        self.input_method_popups
            .iter()
            .filter(|popup| popup.alive())
            .map(|popup| {
                (
                    surface_id(popup.wl_surface()),
                    popup.get_parent().map(|parent| surface_id(&parent.surface)),
                )
            })
    }

    /// Every tracked xdg popup: its surface, whether it is alive, and its
    /// parent.
    fn capture_protection_popups(&self) -> impl Iterator<Item = (SurfaceId, bool, SurfaceId)> + '_ {
        self.popups.iter().map(|popup| {
            (
                surface_id(popup.surface.wl_surface()),
                popup.surface.alive(),
                popup.parent,
            )
        })
    }

    /// Passes [`Self::sync_capture_protection`] has made that re-derived
    /// protection rather than finding nothing changed.
    #[cfg(test)]
    pub(crate) const fn capture_protection_passes(&self) -> usize {
        self.capture_protection_sync.passes
    }

    #[doc(hidden)]
    pub fn xdg_popup_count(&self) -> usize {
        self.popups.len()
    }

    #[doc(hidden)]
    pub fn popup_surfaces_for_tests(&self) -> Vec<(SurfaceId, Rect)> {
        self.popup_surfaces()
    }

    /// Remember a connection accepted on a trusted input method's display.
    pub fn register_trusted_input_method_client(
        &mut self,
        client: &Client,
        kind: crate::compositor::input_method::TrustedInputMethod,
    ) {
        self.trusted_input_method_clients.push((client.id(), kind));
    }

    /// The trusted input method kind that owns the seat.
    pub fn input_method_owner_kind(
        &self,
    ) -> Option<crate::compositor::input_method::TrustedInputMethod> {
        self.seat_input_method.get()
    }

    /// Hand the seat's input method to `owner` (or nobody). Connections of
    /// any other trusted kind are disconnected on the spot, releasing the
    /// single `input-method-v2` slot before the new owner can ask for it; a
    /// dying process cannot keep the seat.
    pub fn set_input_method_owner(
        &mut self,
        owner: Option<crate::compositor::input_method::TrustedInputMethod>,
    ) {
        if self.seat_input_method.get() != owner {
            self.cancel_active_dictation();
        }
        self.seat_input_method.set(owner);
        let handle = self.display_handle.clone();
        self.trusted_input_method_clients.retain(|(id, kind)| {
            if handle.backend_handle().get_client_data(id.clone()).is_err() {
                return false;
            }
            if crate::compositor::input_method::SeatInputMethod::keeps(*kind, owner) {
                return true;
            }
            handle
                .backend_handle()
                .kill_client(id.clone(), DisconnectReason::ConnectionClosed);
            false
        });
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
                let size = crate::compositor::foreign::store()
                    .frame_size(surface_id(popup.wl_surface()))?;
                let output = self.popup_constraint_output(parent_id);
                let bounds = Rect::new(
                    f64::from(output.loc.x),
                    f64::from(output.loc.y),
                    f64::from(output.loc.x.saturating_add(output.size.w)),
                    f64::from(output.loc.y.saturating_add(output.size.h)),
                );
                Some((
                    surface_id(popup.wl_surface()),
                    candidate_rect(parent_rect, popup.text_input_rectangle(), size, bounds),
                ))
            })
            .collect()
    }

    /// [`Self::input_method_popup_surfaces`] for integration tests.
    pub fn input_method_popup_surfaces_for_tests(&self) -> Vec<(SurfaceId, Rect)> {
        self.input_method_popup_surfaces()
    }

    /// The xdg popups mapped over a window, in the order they opened.
    pub fn xdg_popups(&self) -> impl Iterator<Item = &PopupSurface> + '_ {
        self.popups.iter().map(|popup| &popup.surface)
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

impl State {
    /// Mint the standard one-shot token immediately before a user-selected
    /// host launch. Only the launched process receives its opaque value.
    #[doc(hidden)]
    pub fn issue_host_launch_activation(
        &mut self,
        desktop_id: String,
        display_name: String,
        placement: crate::compositor::tab_binding::Placement,
    ) -> String {
        self.expire_host_launch_activations();
        let (token, _) = self.xdg_activation_state.create_external_token(None);
        let token = token.as_str().to_string();
        self.host_launch_activations.insert(
            token.clone(),
            HostLaunchActivation {
                desktop_id,
                display_name,
                placement,
                issued: std::time::Instant::now(),
            },
        );
        token
    }

    pub(crate) fn cancel_host_launch_activation(&mut self, token: &str) {
        self.host_launch_activations.remove(token);
        let _ = self
            .xdg_activation_state
            .remove_token(&XdgActivationToken::from(token.to_string()));
    }

    /// Expire compositor-created launch tokens even when no subsequent launch
    /// occurs. Smithay owns the protocol store; Tilo owns the placement
    /// metadata and retires both together after the same bounded age.
    pub(crate) fn expire_host_launch_activations(&mut self) {
        let now = std::time::Instant::now();
        let stale: Vec<_> = self
            .host_launch_activations
            .iter()
            .filter_map(|(token, launch)| {
                (now.saturating_duration_since(launch.issued)
                    > crate::compositor::activation::MAX_ACTIVATION_AGE)
                    .then_some((token.clone(), launch.desktop_id.clone()))
            })
            .collect();
        for (token, desktop_id) in stale {
            self.cancel_host_launch_activation(&token);
            crate::shell::launcher::clear_pending_placement(&desktop_id);
        }
    }
}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        use crate::compositor::activation::{ActivationFacts, evaluate_token_creation};

        let now = std::time::Instant::now();
        self.activation_serials.retain(|(_, _, issued)| {
            now.saturating_duration_since(*issued)
                <= crate::compositor::activation::MAX_ACTIVATION_AGE
        });
        let source_id = data.surface.as_ref().map(surface_id);
        let serial_belongs_to_source = data.serial.as_ref().is_some_and(|(serial, _)| {
            self.activation_serials
                .iter()
                .any(|(issued, recipient, _)| issued == serial && Some(*recipient) == source_id)
        });
        let seat_is_owned = data
            .serial
            .as_ref()
            .is_some_and(|(_, seat)| self.seat.owns(seat));
        let source_exists = source_id.is_some_and(|id| self.client_input.is_known(id));
        let source_belongs_to_creator = data
            .surface
            .as_ref()
            .and_then(Resource::client)
            .zip(data.client_id.as_ref())
            .is_some_and(|(client, creator)| client.id() == *creator);
        let source_was_focused = source_id.is_some_and(|id| {
            self.client_input.keyboard_focus() == Some(id)
                || self.client_input.pointer_focus() == Some(id)
        });
        let facts = ActivationFacts {
            age: now.saturating_duration_since(data.timestamp),
            serial_belongs_to_source,
            seat_is_owned,
            source_exists,
            source_belongs_to_creator,
            source_was_focused,
        };
        match evaluate_token_creation(facts) {
            Ok(()) => true,
            Err(reason) => {
                tracing::debug!(
                    target: "shell::compositor::activation",
                    ?reason,
                    source_id,
                    app_id = data.app_id.as_deref(),
                    "refusing xdg-activation token"
                );
                false
            }
        }
    }

    #[allow(
        clippy::collapsible_if,
        reason = "The nested host-launch branch keeps the one-shot activation token's relocation decision visually separate from its post-relocation label mutation."
    )]
    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        use crate::compositor::activation::{app_ids_match, evaluate_activation_request};

        let host_launch = self.host_launch_activations.remove(token.as_str());
        // A token is a one-shot launch capability. Consume before evaluating
        // the target so neither an accepted request nor probing a wrong app id
        // can replay it later against another window.
        let _ = self.xdg_activation_state.remove_token(&token);
        let target = surface_id(&surface);
        let actual_app_id = {
            let tree = self.workspace_tree.read();
            tab_binding::app_id_for_surface(&tree, target).map(str::to_owned)
        };
        let app_id_matches_target = data.app_id.as_deref().is_none_or(|requested| {
            actual_app_id
                .as_deref()
                .is_some_and(|actual| app_ids_match(requested, actual))
        });
        if let Some(host_launch) = host_launch.as_ref() {
            crate::shell::launcher::clear_pending_placement(&host_launch.desktop_id);
            if host_launch.issued.elapsed() > crate::compositor::activation::MAX_ACTIVATION_AGE {
                return;
            }
        }
        let result = evaluate_activation_request(
            data.timestamp.elapsed(),
            self.client_input.is_known(target) && actual_app_id.is_some(),
            app_id_matches_target,
        );
        if let Err(reason) = result {
            tracing::debug!(
                target: "shell::compositor::activation",
                ?reason,
                target,
                requested_app_id = data.app_id.as_deref(),
                actual_app_id,
                "refusing xdg-activation focus request"
            );
            return;
        }
        if let Some(host_launch) = host_launch {
            let relocated = {
                let mut tree = self.workspace_tree.write();
                tab_binding::relocate_foreign_toplevel(
                    &mut tree,
                    target,
                    host_launch.placement,
                    &host_launch.display_name,
                )
            };
            if relocated {
                self.host_launch_surface_labels
                    .insert(target, host_launch.display_name.clone());
                tracing::debug!(target, desktop_id = %host_launch.desktop_id, "placed host launch through activation token");
            }
        }
        let activated = {
            let mut tree = self.workspace_tree.write();
            tab_binding::activate_surface(&mut tree, target)
        };
        if activated {
            self.set_client_keyboard_focus(Some(target));
            tracing::debug!(
                target: "shell::compositor::activation",
                target,
                "accepted single-use xdg-activation focus request"
            );
        }
    }
}

impl XdgDialogHandler for State {
    fn modal_changed(&mut self, toplevel: ToplevelSurface, is_modal: bool) {
        let surface_id = surface_id(toplevel.wl_surface());
        crate::compositor::foreign::store().set_sheet_modal(surface_id, is_modal);
    }
}

impl State {
    /// Whether `parent` is something a dialog can be shown on: a tab, or a
    /// sheet already on one.
    fn can_host_sheet(&self, parent: SurfaceId) -> bool {
        crate::compositor::foreign::store().is_sheet(parent) || {
            let tree = self.workspace_tree.read();
            tab_binding::group_id_for_surface(&tree, parent).is_some()
        }
    }

    /// Bring `surface`'s place in the desktop in line with its xdg parent:
    /// a sheet on the parent's tile while it has one, a tab otherwise.
    fn sync_toplevel_parent(&mut self, surface: &ToplevelSurface) {
        let id = surface_id(surface.wl_surface());
        if self.osk_surface == Some(id)
            || self.is_floating_toplevel(id)
            || self.is_output_presentation_surface(id)
        {
            return;
        }
        let store = crate::compositor::foreign::store();
        let was_sheet = store.is_sheet(id);
        let parent = surface
            .parent()
            .map(|parent| surface_id(&parent))
            .filter(|parent| self.can_host_sheet(*parent));
        let Some(parent) = parent else {
            if was_sheet {
                store.detach_sheet(id);
                self.adopt_former_sheet(surface);
            }
            return;
        };
        let modal = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().map(|data| data.modal))
                .unwrap_or(false)
        });
        if !store.attach_sheet(id, parent, modal) {
            tracing::warn!(surface_id = id, parent, "refused a cyclic xdg parent");
            return;
        }
        if was_sheet {
            return;
        }
        // Adopted as a tab when it was created (the parent arrives after
        // `get_toplevel`); a dialog is part of its parent's window, so that
        // provisional tab goes.
        let _ = {
            let mut tree = self.workspace_tree.write();
            tab_binding::close_tab_for_destroyed_surface(&mut tree, id)
        };
        self.client_input.promote_to_overlay(id);
        let root = store.sheet_root(id);
        // Unlike a tab, the parent's tile does not size a sheet: the dialog
        // picks its own size, bounded by the tile it sits on.
        let scale = self.surface_scale(root);
        let bounds = store.placement(root).map(|rect| {
            (
                logical_extent(rect.width(), scale),
                logical_extent(rect.height(), scale),
            )
        });
        surface.with_pending_state(|pending| {
            pending.size = None;
            pending.bounds = bounds.map(Into::into);
        });
        tracing::info!(
            target: "shell::compositor::foreign",
            surface_id = id,
            parent,
            root,
            modal,
            "showing transient toplevel as a sheet on its parent's tile"
        );
    }

    /// A sheet whose parent went away is a window in its own right, per
    /// xdg-shell; adopt it as a tab in the active workspace.
    fn adopt_former_sheet(&self, surface: &ToplevelSurface) {
        let id = surface_id(surface.wl_surface());
        let announced = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        let client = surface.wl_surface().client();
        let client_state = client
            .as_ref()
            .and_then(|client| client.get_data::<ClientState>());
        let package = launched_package(client_state);
        let identity = authenticated_toplevel_identity(client_state, announced);
        let Ok((app_id, room)) = identity else {
            surface.send_close();
            return;
        };
        let adopted = {
            let mut tree = self.workspace_tree.write();
            let tab = if let Some(room) = room {
                tab_binding::adopt_toplevel_in_room(&mut tree, id, app_id, room)
            } else {
                Some(tab_binding::adopt_toplevel(&mut tree, id, app_id))
            };
            if let (Some(tab), Some(package)) = (tab, package) {
                tree.remember_package_launch(tab, package);
            }
            drop(tree);
            tab.is_some()
        };
        if !adopted {
            surface.send_close();
        }
    }

    /// A toplevel is going away: its own sheet link goes, and every sheet
    /// that hung off it follows. A sheet whose parent was itself a sheet
    /// moves down onto that sheet's parent; one whose parent was the tile is
    /// asked to close with it, and becomes a tab meanwhile so a dialog that
    /// ignores the close is still reachable.
    fn release_sheets_of_destroyed(&self, id: SurfaceId) {
        let store = crate::compositor::foreign::store();
        let grandparent = store.detach_sheet(id).map(|link| link.parent);
        let orphans = store.sheet_children(id);
        if orphans.is_empty() {
            return;
        }
        let toplevels: Vec<ToplevelSurface> = self.xdg_shell_state.toplevel_surfaces().to_vec();
        for orphan in orphans {
            let Some(toplevel) = toplevels
                .iter()
                .find(|toplevel| surface_id(toplevel.wl_surface()) == orphan)
            else {
                store.detach_sheet(orphan);
                continue;
            };
            if let Some(grandparent) = grandparent {
                let modal = store.sheet_link(orphan).is_some_and(|link| link.modal);
                let _ = store.attach_sheet(orphan, grandparent, modal);
                continue;
            }
            toplevel.send_close();
            store.detach_sheet(orphan);
            self.adopt_former_sheet(toplevel);
        }
    }

    /// Ask every sheet on `root`'s tile to close — the user closed the
    /// window they belong to.
    pub(crate) fn close_sheets_over(&mut self, root: SurfaceId) {
        let sheets = crate::compositor::foreign::store().sheets_over(root);
        let closing: Vec<ToplevelSurface> = self
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .filter(|toplevel| {
                let id = surface_id(toplevel.wl_surface());
                sheets.iter().any(|link| link.surface == id)
            })
            .cloned()
            .collect();
        for toplevel in closing {
            self.ask_toplevel_to_close(&toplevel);
        }
    }

    /// Wayland peer pids that currently own at least one `xdg_toplevel`.
    /// xwayland-satellite keeps leftover X clients (Wine) after the last
    /// window; this is how the supervisor knows the user already closed it.
    pub(crate) fn mapped_xwayland_pids(&self) -> std::collections::HashSet<u32> {
        let mut pids = std::collections::HashSet::new();
        for toplevel in self.xdg_shell_state.toplevel_surfaces() {
            let Some(client) = toplevel.wl_surface().client() else {
                continue;
            };
            let Some(pid) = client.get_data::<ClientState>().and_then(|data| data.pid) else {
                continue;
            };
            pids.insert(pid);
        }
        pids
    }
}

/// `px` physical pixels as whole surface-local (logical) pixels at `scale`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to [1.0, i32::MAX] and rounded before the cast, so it neither truncates a fraction nor wraps"
)]
fn logical_extent(px: f64, scale: f64) -> i32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (px / scale).round().clamp(1.0, f64::from(i32::MAX)) as i32
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn client_pong(&mut self, client: ShellClient) {
        self.shell_pongs.note(client);
    }

    fn client_destroyed(&mut self, client: ShellClient) {
        self.shell_pongs.forget(&client);
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
        crate::compositor::foreign::note_client_pump();
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        let authenticated_launch_key = surface.wl_surface().client().and_then(|client| {
            client
                .get_data::<ClientState>()
                .and_then(|data| data.package_id.clone())
        });
        if let Some(launch_key) = authenticated_launch_key.as_deref() {
            crate::shell::launcher::attribute_surface_launch(surface_id, launch_key);
        }
        let announced_app_id = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        let (identity, package) = if let Some(client) = surface.wl_surface().client() {
            let client_state = client.get_data::<ClientState>();
            (
                authenticated_toplevel_identity(client_state, announced_app_id),
                launched_package(client_state),
            )
        } else {
            (
                authenticated_toplevel_identity(None, announced_app_id),
                None,
            )
        };
        let Ok((app_id, attributed_room)) = identity else {
            tracing::warn!(
                surface_id,
                "rejected unauthenticated Wayland toplevel identity"
            );
            surface.send_close();
            return;
        };
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
        let attributed_room_log = attributed_room;
        let app_id_log = app_id.clone();
        if trusted_osk_identity(surface.wl_surface(), app_id.as_deref()) {
            self.client_input.promote_to_overlay(surface_id);
            self.osk_surface = Some(surface_id);
            self.reannounce_text_input_focus();
        } else if app_id.is_none() && trusted_osk_connection(surface.wl_surface()) {
            // get_toplevel precedes set_app_id. The immutable private-socket
            // role already rules out an ordinary window, so never adopt a
            // temporary tab or move the person's focus. Overlay authority
            // still waits for the exact OSK app-id.
            self.client_input.promote_to_overlay(surface_id);
        } else if !floating && !presentation {
            if let Some(room) = attributed_room {
                let adopted = {
                    let mut tree = self.workspace_tree.write();
                    let tab =
                        tab_binding::adopt_toplevel_in_room(&mut tree, surface_id, app_id, room);
                    if let (Some(tab), Some(package)) = (tab, package) {
                        tree.remember_package_launch(tab, package);
                    }
                    tab
                };
                if adopted.is_none() {
                    tracing::warn!(surface_id, %room, "rejected Wayland toplevel for a stale workspace");
                    self.client_input.unregister_toplevel(surface_id);
                    surface.send_close();
                    return;
                }
            } else {
                let mut tree = self.workspace_tree.write();
                let tab = tab_binding::adopt_toplevel(&mut tree, surface_id, app_id);
                if let Some(package) = package {
                    tree.remember_package_launch(tab, package);
                }
            }
        }

        tracing::info!(
            target: "shell::compositor::foreign",
            surface_id,
            app_id = ?app_id_log,
            room = ?attributed_room_log,
            floating,
            presentation,
            "accepted Wayland toplevel"
        );

        // Start the clock the deferred opening configure is bounded against — adoption is when the wait becomes worth giving up on.
        crate::compositor::foreign::store().note_adopted(surface_id);
        observe_toplevel_startup(&surface);
        if let Some(app_id) = app_id_log.as_deref() {
            crate::shell::launcher::record_host_window_open(surface_id, app_id);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        crate::compositor::foreign::note_client_pump();
        // Remove the matching `Foreign` tab so the chrome stops advertising a destroyed surface.
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        let launch = crate::shell::launcher::launch_key_for_surface(surface_id);
        let asked_to_close = self.closing_toplevels.remove(&surface_id).is_some();
        eprintln!(
            "window-destroyed: surface={surface_id} launch={} asked_to_close={asked_to_close}",
            launch.as_deref().unwrap_or("-")
        );
        let person_reached_it = self
            .clients_given_input
            .contains(&client_index_of(surface_id));
        if let Some(pid) = surface
            .wl_surface()
            .client()
            .and_then(|client| client.get_data::<ClientState>().and_then(|state| state.pid))
            && !self
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .any(|other| {
                    other.wl_surface() != surface.wl_surface()
                        && other.wl_surface().client().and_then(|client| {
                            client.get_data::<ClientState>().and_then(|state| state.pid)
                        }) == Some(pid)
                })
        {
            crate::shell::launcher::note_native_writer_retiring(pid);
        }
        crate::shell::launcher::note_surface_gone(surface_id, asked_to_close || person_reached_it);
        self.release_sheets_of_destroyed(surface_id);
        crate::compositor::toplevel_icon::forget(surface_id);
        if self.osk_surface == Some(surface_id) {
            self.osk_surface = None;
        }
        let _ = self.remove_output_presentation_surface(surface_id);
        if self
            .presenter_pin
            .as_ref()
            .is_some_and(|pin| pin.presenter == surface_id)
        {
            self.release_presenter_pin();
        }
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
        let fullscreen_group_matches = {
            let tree = self.workspace_tree.read();
            self.fullscreen_group == tab_binding::group_id_for_surface(&tree, surface_id)
        };
        if fullscreen_group_matches {
            self.fullscreen_group = None;
            self.fullscreen_output = None;
        }
        if self
            .fullscreen_target
            .as_ref()
            .is_some_and(|target| target.surface_id == surface_id)
        {
            self.fullscreen_target = None;
        }
        let _ = {
            let mut tree = self.workspace_tree.write();
            tab_binding::close_tab_for_destroyed_surface_and_persist(&mut tree, surface_id)
        };
        self.host_launch_surface_labels.remove(&surface_id);
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
        crate::compositor::foreign::note_client_pump();
        // Clients call `set_app_id` *after* `get_toplevel` fires
        // `new_toplevel`, so the tab was adopted with a synthetic
        // `anon-<surface_id>` placeholder. Re-read the now-populated app-id and rewrite the tab in-place so the chrome (and the integration test) see the populated one.
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        let new_app_id = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|d| d.app_id.clone()))
        });
        let identity = if let Some(client) = surface.wl_surface().client() {
            authenticated_toplevel_identity(client.get_data::<ClientState>(), new_app_id)
        } else {
            authenticated_toplevel_identity(None, new_app_id)
        };
        let Ok((Some(app_id), _)) = identity else {
            tracing::warn!(surface_id, "ignored unauthenticated Wayland app-id change");
            return;
        };
        if trusted_osk_connection(surface.wl_surface()) && app_id != OSK_APP_ID {
            // A pending private keyboard cannot become an ordinary app by
            // announcing a different identity after get_toplevel.
            surface.send_close();
            return;
        }
        {
            if trusted_osk_identity(surface.wl_surface(), Some(&app_id)) {
                // `set_app_id` normally arrives after `new_toplevel`; undo the
                // temporary anonymous adoption before either tree can expose a
                // keyboard as an ordinary app window.
                let _ = {
                    let mut tree = self.workspace_tree.write();
                    tab_binding::close_tab_for_destroyed_surface(&mut tree, surface_id)
                };
                self.client_input.promote_to_overlay(surface_id);
                self.osk_surface = Some(surface_id);
                self.reannounce_text_input_focus();
                return;
            }
            if self.promote_floating_toplevel(surface_id, &app_id) {
                // `new_toplevel` may have inserted an anonymous placeholder
                // before the app-id arrived. Remove it before the next tree
                // mirror can expose a one-frame phantom tab.
                let _ = {
                    let mut tree = self.workspace_tree.write();
                    tab_binding::close_tab_for_destroyed_surface(&mut tree, surface_id)
                };
                return;
            }
            if is_output_presentation_app_id(&app_id)
                && self.promote_output_presentation_toplevel(surface_id, app_id.clone())
            {
                let provisional_group = {
                    let tree = self.workspace_tree.read();
                    tab_binding::group_id_for_surface(&tree, surface_id)
                };
                // The provisional anonymous tab must never share the
                // presenter's workspace with the clean audience surface.
                let _ = {
                    let mut tree = self.workspace_tree.write();
                    tab_binding::close_tab_for_destroyed_surface(&mut tree, surface_id)
                };
                // `set_fullscreen` is permitted to arrive before
                // `set_app_id`; undo that provisional group promotion now
                // that the toplevel has become output-scoped.
                if self.fullscreen_group == provisional_group {
                    self.fullscreen_group = None;
                    self.fullscreen_output = None;
                }
                return;
            }
            if crate::compositor::foreign::store().is_sheet(surface_id) {
                // A sheet has no tab to rename; the id is read again should it ever become one.
                return;
            }
            let host_launch_title = self
                .host_launch_surface_labels
                .get(&surface_id)
                .map(String::as_str);
            let _ = {
                let mut tree = self.workspace_tree.write();
                tab_binding::rebind_toplevel_for_app_id(
                    &mut tree,
                    surface_id,
                    &app_id,
                    host_launch_title,
                )
            };
            crate::shell::launcher::record_host_window_open(surface_id, &app_id);
            // Mirror the rename onto the chrome-side tab the input router mounted (or its still-pending adoption).
            self.client_input.note_app_id(surface_id, app_id);
        }
    }

    /// `xdg_toplevel.set_parent`: a toplevel with a parent is a dialog of
    /// that window, shown as a sheet on its tile; losing the parent makes
    /// it an ordinary tab again.
    fn parent_changed(&mut self, surface: ToplevelSurface) {
        crate::compositor::foreign::note_client_pump();
        self.sync_toplevel_parent(&surface);
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        crate::compositor::foreign::note_client_pump();
        let surface_id = surface_id(surface.wl_surface());
        let title = wl_compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|data| data.title.clone()))
        });
        if let Some(title) = title {
            let mut tree = self.workspace_tree.write();
            let _ = tab_binding::rename_tab_for_title(&mut tree, surface_id, &title);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        crate::compositor::foreign::note_client_pump();
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
        if let Err(error) = self
            .popup_manager
            .track_popup(PopupKind::from(surface.clone()))
        {
            tracing::debug!(?error, parent, "could not register popup grab tree");
        }
        if let Err(error) = surface.send_configure() {
            tracing::warn!(
                target: "shell::compositor::popup",
                parent,
                error = %error,
                "could not send initial popup configure"
            );
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        if !self.seat.owns(&seat) {
            return;
        }
        let popup_kind = PopupKind::from(surface.clone());
        let Ok(root) = find_popup_root_surface(&popup_kind) else {
            return;
        };
        let Ok(grab) = self
            .popup_manager
            .grab_popup(root, popup_kind, &self.seat, serial)
        else {
            return;
        };
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Clear);
        }
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
        crate::compositor::foreign::note_client_pump();
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
        crate::compositor::foreign::note_client_pump();
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
            // The audience may just have named a different screen than the
            // one it was given on promotion; the presenter view follows.
            self.reconcile_presenter_pin();
        } else {
            // A client's own fullscreen request takes the slot; the presenter
            // placement it displaces is not restored over it.
            if self.presenter_pin.is_some() {
                self.release_presenter_pin();
            }
            self.pending_presentation_outputs
                .insert(surface_id, requested);
            self.fullscreen_group = {
                let tree = self.workspace_tree.read();
                tab_binding::group_id_for_surface(&tree, surface_id)
            };
            self.fullscreen_output = requested;
            self.fullscreen_target = self.fullscreen_group.map(|_| FullscreenTarget {
                surface_id,
                requested_output: wl_output.clone(),
                surface: surface.clone(),
                configured_size: None,
                pinned_output: None,
            });
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
        crate::compositor::foreign::note_client_pump();
        let surface_id = crate::compositor::state::surface_id(surface.wl_surface());
        if self.is_output_presentation_surface(surface_id) {
            let _ = self.remove_output_presentation_surface(surface_id);
        }
        // An app that just opened its audience surface drops its own
        // single-screen fullscreen next; the presenter view stays on the
        // screen it was moved to, sized to that screen.
        if let Some(size) = self.pinned_presenter_size(surface_id) {
            surface.with_pending_state(|state| {
                state.states.unset(
                    smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
                );
                state.size = Some(size.into());
            });
            surface.send_configure();
            return;
        }
        // Only clear if THIS surface was the fullscreen one — a stale request from a background toplevel must not steal another's.
        let fullscreen_group_matches = {
            let tree = self.workspace_tree.read();
            self.fullscreen_group == tab_binding::group_id_for_surface(&tree, surface_id)
        };
        if fullscreen_group_matches {
            self.fullscreen_group = None;
            self.fullscreen_output = None;
            self.fullscreen_target = None;
        }
        // The tab's size was withheld while fullscreen; the next configure
        // pass hands it back.
        crate::compositor::foreign::store().resend_configure(surface_id);
        surface.with_pending_state(|state| {
            state.states.unset(
                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
            );
            state.size = None;
        });
        surface.send_configure();
    }
}

/// Who draws the frame around a toplevel. On Tilo, the compositor —
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
        crate::compositor::foreign::note_client_pump();
        Self::decorate(&toplevel);
    }

    /// The client has stated a preference; answered with the same mode as always, including `ClientSide` — the request this exists to refuse.
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        crate::compositor::foreign::note_client_pump();
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {
        self.cancel_active_dictation();
    }

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        // A window another device of the person points at shapes that
        // device's cursor, not this one's.
        if self.personal_pointer_hold {
            return;
        }
        let scale = self.scale;
        self.client_cursor.set(image, scale);
    }
}

// Tablet tools may carry a cursor shape too; with no tablet-specific cursor
// they share the pointer's.
impl TabletSeatHandler for State {}

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: InputMethodPopupSurface) {
        crate::compositor::foreign::note_client_pump();
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
        // The protocol id is only unique within a client connection.  Use the
        // same connection-qualified key as `commit` and `update_surface_scales`
        // so a second client cannot inherit the first client's cached output
        // scale when both happen to allocate the same wl_surface id.
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
delegate_xdg_activation!(State);
delegate_xdg_shell!(State);
delegate_xdg_dialog!(State);
// `zxdg_decoration_manager_v1`: how a client is told the compositor draws chrome, rather than left to guess. See `XdgDecorationHandler` above.
delegate_xdg_decoration!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
// `wl_seat` is Smithay's, with one observation added: whether each client
// asked for a `wl_touch` (see `ClientState::touch_bound`). The rest of the
// seat (pointer, keyboard, touch objects) is delegated unchanged.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [
    WlSeat: smithay::wayland::seat::SeatGlobalData<State>
] => SeatState<State>);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer: smithay::wayland::seat::PointerUserData<State>
] => SeatState<State>);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_server::protocol::wl_keyboard::WlKeyboard: smithay::wayland::seat::KeyboardUserData<State>
] => SeatState<State>);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_server::protocol::wl_touch::WlTouch: smithay::wayland::seat::TouchUserData<State>
] => SeatState<State>);

impl
    smithay::reexports::wayland_server::Dispatch<
        WlSeat,
        smithay::wayland::seat::SeatUserData<State>,
    > for State
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WlSeat,
        request: smithay::reexports::wayland_server::protocol::wl_seat::Request,
        data: &smithay::wayland::seat::SeatUserData<State>,
        dhandle: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        if matches!(
            request,
            smithay::reexports::wayland_server::protocol::wl_seat::Request::GetTouch { .. }
        ) && let Some(client_state) = client.get_data::<ClientState>()
        {
            client_state
                .touch_bound
                .store(true, std::sync::atomic::Ordering::Release);
        }
        <SeatState<State> as smithay::reexports::wayland_server::Dispatch<
            WlSeat,
            smithay::wayland::seat::SeatUserData<State>,
            State,
        >>::request(state, client, resource, request, data, dhandle, data_init);
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &WlSeat,
        data: &smithay::wayland::seat::SeatUserData<State>,
    ) {
        <SeatState<State> as smithay::reexports::wayland_server::Dispatch<
            WlSeat,
            smithay::wayland::seat::SeatUserData<State>,
            State,
        >>::destroyed(state, client, resource, data);
    }
}
// Text input is dispatched in `compositor::text_commit`, which follows each
// field's declared content type before handing the request to Smithay.
// The input-method manager is Smithay's. `zwp_input_method_v2` is too, with
// the on-screen keyboard's commits counted on the way (`State::osk_commits`).
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2:
    smithay::wayland::input_method::InputMethodManagerGlobalData
] => InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2: ()
] => InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2:
    smithay::wayland::input_method::InputMethodKeyboardUserData<State>
] => InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2:
    smithay::wayland::input_method::InputMethodPopupSurfaceUserData
] => InputMethodManagerState);

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
        smithay::wayland::input_method::InputMethodUserData<State>,
    > for State
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
        request: smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::Request,
        data: &smithay::wayland::input_method::InputMethodUserData<State>,
        dhandle: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::Request;
        if matches!(request, Request::CommitString { .. })
            && client.get_data::<ClientState>().is_some_and(|data| {
                data.input_method
                    == Some(crate::compositor::input_method::TrustedInputMethod::OnScreenKeyboard)
            })
        {
            state.osk_commits = state.osk_commits.saturating_add(1);
        }
        <InputMethodManagerState as smithay::reexports::wayland_server::Dispatch<
            smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
            smithay::wayland::input_method::InputMethodUserData<State>,
            State,
        >>::request(state, client, resource, request, data, dhandle, data_init);
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
        data: &smithay::wayland::input_method::InputMethodUserData<State>,
    ) {
        <InputMethodManagerState as smithay::reexports::wayland_server::Dispatch<
            smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
            smithay::wayland::input_method::InputMethodUserData<State>,
            State,
        >>::destroyed(state, client, resource, data);
    }
}
// The virtual-keyboard manager is Smithay's. `zwp_virtual_keyboard_v1` is
// too, but while one of the shell's own fields is served (see
// `compositor::shell_text_input`) its keys also reach the chrome, which holds
// no `wl_keyboard` Smithay could send them to.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1:
    smithay::wayland::virtual_keyboard::VirtualKeyboardManagerGlobalData
] => VirtualKeyboardManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1: ()
] => VirtualKeyboardManagerState);

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<State>,
    > for State
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        request: smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Request,
        data: &smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<State>,
        dhandle: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Request;
        match &request {
            Request::Keymap { fd, size, .. } => {
                if let Some(keymap) = read_virtual_keymap(fd, *size) {
                    state.virtual_keymaps.insert(resource.id(), keymap);
                }
            }
            Request::Key { key, state: key_state, .. } if state.shell_field_active => {
                if let Some(event) = state
                    .virtual_keymaps
                    .get(&resource.id())
                    .and_then(|keymap| {
                        let keysym = smithay::input::keyboard::xkb::State::new(keymap)
                            .key_get_one_sym(smithay::input::keyboard::Keycode::new(
                                key.saturating_add(8),
                            ));
                        crate::compositor::shell_text_input::translate_key(
                            keysym.raw(),
                            *key_state == 1,
                        )
                    })
                {
                    state.shell_field_keys.push(event);
                }
            }
            _ => {}
        }
        <VirtualKeyboardManagerState as smithay::reexports::wayland_server::Dispatch<
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
            smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<State>,
            State,
        >>::request(state, client, resource, request, data, dhandle, data_init);
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        data: &smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<State>,
    ) {
        state.virtual_keymaps.remove(&resource.id());
        <VirtualKeyboardManagerState as smithay::reexports::wayland_server::Dispatch<
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
            smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<State>,
            State,
        >>::destroyed(state, client, resource, data);
    }
}

/// Compile the XKB text keymap a virtual keyboard uploaded, read without
/// moving the descriptor Smithay reads after us.
fn read_virtual_keymap(
    fd: &std::os::fd::OwnedFd,
    size: u32,
) -> Option<smithay::input::keyboard::xkb::Keymap> {
    use smithay::input::keyboard::xkb;
    use std::os::unix::fs::FileExt as _;
    // A keymap is text of a few kilobytes; refuse anything absurd.
    let size = usize::try_from(size).ok().filter(|size| *size <= 1 << 20)?;
    let file = std::fs::File::from(fd.try_clone().ok()?);
    let mut bytes = vec![0_u8; size];
    file.read_exact_at(&mut bytes, 0).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_string(
        &context,
        text.trim_end_matches('\0').to_owned(),
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
}
delegate_pointer_gestures!(State);
delegate_cursor_shape!(State);
delegate_relative_pointer!(State);
delegate_pointer_constraints!(State);
delegate_keyboard_shortcuts_inhibit!(State);
delegate_content_type!(State);
// `zwp_tablet_manager_v2`, as Smithay's `delegate_tablet_manager!` wires it
// except that manager requests pass through `TabletSeatTracker` first, which
// is how the compositor knows which clients take tablet input themselves.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_manager_v2::ZwpTabletManagerV2: ()
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_manager_v2::ZwpTabletManagerV2: ()
] => crate::compositor::tablet::TabletSeatTracker);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_seat_v2::ZwpTabletSeatV2: smithay::wayland::tablet_manager::TabletSeatUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_tool_v2::ZwpTabletToolV2: smithay::wayland::tablet_manager::TabletToolUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_v2::ZwpTabletV2: smithay::wayland::tablet_manager::TabletUserData
] => TabletManagerState);
delegate_output!(State);
delegate_fractional_scale!(State);
delegate_viewporter!(State);
delegate_idle_inhibit!(State);
impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.inhibit(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.uninhibit(&surface);
    }
}

// Selection: clipboard, primary selection, drag-and-drop. Handler impls live in `compositor::selection`; only dispatch wiring belongs here.
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_data_control!(State);
delegate_ext_data_control!(State);

// `ext_session_lock_v1`: a client can lock the session; while locked the
// compositor hides all normal client surfaces + routes input only to the
// lock surface (enforced by the native run loop, which owns the
// authoritative lock + overlay). Tilo renders its own lock UI rather than a
// client lock surface, so the handler bridges only protocol lock requests
// into that state via [`SessionLockHooks`]. Protocol-object release is not an
// authentication event.
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
        // Tilo paints and authenticates its own lock screen. A client may
        // release the ext-session-lock protocol object, but that is not proof
        // of authentication and must never clear the compositor's lock.
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        // Tilo renders its own lock overlay, so a client lock surface is accepted (no error/hang) but not composited; configured to the output size.
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
        popup_rect_from_parent(parent, geometry)
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
    fn popup_constraints_are_parent_relative_and_saturating() {
        // Phase `popup_constraints_are_relative_to_the_parent_not_global_origin`.
        {
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
        // Phase `popup_constraints_preserve_requested_size_when_slide_can_fit_it`.
        {
            let positioner = right_anchored_positioner((40, 30), (95, 5));
            let geometry = constrained_popup_geometry(
                positioner,
                (0, 0),
                Rectangle::new((0, 0).into(), (100, 100).into()),
            );
            assert_eq!(geometry.loc.x, 60);
            assert_eq!(geometry.size, (40, 30).into());
        }
        // Phase `popup_scene_rect_saturates_extreme_client_geometry`.
        {
            let parent = Rect::new(10.0, 20.0, 110.0, 120.0);
            let geometry = Rectangle::new((i32::MAX - 4, i32::MAX - 2).into(), (64, 64).into());

            let rect = popup_rect_from_parent(parent, geometry);

            assert_eq!(rect.x1, parent.x0 + f64::from(i32::MAX));
            assert_eq!(rect.y1, parent.y0 + f64::from(i32::MAX));
            assert!(rect.x1.is_finite() && rect.y1.is_finite());
        }
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::too_many_lines,
        reason = "The single raster fixture keeps normal and scaled popup geometry in one scene contract; its fixed, viewport-bounded coordinates are explicitly checked before rendering."
    )]
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
            .join("../../../target/popup-ime-geometry-fixtures");
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
    fn only_a_confined_package_launch_names_a_windows_package() {
        let room = crate::shell::state::RoomId::new();
        let launched = |confined| {
            ClientState::attributed_arc(
                crate::shell::launcher::VerifiedLaunchIdentity {
                    package_id: "so.tilo.files".into(),
                    workspace_id: Some(room.to_string()),
                    confined,
                },
                42,
            )
        };
        assert_eq!(
            launched_package(Some(&launched(true))).as_deref(),
            Some("so.tilo.files")
        );
        // A host app the launcher started unconfined keeps its desktop id
        // and is a legacy window, whatever the id says.
        assert_eq!(launched_package(Some(&launched(false))), None);
        // A client nobody launched, announcing a Tilo package's id.
        assert_eq!(launched_package(Some(&ClientState::default())), None);
        assert_eq!(launched_package(None), None);
        let guest = ClientState {
            package_id: Some("so.tilo.files".into()),
            workspace_id: Some(room.to_string()),
            enforce_package_id: true,
            legacy_guest: Some(legacy_agent_protocol::GuestOs::Windows),
            ..ClientState::default()
        };
        assert_eq!(launched_package(Some(&guest)), None);
    }

    #[test]
    fn osk_and_floating_identity_trust_only_exact_credentials() {
        // Phase `only_private_socket_plus_exact_app_id_is_trusted`.
        assert!(osk_identity_is_trusted(Some(OSK_APP_ID), true));
        assert!(!osk_identity_is_trusted(Some(OSK_APP_ID), false));
        assert!(!osk_identity_is_trusted(Some("so.tilo.lookalike"), true));
        assert!(!osk_identity_is_trusted(None, true));
        // Phase `public_clients_never_receive_the_osk_global_credential`.
        {
            use crate::compositor::input_method::TrustedInputMethod;
            assert_eq!(ClientState::default().input_method, None);
            assert_eq!(
                ClientState::input_method_arc(TrustedInputMethod::OnScreenKeyboard).input_method,
                Some(TrustedInputMethod::OnScreenKeyboard)
            );
        }
        // Phase `launcher_attribution_overrides_missing_app_id_and_rejects_spoofing`.
        {
            let room = crate::shell::state::RoomId::new();
            let client = ClientState::attributed_arc(
                crate::shell::launcher::VerifiedLaunchIdentity {
                    package_id: "so.tilo.notes".into(),
                    workspace_id: Some(room.to_string()),
                    confined: true,
                },
                42,
            );
            assert_eq!(
                authenticated_toplevel_identity(Some(&client), None),
                Ok((Some("so.tilo.notes".into()), Some(room)))
            );
            assert!(
                authenticated_toplevel_identity(
                    Some(&client),
                    Some("so.tilo.file-explorer".into())
                )
                .is_err()
            );
        }
        // Phase `stale_workspace_attribution_fails_closed_but_host_clients_remain_compatible`.
        {
            let attributed = ClientState::attributed_arc(
                crate::shell::launcher::VerifiedLaunchIdentity {
                    package_id: "so.tilo.notes".into(),
                    workspace_id: Some("not-a-room".into()),
                    confined: true,
                },
                42,
            );
            assert!(authenticated_toplevel_identity(Some(&attributed), None).is_err());
            assert_eq!(
                authenticated_toplevel_identity(
                    Some(&ClientState::default()),
                    Some("org.example.HostApp".into())
                ),
                Ok((Some("org.example.HostApp".into()), None))
            );
            let legacy = ClientState {
                legacy_guest: Some(legacy_agent_protocol::GuestOs::Linux),
                ..ClientState::default()
            };
            assert_eq!(
                authenticated_toplevel_identity(Some(&legacy), Some("org.gnome.Calculator".into())),
                Ok((Some("legacy.linux.org.gnome.Calculator".into()), None)),
                "a legacy VM's window is named after its guest"
            );
            assert_eq!(
                authenticated_toplevel_identity(Some(&legacy), None),
                Ok((Some("legacy.linux.window".into()), None))
            );
            let room = crate::shell::state::RoomId::new();
            let host = ClientState::attributed_arc(
                crate::shell::launcher::VerifiedLaunchIdentity {
                    package_id: "host.desktop-entry".into(),
                    workspace_id: Some(room.to_string()),
                    confined: false,
                },
                42,
            );
            assert_eq!(
                authenticated_toplevel_identity(Some(&host), Some("LegacyStartupWMClass".into())),
                Ok((Some("LegacyStartupWMClass".into()), Some(room)))
            );
        }
        // Phase `floating_overlay_identity_requires_a_full_random_capability`.
        {
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
                floating_capability_token("so.tilo.shell-float.predictable"),
                None
            );
            assert_eq!(floating_capability_token("so.tilo.chat"), None);
        }
    }

    #[test]
    fn candidates_sit_below_the_caret_and_flip_above_at_the_output_edge() {
        let output = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let field = Rect::new(100.0, 200.0, 900.0, 1000.0);
        let caret = Rectangle::new((40, 30).into(), (2, 20).into());
        assert_eq!(
            candidate_rect(field, caret, (300, 40), output),
            Rect::new(140.0, 250.0, 440.0, 290.0)
        );
        let fits_exactly = Rectangle::new((40, 820).into(), (2, 20).into());
        assert_eq!(
            candidate_rect(field, fits_exactly, (300, 40), output),
            Rect::new(140.0, 1040.0, 440.0, 1080.0),
            "just enough room below: still below"
        );
        let near_bottom = Rectangle::new((40, 830).into(), (2, 20).into());
        assert_eq!(
            candidate_rect(field, near_bottom, (300, 40), output),
            Rect::new(140.0, 990.0, 440.0, 1030.0),
            "no room below: flipped above the caret"
        );
        let near_right = Rectangle::new((1780, 30).into(), (2, 20).into());
        assert_eq!(
            candidate_rect(field, near_right, (300, 40), output),
            Rect::new(1620.0, 250.0, 1920.0, 290.0),
            "kept on the output horizontally"
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

    #[test]
    fn a_paint_app_streams_its_page_to_web_viewers_instead_of_video() {
        let mut state = make_state();
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&sent);
        state.set_legacy_stream_publisher(Arc::new(move |_, message| {
            sink.lock().unwrap().push(message);
            true
        }));
        state.note_paint_app("so.tilo.browser", 5);
        // As reconciliation starts it for a host bridging to web members.
        let session = Arc::new(
            SharedFlatpakSession::start_paint(1, "so.tilo.browser", 42, 800, 600, &cfg())
                .expect("start"),
        );
        session.sync_viewers(&[9].into_iter().collect());
        state.register_share_session(Arc::clone(&session));
        assert_eq!(
            state.paint_watch_changes(),
            vec![(5, true)],
            "the app is asked for a page"
        );
        assert!(state.paint_watch_changes().is_empty(), "and asked once");

        state.receive_paint_frame(
            "so.tilo.browser",
            PaintFrame {
                payload: vec![0, 7],
                page: [10.0, 20.0, 780.0, 560.0],
                scale: 2.0,
            },
        );
        let messages = sent.lock().unwrap().clone();
        assert!(messages.iter().any(|m| matches!(
            m,
            crate::network::NeighbourMsg::SurfaceStreamStart { codec, surface: 42, .. }
                if codec == "tilo-paint"
        )));
        assert!(
            messages.iter().any(|m| matches!(
                m,
                crate::network::NeighbourMsg::SurfaceFrame { surface: 42, encoded_h264, .. }
                    if encoded_h264 == &vec![0, 7]
            )),
            "the app's frame goes out as it came: {messages:?}"
        );
        // Input in page pixels lands where the page sits in the window.
        assert_eq!(
            state.paint_point_to_surface(42, InputEvent::PointerMotion { x: 5.0, y: 5.0 }),
            InputEvent::PointerMotion { x: 15.0, y: 25.0 }
        );
        // An app that is not streaming ignores its frames' origin.
        state.receive_paint_frame(
            "org.example.Other",
            PaintFrame {
                payload: vec![0],
                page: [0.0; 4],
                scale: 1.0,
            },
        );
        assert_eq!(sent.lock().unwrap().len(), messages.len());

        state.unregister_share_session(42);
        assert_eq!(
            state.paint_watch_changes(),
            vec![(5, false)],
            "nobody watches any more"
        );
        state.drop_paint_connection(5);
        assert!(state.paint_apps.is_empty());
    }
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
            encoder: EncoderChoice::Software,
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
            refresh_mhz: None,
            logical_position: position,
            identity: crate::compositor::backend::OutputIdentity::default(),
        }
    }

    #[test]
    fn surface_scale_follows_the_output_it_sits_on() {
        // Phase `without_an_output_table_the_scale_is_the_session_wide_one`.
        // Before any output table arrives (hosted/headless run), the session-wide scale is the only answer there is.
        {
            let mut state = make_state();
            state.scale = 1.75;
            assert_eq!(state.scale_for_origin((0.0, 0.0)), 1.75);
            assert_eq!(state.surface_scale(7), 1.75);
        }
        // Phase `packed_nonzero_client_surface_id_keeps_its_own_scale`.
        {
            let mut state = make_state();
            let id = (17_u64 << 32) | 0x002a;
            state.surface_scales.insert(id, 1.5);
            assert_eq!(state.surface_scale(id), 1.5);
            assert_ne!(state.surface_scale(42), 1.5);
        }
        // Phase `a_surface_is_told_the_scale_of_the_output_it_sits_on`.
        // The docked-laptop case: a dense built-in panel beside a sparse external monitor — one session-wide number is wrong for one of them.
        {
            let mut state = make_state();
            state.scale = 1.0;
            state.set_output_layout(&[
                out(0, (0, 0), (1920, 1080), 1.5),
                out(1, (1920, 0), (2560, 1440), 1.0),
            ]);
            assert_eq!(state.scale_for_origin((100.0, 100.0)), 1.5);
            assert_eq!(state.scale_for_origin((2000.0, 100.0)), 1.0);
        }
        // Phase `fullscreen_target_uses_requested_output_dimensions_and_rehomes_on_hotplug`.
        {
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
        // Phase `a_fullscreen_configure_is_logical_like_a_tiled_one`.
        {
            // A tile's configure is its pixels over its scale; a fullscreen one
            // asking a 2x client for all 3840 pixels would make it render a
            // buffer twice the output's size.
            let mut state = make_state();
            state.set_output_layout(&[out(0, (0, 0), (3840, 2160), 2.0)]);
            state.fullscreen_output = Some(crate::compositor::backend::OutputId(0));
            assert_eq!(state.fullscreen_output_size(), Some((1920, 1080)));
        }
        // Phase `moving_a_window_between_outputs_changes_its_recorded_scale`.
        // A window dragged to the external monitor has to re-render at its density — the cache is what makes that observable to the commit hook.
        {
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
        // Phase `an_unplaced_surface_loses_its_cached_scale`.
        // A surface no longer placed must not keep a stale scale — it would be handed back unrecomputed on re-placement.
        {
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
        // Phase `a_new_output_layout_drops_the_cached_scales`.
        // A hotplug invalidates every cached scale, since the layout the values were derived from is what moved.
        {
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
    }

    #[test]
    fn audience_role_requires_an_exact_presenter_and_its_own_output() {
        // Phase `audience_identity_is_narrow_and_does_not_match_the_presenter`.
        assert!(is_output_presentation_app_id("so.tilo.slides.audience"));
        assert!(!is_output_presentation_app_id("so.tilo.slides"));
        assert!(!is_output_presentation_app_id(".audience"));
        // Phase `audience_role_requires_an_exact_same_client_presenter`.
        {
            let mut tree = WorkspaceTree::bootstrap_default();
            tab_binding::adopt_toplevel(
                &mut tree,
                (9_u64 << 32) | 1,
                Some("so.tilo.slides".into()),
            );
            assert_eq!(
                output_presentation_owner(&tree, (9_u64 << 32) | 2, "so.tilo.slides.audience"),
                Some((9_u64 << 32) | 1)
            );
            assert!(
                output_presentation_owner(&tree, (10_u64 << 32) | 2, "so.tilo.slides.audience")
                    .is_none()
            );
            assert!(
                output_presentation_owner(&tree, (9_u64 << 32) | 2, "so.tilo.evil.audience")
                    .is_none()
            );
        }
        // Phase `audience_surface_hides_when_its_distinct_output_is_unplugged`.
        {
            let mut state = make_state();
            state.set_output_layout(&[
                out(1, (0, 0), (1280, 720), 1.0),
                out(2, (1280, 0), (1920, 1080), 1.0),
            ]);
            let presenter_groups = state
                .workspace_tree
                .read()
                .desktop
                .active_workspace()
                .unwrap()
                .strips[0]
                .window_groups
                .len();
            state
                .output_presentation_surfaces
                .push(OutputPresentationSurface {
                    surface_id: 77,
                    app_id: "so.tilo.slides.audience".into(),
                    output: crate::compositor::backend::OutputId(2),
                    rect: Rect::ZERO,
                });
            state.set_output_layout(&[out(1, (0, 0), (1280, 720), 1.0)]);
            let audience = &state.output_presentation_surfaces[0];
            assert_eq!(audience.output, crate::compositor::backend::OutputId(2));
            assert_eq!(audience.rect, Rect::ZERO);
            assert_eq!(
                state
                    .workspace_tree
                    .read()
                    .desktop
                    .active_workspace()
                    .unwrap()
                    .strips[0]
                    .window_groups
                    .len(),
                presenter_groups,
                "hotplug never moves the presenter into the audience layer"
            );
        }
        // Phase `audience_teardown_releases_its_output_and_focus_only`.
        {
            let mut state = make_state();
            state
                .output_presentation_surfaces
                .push(OutputPresentationSurface {
                    surface_id: 88,
                    app_id: "so.tilo.slides.audience".into(),
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
    }

    #[test]
    fn audience_surface_is_gated_scoped_and_published_narrowly() {
        // Phase `commit_capture_gate_includes_audience_surface_without_a_workspace_tab`.
        {
            let mut state = make_state();
            let id = (31_u64 << 32) | 9;
            state
                .output_presentation_surfaces
                .push(OutputPresentationSurface {
                    surface_id: id,
                    app_id: "so.tilo.slides.audience".into(),
                    output: crate::compositor::backend::OutputId(2),
                    rect: Rect::new(100.0, 0.0, 200.0, 100.0),
                });
            assert!(state.should_capture_foreign_surface(id));
            assert!(tab_binding::group_id_for_surface(&state.workspace_tree.read(), id).is_none());
        }
        // Phase `unpaired_audience_is_rejected_by_the_actual_promotion_gate`.
        {
            let mut state = make_state();
            state.set_output_layout(&[out(2, (0, 0), (1920, 1080), 1.0)]);
            assert!(!state.promote_output_presentation_toplevel(
                (44_u64 << 32) | 2,
                "so.tilo.evil.audience".into()
            ));
            assert!(state.output_presentation_surfaces.is_empty());
        }
        // Phase `audience_hit_and_focus_are_scoped_before_workspace_rows`.
        {
            let mut state = make_state();
            state
                .output_presentation_surfaces
                .push(OutputPresentationSurface {
                    surface_id: (8_u64 << 32) | 5,
                    app_id: "so.tilo.slides.audience".into(),
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
        // Phase `audience_remote_view_includes_only_live_direct_surface_tiles`.
        {
            let mut state = make_state();
            let live = (3_u64 << 32) | 7;
            state
                .output_presentation_surfaces
                .push(OutputPresentationSurface {
                    surface_id: live,
                    app_id: "so.tilo.slides.audience".into(),
                    output: crate::compositor::backend::OutputId(2),
                    rect: Rect::new(1280.0, 0.0, 1920.0, 720.0),
                });
            state
                .output_presentation_surfaces
                .push(OutputPresentationSurface {
                    surface_id: live + 1,
                    app_id: "so.tilo.slides.audience".into(),
                    output: crate::compositor::backend::OutputId(3),
                    rect: Rect::new(1920.0, 0.0, 2560.0, 720.0),
                });
            assert_eq!(
                state.output_presentation_remote_tiles_for(|id| id == live),
                vec![(live, Rect::new(1280.0, 0.0, 1920.0, 720.0))]
            );
        }
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

    #[test]
    fn tick_hook_encodes_one_frame_per_commit_until_unregistered() {
        // Phase `two_commits_produce_two_encoded_frames`.
        // Two commits with a share session registered must produce two encoded frames — `encode_next_frame` pumps once per commit into the per-subscriber queue.
        {
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
        // Phase `commit_without_active_session_is_noop`.
        // A commit for a surface with no registered share session is a no-op (no encoder pumped, no substream traffic).
        {
            let mut state = make_state();
            state
                .surface_tracker
                .deposit_for_test(999, vec![0; 64], 4, 4);
            assert!(!state.tick_share_session_for_surface(999));
        }
        // Phase `commit_without_staged_frame_returns_false`.
        // A commit with a registered session but no staged frame returns `false` — the damage-only path (cursor motion, null-attach).
        {
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
        // Phase `unregister_stops_the_tick_hook`.
        // `unregister_share_session` stops the tick hook from pumping.
        {
            const HOST: PeerId = 0xAAAA;
            const VIEWER: PeerId = 0xBBBB;
            const SURFACE: SurfaceId = 77;

            let session = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
            );
            session.add_viewer(VIEWER, false);
            let mut state = make_state();
            state.register_share_session(session);
            let _ = state.remote_seats.seat_mut(VIEWER).enter(SURFACE);
            assert!(state.remote_seats.key_transition(VIEWER, 42, true));
            state
                .surface_tracker
                .deposit_for_test(SURFACE, vec![0xFFu8; 32 * 32 * 4], 32, 32);
            assert!(state.tick_share_session_for_surface(SURFACE));
            state.unregister_share_session(SURFACE);
            assert_eq!(
                state
                    .remote_seats
                    .seat(VIEWER)
                    .and_then(|seat| seat.focused_surface),
                None,
                "ending the share clears the revoked viewer's caret focus"
            );
            assert!(
                !state.remote_seats.key_transition(VIEWER, 42, false),
                "ending the share already consumed the held modifier"
            );
            // Post-unregister the tick is a no-op even with a staged frame.
            state
                .surface_tracker
                .deposit_for_test(SURFACE, vec![0xFFu8; 32 * 32 * 4], 32, 32);
            assert!(!state.tick_share_session_for_surface(SURFACE));
        }
    }

    /// A shared window that resizes gets a new encoder, but stays shared:
    /// receivers learn it from the new start alone, and its viewer keeps
    /// driving it. An end first had a host bridging the window to web
    /// members forget it, and refuse their input, until the start came.
    #[test]
    fn a_resized_shared_window_is_restarted_without_ending_its_stream() {
        const HOST: PeerId = 0xAAAA;
        const VIEWER: PeerId = 0xBBBB;
        const SURFACE: SurfaceId = 0x7e57_0000_0091;
        let mut state = make_state();
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&sent);
        state.set_legacy_stream_publisher(Arc::new(move |_, message| {
            sink.lock().unwrap().push(message);
            true
        }));
        let frames = crate::compositor::foreign::store();
        frames.publish_rgba(SURFACE, &[0x80; 32 * 32 * 4], 32, 32);
        let desired = HashMap::from([(
            SURFACE,
            LegacyShare {
                app_id: "app".into(),
                viewers: [VIEWER].into_iter().collect(),
                ..LegacyShare::default()
            },
        )]);
        state.reconcile_legacy_share_sessions(HOST, &desired, &cfg());
        let _ = state.remote_seats.seat_mut(VIEWER).enter(SURFACE);
        sent.lock().unwrap().clear();

        frames.publish_rgba(SURFACE, &[0x80; 48 * 32 * 4], 48, 32);
        state.reconcile_legacy_share_sessions(HOST, &desired, &cfg());
        let messages = std::mem::take(&mut *sent.lock().unwrap());
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, crate::network::NeighbourMsg::SurfaceStreamEnd { .. })),
            "the window is still shared: {messages:?}"
        );
        assert!(messages.iter().any(|m| matches!(
            m,
            crate::network::NeighbourMsg::SurfaceStreamStart {
                surface: SURFACE,
                width: 48,
                ..
            }
        )));
        assert_eq!(
            state
                .remote_seats
                .seat(VIEWER)
                .and_then(|seat| seat.focused_surface),
            Some(SURFACE),
            "its viewer keeps its focus"
        );

        state.reconcile_legacy_share_sessions(HOST, &HashMap::new(), &cfg());
        assert!(
            sent.lock().unwrap().iter().any(|m| matches!(
                m,
                crate::network::NeighbourMsg::SurfaceStreamEnd {
                    surface: SURFACE,
                    ..
                }
            )),
            "a window no longer shared ends its stream"
        );
        frames.forget(SURFACE);
    }

    #[test]
    fn delayed_blur_of_previous_stream_preserves_current_remote_focus() {
        use crate::network::input_firewall::ShellView;
        use crate::network::streaming::StreamingShare;
        use vello::kurbo::Rect;

        const PEER: PeerId = 9;
        const OLD: SurfaceId = 81;
        const CURRENT: SurfaceId = 82;
        const SHIFT: u32 = 42;
        let mut state = make_state();
        for surface in [OLD, CURRENT] {
            state
                .share_registry
                .insert(PEER, surface, StreamingShare::new());
        }
        state.remote_input_firewall.publish_view(ShellView::new(
            vec![
                (OLD, Rect::new(0.0, 0.0, 400.0, 600.0)),
                (CURRENT, Rect::new(400.0, 0.0, 800.0, 600.0)),
            ],
            Some(CURRENT),
            false,
            false,
        ));
        for surface in [OLD, CURRENT] {
            assert!(state.apply_remote_input(
                PEER,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: true
                }
            ));
        }
        assert!(state.apply_remote_input(
            PEER,
            CURRENT,
            InputEvent::Key {
                keysym: SHIFT,
                pressed: true,
                modifiers: 0
            }
        ));
        assert!(state.apply_remote_input(
            PEER,
            OLD,
            InputEvent::KeyboardFocus {
                surface: OLD,
                focused: false
            }
        ));
        assert_eq!(
            state.remote_seats.seat(PEER).unwrap().focused_surface,
            Some(CURRENT),
            "the previous stream's late blur cannot cancel the newly focused stream"
        );
        assert_eq!(
            state.remote_seats.key_holder_count(SHIFT),
            1,
            "the previous stream's blur cannot release a key held on the current stream"
        );
        assert!(state.apply_remote_input(
            PEER,
            CURRENT,
            InputEvent::Key {
                keysym: 30,
                pressed: true,
                modifiers: 0
            }
        ));
        assert!(state.apply_remote_input(
            PEER,
            CURRENT,
            InputEvent::KeyboardFocus {
                surface: CURRENT,
                focused: false
            }
        ));
        assert_eq!(state.remote_seats.seat(PEER).unwrap().focused_surface, None);
        assert_eq!(state.remote_seats.key_holder_count(SHIFT), 0);
    }

    #[test]
    fn stream_viewers_are_disclosed_for_their_whole_lifecycle() {
        // Phase `quiet_viewer_is_disclosed_for_the_entire_stream_lifecycle`.
        {
            const HOST: PeerId = 0xAAAA;
            const VIEWER: PeerId = 0xBBBB;
            const SURFACE: SurfaceId = 79;

            let session = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
            );
            session.add_viewer(VIEWER, true);
            let mut state = make_state();

            state.register_share_session(session);
            assert_eq!(
                state.share_registry.disclosure(),
                vec![(VIEWER, SURFACE, false)],
                "view-only disclosure begins when the stream is registered, without waiting for input"
            );

            state.unregister_share_session(SURFACE);
            assert!(
                state.share_registry.disclosure().is_empty(),
                "ending the stream removes its trusted disclosure immediately"
            );
        }
        // Phase `replacing_a_stream_cannot_leave_its_old_viewer_disclosed`.
        {
            const HOST: PeerId = 0xAAAA;
            const OLD: PeerId = 0xBBBB;
            const NEW: PeerId = 0xCCCC;
            const SURFACE: SurfaceId = 80;

            let old = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("old"),
            );
            old.add_viewer(OLD, false);
            let new = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("new"),
            );
            new.add_viewer(NEW, true);
            let mut state = make_state();

            state.register_share_session(old);
            state.register_share_session(new);
            assert_eq!(
                state.share_registry.disclosure(),
                vec![(NEW, SURFACE, false)]
            );
        }
        // Phase `revoking_one_viewer_releases_focus_without_clearing_another_modifier_hold`.
        {
            const SURFACE: SurfaceId = 78;
            const FIRST: PeerId = 0xA1;
            const SECOND: PeerId = 0xB2;
            const LEFT_SHIFT: u32 = 42;

            let mut state = make_state();
            let _ = state.remote_seats.seat_mut(FIRST).enter(SURFACE);
            let _ = state.remote_seats.seat_mut(SECOND).enter(SURFACE);
            assert!(state.remote_seats.key_transition(FIRST, LEFT_SHIFT, true));
            assert!(
                !state.remote_seats.key_transition(SECOND, LEFT_SHIFT, true),
                "the legacy app sees one shared modifier press"
            );

            state.release_remote_focus_for_surface(FIRST, SURFACE);
            assert_eq!(
                state
                    .remote_seats
                    .seat(FIRST)
                    .and_then(|seat| seat.focused_surface),
                None
            );
            assert_eq!(
                state
                    .remote_seats
                    .seat(SECOND)
                    .and_then(|seat| seat.focused_surface),
                Some(SURFACE)
            );
            assert!(
                state.remote_seats.key_transition(SECOND, LEFT_SHIFT, false),
                "the remaining holder owns the eventual app-facing release"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn viewer_downgrade_or_removal_retires_held_input() {
        // Phase `downgrading_live_viewer_to_view_only_retires_held_input`.
        {
            const HOST: PeerId = 0xAAAA;
            const VIEWER: PeerId = 0xBBBB;
            const SURFACE: SurfaceId = 81;
            const SHIFT: u32 = 42;

            let session = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
            );
            session.add_viewer(VIEWER, false);
            let mut state = make_state();
            state.register_share_session(Arc::clone(&session));
            let _ = state.remote_seats.seat_mut(VIEWER).enter(SURFACE);
            assert!(state.remote_seats.key_transition(VIEWER, SHIFT, true));

            session.set_read_only(VIEWER, true);
            state.sync_share_authority(&session);

            assert_eq!(
                state.share_registry.disclosure(),
                vec![(VIEWER, SURFACE, false)]
            );
            assert_eq!(
                state
                    .remote_seats
                    .seat(VIEWER)
                    .and_then(|seat| seat.focused_surface),
                None,
                "view-only transition revokes the remote caret/input focus"
            );
            assert!(
                !state.remote_seats.key_transition(VIEWER, SHIFT, false),
                "permission downgrade already retired the held modifier"
            );
        }
        // Phase `packet_fast_path_retires_input_when_it_observes_a_downgrade_first`.
        {
            const HOST: PeerId = 0xAAAA;
            const VIEWER: PeerId = 0xBBBB;
            const SURFACE: SurfaceId = 83;
            const SHIFT: u32 = 42;

            let session = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
            );
            session.add_viewer(VIEWER, false);
            let mut state = make_state();
            state.register_share_session(Arc::clone(&session));
            let _ = state.remote_seats.seat_mut(VIEWER).enter(SURFACE);
            assert!(state.remote_seats.key_transition(VIEWER, SHIFT, true));

            session.set_read_only(VIEWER, true);
            assert!(
                !state.apply_network_remote_input(
                    VIEWER,
                    SURFACE,
                    InputEvent::Key {
                        keysym: SHIFT,
                        pressed: false,
                        modifiers: 0,
                    },
                ),
                "the downgraded release packet is refused after cleanup"
            );
            assert_eq!(
                state
                    .remote_seats
                    .seat(VIEWER)
                    .and_then(|seat| seat.focused_surface),
                None,
                "the fast path retires the shared caret before refusing input"
            );
            assert!(
                !state.remote_seats.key_transition(VIEWER, SHIFT, false),
                "the held modifier was already released exactly once"
            );
        }
        // Phase `packet_fast_path_retires_input_when_it_observes_viewer_removal_first`.
        {
            const HOST: PeerId = 0xAAAA;
            const VIEWER: PeerId = 0xBBBB;
            const SURFACE: SurfaceId = 84;
            const LEFT: u32 = 272;

            let session = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
            );
            session.add_viewer(VIEWER, false);
            let mut state = make_state();
            state.register_share_session(Arc::clone(&session));
            let _ = state.remote_seats.seat_mut(VIEWER).pointer_enter(SURFACE);
            assert!(state.remote_seats.button_transition(VIEWER, LEFT, true));

            session.drop_viewer(VIEWER);
            assert!(
                !state.apply_network_remote_input(
                    VIEWER,
                    SURFACE,
                    InputEvent::PointerButton {
                        button: LEFT,
                        pressed: false,
                    },
                ),
                "a removed viewer's packet is refused after cleanup"
            );
            assert_eq!(state.remote_seats.button_holder_count(LEFT), 0);
            assert_eq!(
                state
                    .remote_seats
                    .seat(VIEWER)
                    .and_then(|seat| seat.pointer_surface),
                None
            );
            assert!(state.share_registry.get(VIEWER, SURFACE).is_none());
        }
        // Phase `downgrading_pointer_only_viewer_ends_drag_without_a_caret`.
        {
            const HOST: PeerId = 0xAAAA;
            const VIEWER: PeerId = 0xBBBB;
            const SURFACE: SurfaceId = 82;
            const LEFT: u32 = 272;

            let session = Arc::new(
                SharedFlatpakSession::start(HOST, "app", SURFACE, 32, 32, &cfg()).expect("session"),
            );
            session.add_viewer(VIEWER, false);
            let mut state = make_state();
            state.register_share_session(Arc::clone(&session));
            let _ = state.remote_seats.seat_mut(VIEWER).pointer_enter(SURFACE);
            assert!(state.remote_seats.button_transition(VIEWER, LEFT, true));
            assert_eq!(
                state
                    .remote_seats
                    .seat(VIEWER)
                    .and_then(|seat| seat.focused_surface),
                None
            );

            session.set_read_only(VIEWER, true);
            state.sync_share_authority(&session);

            assert_eq!(state.remote_seats.button_holder_count(LEFT), 0);
            assert_eq!(
                state
                    .remote_seats
                    .seat(VIEWER)
                    .and_then(|seat| seat.pointer_surface),
                None
            );
        }
    }

    #[test]
    fn session_lock_hooks_report_edges_without_granting_unlock() {
        // Phase `session_lock_hooks_report_and_clear_lock_edge_only`.
        // The `ext_session_lock_v1` bridge reports a lock request as a one-shot
        // edge. Protocol-object release deliberately has no unlock edge.
        {
            let mut h = SessionLockHooks::default();
            assert!(!h.take_lock_requested());
            assert!(!h.has_pending_locker());

            h.lock_requested = true;
            assert!(h.take_lock_requested());
            assert!(
                !h.take_lock_requested(),
                "lock edge is consumed exactly once"
            );
        }
        // Phase `protocol_session_lock_release_is_not_unlock_authority`.
        // Releasing an ext-session-lock object cannot manufacture an unlock
        // request. The protocol callback itself is exercised here so a future
        // refactor cannot accidentally reconnect object lifetime to authentication.
        {
            let mut state = make_state();
            state.session_lock.lock_requested = true;

            <State as SessionLockHandler>::unlock(&mut state);

            assert!(
                state.session_lock.take_lock_requested(),
                "releasing the protocol object must not cancel the pending lock"
            );
            assert!(!state.session_lock.has_pending_locker());
        }
        // Phase `fresh_state_advertises_session_lock_and_starts_unlocked`.
        // A freshly constructed state advertises the session-lock global and starts with no client holding a lock.
        {
            let mut state = make_state();
            // Constructing the state ran `SessionLockManagerState::new` (global advertised) without panic; no client holds a lock yet.
            assert!(!state.session_lock.take_lock_requested());
            assert!(!state.session_lock.has_pending_locker());
        }
    }
}

#[cfg(test)]
mod keycap_layout_tests {
    use super::*;
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;

    fn write_input(settings: &std::path::Path, active: &str) {
        let document = serde_json::json!({
            "input": {"keyboard_layout": active, "keyboard_layouts": ["us", "de"]},
        });
        std::fs::write(settings, document.to_string()).expect("settings");
    }

    #[test]
    fn model_only_reload_preserves_the_declared_physical_model() {
        let temp = tempfile::tempdir().expect("temp");
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_CONFIG_HOME", temp.path());
        let settings = settings_store::settings_path().expect("settings path");
        std::fs::create_dir_all(settings.parent().expect("config dir")).expect("config dir");
        write_input(&settings, "us");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());
        for model in ["jp106", "kr106", "pc104", "pc105"] {
            settings_store::update_section(
                "input",
                serde_json::json!({"keyboard_model": model})
                    .as_object()
                    .expect("input section")
                    .clone(),
            )
            .expect("persist model");
            state.reload_keyboard_settings();
            assert_eq!(state.applied_keyboard.model, model);
            assert_eq!(state.applied_keyboard.layouts, "us,de");
            assert_eq!(state.applied_keyboard.active_index, 0);
        }
        // The installed PC models have distinct geometry. Their evdev keymap
        // bytes may be identical, so Smithay correctly suppresses duplicate
        // keymap FDs. Geometry is tested against native compiled model data
        // in Settings; actual symbol/mask ownership has its own Winit wire test.
    }

    #[test]
    fn key_names_follow_every_layout_switch_live() {
        let temp = tempfile::tempdir().expect("temp");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", temp.path());
        let settings = settings_store::settings_path().expect("settings path");
        std::fs::create_dir_all(settings.parent().expect("config dir")).expect("config dir");
        write_input(&settings, "us");

        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let power = settings_store::hardware::KeyChord::parse("Ctrl+Alt+Delete").expect("chord");
        let label = || crate::shell::keyboard_shortcuts::chord_text(&power);
        assert_eq!(label(), "Ctrl+Alt+Delete", "the session starts on US");

        // The input-source chord switches the seat's group in place.
        state.activate_keyboard_source(1);
        assert_eq!(label(), "Strg+Alt+Entf");
        assert_eq!(
            crate::compositor::pointer_lock::release_chord_label(),
            "Strg+Alt+Esc"
        );

        // A choice made in Settings arrives through the settings reload.
        write_input(&settings, "us");
        state.reload_keyboard_settings();
        assert_eq!(label(), "Ctrl+Alt+Delete");
        write_input(&settings, "de");
        state.reload_keyboard_settings();
        assert_eq!(label(), "Strg+Alt+Entf");
        // The stored chord never changed.
        assert_eq!(power.display(), "Ctrl+Alt+Delete");
    }
}

/// The compositor loop re-derives state from the workspace tree and the
/// surface registries only after something it depends on changed.
#[cfg(test)]
mod loop_gating_tests {
    use super::*;
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;

    fn make_state() -> State {
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        State::new(event_loop.handle(), display.handle())
    }

    #[test]
    fn update_surface_scales_drops_unplaced_surfaces() {
        let mut state = make_state();
        state.update_surface_scales(&[(1, (0.0, 0.0)), (2, (10.0, 10.0)), (2, (10.0, 10.0))]);
        let mut placed = state.surface_scales.keys().copied().collect::<Vec<_>>();
        placed.sort_unstable();
        assert_eq!(placed, vec![1, 2]);
        state.update_surface_scales(&[(2, (10.0, 10.0))]);
        assert_eq!(
            state.surface_scales.keys().copied().collect::<Vec<_>>(),
            vec![2],
            "a surface no longer placed loses its cached scale"
        );
        state.update_surface_scales(&[]);
        assert!(state.surface_scales.is_empty());
    }

    #[test]
    fn a_wakeup_without_a_tree_write_walks_nothing() {
        const SURFACE: u64 = 0x0188_0001;
        const APP: &str = "org.example.LoopGating";
        let mut state = make_state();
        let tree = state.workspace_tree.clone();
        state.client_input.refresh_from_tree(&tree);
        let walks = state.client_input.tree_walks();
        assert_eq!(walks, 1, "the first wakeup derives everything once");
        for _ in 0..100 {
            assert!(!state.client_input.refresh_from_tree(&tree));
        }
        assert_eq!(
            state.client_input.tree_walks(),
            walks,
            "wakeups with no tree write and no surface change walk nothing"
        );

        crate::shell::tab_binding::adopt_toplevel_at(
            &mut tree.write(),
            SURFACE,
            Some(APP.into()),
            crate::shell::tab_binding::Placement::NewGroup,
        );
        assert!(
            !state.client_input.app_ids_current(tree.generation()),
            "a tree written since the last wakeup is not answered from the cache"
        );
        state.client_input.refresh_from_tree(&tree);
        assert_eq!(state.client_input.tree_walks(), walks.saturating_add(1));
        assert!(state.client_input.app_ids_current(tree.generation()));
        assert_eq!(state.client_input.app_id(SURFACE), Some(APP));
        for _ in 0..100 {
            state.client_input.refresh_from_tree(&tree);
        }
        assert_eq!(state.client_input.tree_walks(), walks.saturating_add(1));
    }

    #[test]
    fn capture_protection_is_rederived_only_after_an_input_changed() {
        const KEYBOARD: u64 = 0x0188_0002;
        const CONNECTION: u64 = 0x0188_0C02;
        const APP: &str = "org.example.LoopGatingProtected";
        let store = crate::compositor::foreign::store();
        let mut state = make_state();
        state.sync_capture_protection();
        assert_eq!(state.capture_protection_passes(), 1);
        for _ in 0..100 {
            state.sync_capture_protection();
        }
        assert_eq!(
            state.capture_protection_passes(),
            1,
            "wakeups that change nothing take neither the tree nor the protection lock"
        );

        drop(state.workspace_tree.write());
        state.sync_capture_protection();
        assert_eq!(
            state.capture_protection_passes(),
            2,
            "a tree write may have renamed a protected window's tab"
        );

        state.osk_surface = Some(KEYBOARD);
        state.sync_capture_protection();
        assert_eq!(
            state.capture_protection_passes(),
            3,
            "a new keyboard surface"
        );
        state.sync_capture_protection();
        assert_eq!(state.capture_protection_passes(), 3);

        store.request_app_capture_protection(CONNECTION, APP, true);
        state.sync_capture_protection();
        assert_eq!(
            state.capture_protection_passes(),
            4,
            "an app asked for protection"
        );
        store.release_capture_protection(CONNECTION);
        state.sync_capture_protection();
        assert_eq!(state.capture_protection_passes(), 5, "and gave it up");
        state.sync_capture_protection();
        assert_eq!(state.capture_protection_passes(), 5);
        store.forget(KEYBOARD);
    }
}

smithay::delegate_presentation!(State);
