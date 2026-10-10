//! Deliver local keyboard/pointer input to foreign Wayland clients.
//!
//! The compositor draws its own chrome through
//! [`crate::compositor::frame::FrameBuilder::dispatch_event`], but a foreign
//! `xdg_toplevel` only sees input this module explicitly pushes into the
//! `wl_seat`. It owns the surface registry (protocol id → `WlSurface`),
//! focus arbitration between chrome and client, and translation of winit
//! events into evdev keycodes and surface-local coordinates. Chrome always wins focus.
//!
//! Geometry: a foreign toplevel is the sole tab of its own window group, so
//! the group's laid-out tile rect is exactly the surface's on-screen rectangle (see [`crate::shell::overlay_host::ShellOverlayHost::active_tile_rect`]).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use ui::widget::{WheelJumpAccum, wheel_jump_detents};

pub use crate::shell::input_policy::*;
use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent,
};
use smithay::input::touch::{
    DownEvent as TouchDownEvent, MotionEvent as TouchMotionEvent, UpEvent as TouchUpEvent,
};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point as SmithayPoint, Serial};
use vello::kurbo::Rect;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

use crate::compositor::backend::{
    GestureEvent as BackendGestureEvent, TouchEvent as BackendTouchEvent,
};
use crate::compositor::frame::FrameBuilder;
use crate::compositor::state::State;
use crate::shell::state::{GroupId, StripId, TileContent, WorkspaceTree, WorkspaceTreeHandle};
use tilo_desktop::notifications::{
    Notification, NotificationIconKind, NotificationSeverity, PERSISTENT_DISMISS,
};

/// Linux `BTN_LEFT`. The Wayland pointer protocol carries raw evdev button codes, not an enum, so the winit `MouseButton` has to be mapped back.
const BTN_LEFT: u32 = 0x110;

/// Which side of the compositor owns the pointer for the duration of an implicit grab (first button down → last button up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerOwner {
    /// The shell's own chrome widgets.
    Chrome,
    /// A foreign client surface (protocol id).
    Client(u64),
}

/// A foreign toplevel waiting to be mirrored into the shell chrome's workspace tree so it gets a tab, a laid-out tile rect, and therefore a place for input to land.
#[derive(Clone, Debug)]
struct PendingAdoption {
    surface_id: u64,
    app_id: Option<String>,
}

/// Per-compositor state for routing local input into client surfaces — lives on [`State`] because the seat lives there; the run loop reaches it through the free functions at the bottom of this module.
#[derive(Default)]
pub struct ClientInput {
    /// Live foreign surfaces keyed by their `wl_surface` protocol id — the same id [`crate::shell::state::TileContent::Foreign`] stores.
    surfaces: HashMap<u64, WlSurface>,
    /// Surface currently holding the seat's keyboard focus, or `None` when the shell chrome owns the keyboard.
    keyboard_focus: Option<u64>,
    /// Toplevel configured with `xdg_toplevel.state.activated` — the focused
    /// tile, which draws itself as the active window. Unlike
    /// [`Self::keyboard_focus`] it survives a shell overlay briefly taking the
    /// keyboard, so opening the palette does not repaint every app inactive.
    activated: Option<u64>,
    /// Surface the pointer is currently inside (and therefore focused on at the seat), or `None` when the pointer is over chrome.
    pointer_focus: Option<u64>,
    /// Global origin of the pointer-focused surface, as the router placed it.
    pointer_focus_origin: Option<(f64, f64)>,
    /// Last known cursor position in global coordinates; `None` until
    /// moved. `Option`, not `(0.0, 0.0)`, since presence is level-triggered — a default would put a peer's cursor at top-left for a session that never moved.
    cursor: Option<(f64, f64)>,
    /// Implicit pointer grab owner, latched on the first button press and released on the last button up — without this a drag leaving its tile would silently switch to the chrome mid-gesture.
    grab: Option<PointerOwner>,
    /// Number of physical pointer buttons currently held.
    buttons_down: u32,
    /// Foreign toplevels announced by the xdg-shell handler but not yet mirrored into the chrome's workspace tree.
    pending_adoptions: Vec<PendingAdoption>,
    /// Surfaces whose toplevel was destroyed but whose chrome tab has not been reaped yet.
    pending_removals: Vec<u64>,
    /// App-id updates that arrived after adoption (`set_app_id` races `get_toplevel`), waiting to be applied to the chrome tab.
    pending_renames: Vec<(u64, String)>,
    /// Where each mirrored surface ended up: `surface id -> (strip, group)`.
    placements: HashMap<u64, (StripId, GroupId)>,
    /// Advanced whenever [`Self::surfaces`] gains or loses a surface, since
    /// placements cover only live surfaces.
    surfaces_epoch: u64,
    /// The tree generation and surface epoch [`Self::placements`] was last
    /// derived from; `None` before the first derivation.
    placements_from: Option<(u64, u64)>,
    /// The app id of every foreign tab in the tree, as of tree generation
    /// [`Self::app_ids_generation`]. Capture protection is decided per app,
    /// for every popup and every streamed frame, so it reads this instead
    /// of walking the tree.
    app_ids: HashMap<u64, String>,
    app_ids_generation: Option<u64>,
    /// Whole-tree walks made to derive placements and app ids.
    #[cfg(test)]
    tree_walks: usize,
    /// evdev codes whose *press* was forwarded to a client. A release
    /// forwards iff its press did — a client never sees a release for a key it never saw pressed (a shell chord).
    forwarded_keys: HashSet<u32>,
    /// Accumulator for the Alt+Shift wheel group-jump gesture, so a touchpad's small pixel deltas fire one discrete jump per [`JUMP_PX_PER_DETENT`]. See [`WheelJumpAccum`].
    jump_wheel: WheelJumpAccum,
    /// Whether this module currently owns a content-drop indicator in the
    /// retained chrome. Prevents cleanup from touching an ordinary tab drag.
    content_drop_hover_active: bool,
    /// Click/touch focus changes awaiting durable session-state persistence.
    /// Saving copies the whole workspace tree, so it runs after
    /// latency-sensitive Wayland input has been queued/flushed.
    focus_persistence_pending: bool,
    /// Whether the latest direct-touch down was delivered to a client over
    /// `wl_touch`. That client owns the whole tap, so the compositor must not
    /// also synthesise a pointer click for it.
    touch_down_reached_client: bool,
    /// Who the input being routed belongs to, when it is not the person at
    /// this machine: an agent's seat. Its key and button edges are then
    /// attributed to that participant on the shared-seat ledger.
    participant: Option<crate::network::protocol::PeerId>,
}

impl ClientInput {
    /// Record a freshly-created foreign toplevel — called from
    /// `XdgShellHandler::new_toplevel`; the canonical workspace tab is
    /// adopted by that callback and chrome observes it through the shared
    /// handle on the next frame.
    pub fn register_toplevel(
        &mut self,
        surface_id: u64,
        surface: WlSurface,
        app_id: Option<String>,
    ) {
        self.surfaces.insert(surface_id, surface);
        self.surfaces_epoch = self.surfaces_epoch.wrapping_add(1);
        self.pending_adoptions
            .push(PendingAdoption { surface_id, app_id });
    }

    /// Register a popup/candidate surface for seat routing without placing it
    /// in the workspace.  Transients are painted above an existing app, never
    /// become tabs in their own right.
    pub fn register_transient(&mut self, surface_id: u64, surface: WlSurface) {
        self.surfaces.insert(surface_id, surface);
        self.surfaces_epoch = self.surfaces_epoch.wrapping_add(1);
    }

    /// Drop any queued adoption/removal/rename bookkeeping. The canonical
    /// workspace handle is mutated directly by protocol callbacks; these
    /// queues are retained as bounded bookkeeping for input routing only.
    pub fn clear_pending(&mut self) -> bool {
        let had_pending = !self.pending_adoptions.is_empty()
            || !self.pending_removals.is_empty()
            || !self.pending_renames.is_empty();
        self.pending_adoptions.clear();
        self.pending_removals.clear();
        self.pending_renames.clear();
        had_pending
    }

    /// Refresh input-routing addresses from the authoritative shared tree.
    /// Protocol callbacks adopt directly into that tree, so draining their
    /// bookkeeping must not also discard the strip/group address needed for
    /// initial keyboard focus (notably xwayland-satellite's first window).
    pub fn refresh_placements_from_tree(&mut self, tree: &WorkspaceTree) -> bool {
        let live = self.surfaces.keys().copied().collect::<HashSet<_>>();
        let next = placements_for_live_surfaces(tree, &live);
        if self.placements == next {
            return false;
        }
        self.placements = next;
        true
    }

    /// Re-derive placements and app ids from the shared tree, but only when
    /// it was written or the set of live surfaces changed since the last
    /// derivation: a wakeup that changed neither walks nothing. Returns
    /// whether the placements changed.
    pub fn refresh_from_tree(&mut self, handle: &WorkspaceTreeHandle) -> bool {
        // Loaded before the lock is taken, so the tree read below is at least
        // this new; a write racing in between only costs one more walk.
        let generation = handle.generation();
        let key = (generation, self.surfaces_epoch);
        if self.placements_from == Some(key) && self.app_ids_generation == Some(generation) {
            return false;
        }
        let tree = handle.read();
        #[cfg(test)]
        {
            self.tree_walks = self.tree_walks.saturating_add(1);
        }
        let changed = self.refresh_placements_from_tree(&tree);
        if self.app_ids_generation != Some(generation) {
            refresh_app_ids(&mut self.app_ids, &tree);
            self.app_ids_generation = Some(generation);
        }
        drop(tree);
        self.placements_from = Some(key);
        changed
    }

    /// Whether [`Self::app_id`] answers for tree generation `generation`.
    /// A tree written since the last [`Self::refresh_from_tree`] may have
    /// renamed or added a tab, so the cache must not be trusted.
    pub fn app_ids_current(&self, generation: u64) -> bool {
        self.app_ids_generation == Some(generation)
    }

    /// The app id of the foreign tab showing `surface`, as of the last
    /// [`Self::refresh_from_tree`]; check [`Self::app_ids_current`] first.
    pub fn app_id(&self, surface: u64) -> Option<&str> {
        self.app_ids.get(&surface).map(String::as_str)
    }

    /// Whole-tree walks [`Self::refresh_from_tree`] has made.
    #[cfg(test)]
    pub(crate) const fn tree_walks(&self) -> usize {
        self.tree_walks
    }

    /// Record a destroyed foreign toplevel — drops the surface handle immediately and queues the chrome-side tab removal.
    pub fn unregister_toplevel(&mut self, surface_id: u64) {
        self.surfaces.remove(&surface_id);
        self.surfaces_epoch = self.surfaces_epoch.wrapping_add(1);
        self.pending_adoptions
            .retain(|p| p.surface_id != surface_id);
        self.pending_removals.push(surface_id);
        self.clear_surface_input_state(surface_id);
    }

    /// Remove a transient without queuing a workspace-tab removal.
    pub fn unregister_transient(&mut self, surface_id: u64) {
        self.surfaces.remove(&surface_id);
        self.surfaces_epoch = self.surfaces_epoch.wrapping_add(1);
        self.clear_surface_input_state(surface_id);
    }

    fn clear_surface_input_state(&mut self, surface_id: u64) {
        if self.keyboard_focus == Some(surface_id) {
            self.keyboard_focus = None;
        }
        if self.pointer_focus == Some(surface_id) {
            self.pointer_focus = None;
        }
        if self.grab == Some(PointerOwner::Client(surface_id)) {
            self.grab = None;
        }
    }

    /// Record a late `xdg_toplevel.set_app_id`.
    pub fn note_app_id(&mut self, surface_id: u64, app_id: String) {
        if let Some(pending) = self
            .pending_adoptions
            .iter_mut()
            .find(|p| p.surface_id == surface_id)
        {
            pending.app_id = Some(app_id);
            return;
        }
        self.pending_renames.push((surface_id, app_id));
    }

    /// Keep a registered surface routable while removing every assumption
    /// that it belongs to a tiled window. Used only for the compositor-
    /// authorised on-screen keyboard after its stable app-id arrives.
    pub fn promote_to_overlay(&mut self, surface_id: u64) {
        self.pending_adoptions
            .retain(|pending| pending.surface_id != surface_id);
        self.pending_renames.retain(|(id, _)| *id != surface_id);
        self.pending_removals.retain(|id| *id != surface_id);
        self.placements.remove(&surface_id);
        // Derive again from the tree, exactly as the next wakeup used to.
        self.surfaces_epoch = self.surfaces_epoch.wrapping_add(1);
    }

    /// The surface the seat's keyboard is currently focused on, if any.
    pub fn keyboard_focus(&self) -> Option<u64> {
        self.keyboard_focus
    }

    /// The surface the seat's pointer is currently focused on, if any.
    pub fn pointer_focus_origin(&self) -> Option<(f64, f64)> {
        self.pointer_focus_origin
    }

    pub fn pointer_focus(&self) -> Option<u64> {
        self.pointer_focus
    }

    /// Whether `surface_id` is a live foreign toplevel this compositor can route input to.
    pub fn is_known(&self, surface_id: u64) -> bool {
        self.surfaces.contains_key(&surface_id)
    }

    /// The protocol object for a surface id, if it is still live.
    pub fn surface(&self, surface_id: u64) -> Option<&WlSurface> {
        self.surfaces.get(&surface_id)
    }

    /// Every mirrored surface and the group it is in, so the run loop can resolve each one's tile rect and derive its preferred scale.
    pub fn placements(&self) -> impl Iterator<Item = (u64, GroupId)> + '_ {
        self.placements.iter().map(|(id, (_, group))| (*id, *group))
    }

    /// The workspace address assigned when this surface was mirrored.
    pub fn placement(&self, surface_id: u64) -> Option<(StripId, GroupId)> {
        self.placements.get(&surface_id).copied()
    }

    /// Number of live foreign toplevels.
    pub fn len(&self) -> usize {
        self.surfaces.len()
    }

    /// True when no foreign toplevel is registered.
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Last cursor position the router observed, in global coordinates —
    /// falls back to the origin for callers wanting a point, not a question. Use [`Self::pointer_position`] to distinguish "never been anywhere" from "at the origin".
    pub fn cursor(&self) -> (f64, f64) {
        self.cursor.unwrap_or_default()
    }

    /// Attribute the input routed from now on to `participant` (an agent's
    /// seat), or with `None` to the person at this machine again.
    pub fn set_participant(&mut self, participant: Option<crate::network::protocol::PeerId>) {
        self.participant = participant;
    }

    /// Where the pointer is, or `None` if it has never been anywhere — the honest form of [`Self::cursor`], for the one caller that must tell the two apart.
    pub fn pointer_position(&self) -> Option<(f64, f64)> {
        self.cursor
    }

    /// Take the edge requesting persistence of a click/touch focus change.
    pub fn take_focus_persistence_pending(&mut self) -> bool {
        std::mem::take(&mut self.focus_persistence_pending)
    }
}

// Pure helpers — no seat, no widgets. Unit-testable in isolation.

pub(crate) fn monotonic_time_ns() -> Option<u128> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the out pointer refers to a live timespec for the entire call.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
        return None;
    }
    u128::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u128::try_from(now.tv_nsec).ok()?)
}

/// Mint a fresh input serial. Wayland clients ignore events whose serial goes backwards, so every delivered event gets its own.
fn mint_serial(state: &mut State) -> Serial {
    state.serial_counter = state.serial_counter.wrapping_add(1);
    Serial::from(state.serial_counter)
}

fn record_activation_serial(state: &mut State, serial: Serial, surface_id: u64) {
    state
        .clients_given_input
        .insert(crate::compositor::state::client_index_of(surface_id));
    let now = Instant::now();
    state
        .activation_serials
        .push_back((serial, surface_id, now));
    while state
        .activation_serials
        .front()
        .is_some_and(|(_, _, issued)| {
            now.saturating_duration_since(*issued)
                > crate::compositor::activation::MAX_ACTIVATION_AGE
        })
        || state.activation_serials.len() > 512
    {
        let _ = state.activation_serials.pop_front();
    }
}

/// The xdg `app_id` a toplevel declared, for naming it in latency records.
fn toplevel_app_id(surface: &WlSurface) -> Option<String> {
    smithay::wayland::compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok()?.app_id.clone())
    })
}

/// Milliseconds since compositor start — the timestamp base every input event on this seat shares.
fn event_time(state: &State) -> u32 {
    // `as_millis()` is `u128`; keep the low 32 bits exactly as `as u32` would (wrapping after ~49.7 days of uptime), just without the lint-flagged cast. `millis` is always in range, so `try_from` never actually falls back.
    let millis = state.start_time.elapsed().as_millis() % (1u128 << 32);
    u32::try_from(millis).unwrap_or(0)
}

impl State {
    /// Revoke client keyboard focus and retire every pressed seat key without
    /// delivering it to an application.
    ///
    /// This is for a trusted input-source reset (lock/seat loss), where the
    /// backend cannot promise that the physical release will arrive.  Smithay
    /// owns the authoritative XKB pressed-key set, so clearing our small
    /// routing ledger alone would leave modifiers stuck in the Wayland seat.
    /// Focus is removed *before* the synthetic releases: forwarding then
    /// clears Smithay's forwarded-key bookkeeping while no client can observe
    /// a fabricated release.
    pub fn reset_client_keyboard_state(&mut self) {
        self.set_client_keyboard_focus(None);
        let _ = self.remote_seats.release_local_keys();
        let Some(keyboard) = self.seat.get_keyboard() else {
            self.client_input.forwarded_keys.clear();
            return;
        };
        let pressed = keyboard.pressed_keys();
        for keycode in pressed {
            let serial = mint_serial(self);
            let time = event_time(self);
            let _: Option<()> = keyboard.input(
                self,
                keycode,
                KeyState::Released,
                serial,
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
        self.client_input.forwarded_keys.clear();
    }

    /// Point the seat's keyboard at `surface_id` (or nothing when `None`), sending `leave`/`enter` and the keymap; idempotent, so the run loop can call it unconditionally every tick.
    /// Configure `surface_id` as the activated toplevel and the previous one
    /// as no longer activated. A toplevel that has not had its opening
    /// configure keeps the state pending, so it rides that configure.
    pub fn set_activated_toplevel(&mut self, surface_id: Option<u64>) {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as Xdg;
        let previous = std::mem::replace(&mut self.client_input.activated, surface_id);
        if previous == surface_id {
            return;
        }
        for toplevel in self.xdg_shell_state.toplevel_surfaces() {
            let id = crate::compositor::state::surface_id(toplevel.wl_surface());
            if Some(id) != previous && Some(id) != surface_id {
                continue;
            }
            let activated = Some(id) == surface_id;
            toplevel.with_pending_state(|pending| {
                if activated {
                    pending.states.set(Xdg::Activated);
                } else {
                    pending.states.unset(Xdg::Activated);
                }
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
    }

    /// The toplevel currently configured as activated.
    pub fn activated_toplevel(&self) -> Option<u64> {
        self.client_input.activated
    }

    pub fn set_client_keyboard_focus(&mut self, surface_id: Option<u64>) {
        // A tile showing a dialog sheet types into the sheet, not the window it belongs to.
        let surface_id =
            surface_id.map(|id| crate::compositor::foreign::store().keyboard_target(id));
        if self.client_input.keyboard_focus == surface_id {
            return;
        }
        if let Some(previous) = self.client_input.keyboard_focus {
            // Remote participants share this one app-facing Wayland seat.
            // Retire their held keys while the old client still owns focus,
            // before Smithay sends the next client its focus-enter key array.
            self.release_remote_keys_for_surface(previous);
            let root = crate::compositor::foreign::store().sheet_root(previous);
            if root != previous {
                self.release_remote_keys_for_surface(root);
            }
        }
        let target = match surface_id {
            Some(id) => self.client_input.surfaces.get(&id).map(|s| (id, s.clone())),
            None => None,
        };
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let serial = mint_serial(self);
        self.client_input.keyboard_focus = target.as_ref().map(|(id, _)| *id);
        // Clipboard/primary selection follow the keyboard: smithay refuses `set_selection` from a client without keyboard focus.
        let selection_client = target.as_ref().and_then(|(_, surface)| surface.client());
        self.set_selection_focus(selection_client);
        keyboard.set_focus(self, target.clone().map(|(_, s)| s), serial);
        self.tablet_pads_follow_keyboard(target.as_ref().map(|(_, surface)| surface));
        if let Some((surface_id, surface)) = target {
            record_activation_serial(self, serial, surface_id);
            // The focused app's scope gets the foreground tier and the one it
            // replaces is demoted. Queued to a worker thread: this is the
            // compositor's input path and must never wait on systemd.
            if let Some(pid) = surface
                .client()
                .and_then(|client| client.get_data::<crate::compositor::state::ClientState>().and_then(|state| state.pid))
            {
                ::launcher::priority::notify_focus(pid);
            }
        }
    }

    /// Push one key press/release through the seat, forwarding to the focused
    /// client only when `forward` is set; `true` when it reached a client.
    ///
    /// **Every** key goes through here, including ones the shell keeps:
    /// smithay's xkb state derives modifier state from the keycode stream, so
    /// skipping them desynchronises it. `FilterResult::Intercept` advances
    /// state without emitting; `keycode` is evdev, smithay wants xkb form (evdev + 8).
    pub fn deliver_client_key(&mut self, keycode: u32, pressed: bool, forward: bool) -> bool {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let has_focus = self.client_input.keyboard_focus.is_some();
        let serial = mint_serial(self);
        let time = event_time(self);
        let key_state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let observed_surface = keyboard
            .current_focus()
            .as_ref()
            .map(super::state::surface_id);
        let delivery_time_ns = (forward
            && has_focus
            && pressed
            && crate::shell::launcher::LaunchSession::current().presentation_observer_enabled())
        .then(monotonic_time_ns)
        .flatten();
        let intercepted: Option<()> = keyboard.input(
            self,
            keycode.saturating_add(8).into(),
            key_state,
            serial,
            time,
            |_, _, _| {
                if forward {
                    FilterResult::Forward
                } else {
                    FilterResult::Intercept(())
                }
            },
        );
        let delivered = forward && has_focus && intercepted.is_none();
        if delivered && let Some(surface_id) = self.client_input.keyboard_focus {
            if let Some((delivery_time_ns, observed_surface)) =
                delivery_time_ns.zip(observed_surface)
            {
                eprintln!(
                    "tilo-physical-delivery {}",
                    serde_json::json!({
                        "wayland_serial": u32::from(serial),
                        "surface_id": observed_surface,
                        "clock_id": libc::CLOCK_MONOTONIC,
                        "delivery_time_ns": delivery_time_ns,
                        "input_time_ns": self.physical_key_input_time_ns.filter(|origin| u128::from(*origin) <= delivery_time_ns),
                    })
                );
            }
            record_activation_serial(self, serial, surface_id);
            if pressed {
                let app = self
                    .client_input
                    .surfaces
                    .get(&surface_id)
                    .and_then(toplevel_app_id)
                    .unwrap_or_default();
                crate::compositor::key_latency::tracker().key_delivered(
                    surface_id,
                    &app,
                    keycode,
                    Instant::now(),
                );
            }
        }
        delivered
    }

    /// Move the seat pointer to `global` and focus it on `target` (`(surface
    /// id, tile origin)`) — smithay derives `enter`/`leave`/`motion` from the focus transition; passing `None` is how the pointer *leaves* a client.
    pub fn forward_client_pointer_motion(
        &mut self,
        global: (f64, f64),
        target: Option<(u64, (f64, f64))>,
    ) {
        // A locked pointer does not move; only relative motion reaches the
        // client. (The direct backend already holds it; this covers hosts
        // that keep reporting absolute positions.)
        if self.pointer_lock.is_locked() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let focus: Option<(WlSurface, SmithayPoint<f64, Logical>)> =
            target.and_then(|(id, origin)| {
                self.client_input
                    .surfaces
                    .get(&id)
                    .cloned()
                    .map(|s| (s, SmithayPoint::from(origin)))
            });
        let previous_focus = self.client_input.pointer_focus;
        self.client_input.pointer_focus = focus.as_ref().and_then(|_| target.map(|(id, _)| id));
        self.client_input.pointer_focus_origin = focus
            .as_ref()
            .and_then(|_| target.map(|(_, origin)| origin));
        let serial = mint_serial(self);
        let time = event_time(self);
        self.move_seat_pointer(&pointer, focus, global, (serial, time));
        pointer.frame(self);
        // `wl_pointer.motion` itself has no serial. Smithay uses this serial
        // only when the motion changes focus and emits `wl_pointer.enter`, so
        // recording every motion would bless predictable numbers the client
        // never received.
        if previous_focus != self.client_input.pointer_focus
            && let Some(surface_id) = self.client_input.pointer_focus
        {
            record_activation_serial(self, serial, surface_id);
        }
    }

    /// Forward a pointer button press/release to the surface the pointer is focused on — no-op when the pointer is over chrome.
    pub fn forward_client_pointer_button(&mut self, button: u32, pressed: bool) -> bool {
        // A chrome drag's grab needs the release even with no client under the pointer — that is how it learns the drag was cancelled.
        let smithay_grabbed = self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed());
        if self.client_input.pointer_focus.is_none()
            && self.selection.chrome_drag.is_none()
            && !smithay_grabbed
        {
            return false;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let serial = mint_serial(self);
        let time = event_time(self);
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
                serial,
                time,
            },
        );
        pointer.frame(self);
        if let Some(surface_id) = self.client_input.pointer_focus {
            record_activation_serial(self, serial, surface_id);
        }
        true
    }

    /// Forward a scroll event to the surface the pointer is focused on.
    ///
    /// `wl_pointer.axis_stop` ends a finger scroll on that axis, so it goes
    /// out once, when the fingers lift (`phase` ended). Sent with every
    /// event for the axis that did not move, it told toolkits each wheel
    /// notch was the end of a gesture: winit reported it as an ended
    /// scroll, which a scroll view takes for a fling release and ignores.
    pub fn forward_client_pointer_axis(
        &mut self,
        delta: MouseScrollDelta,
        phase: winit::event::TouchPhase,
    ) -> bool {
        if self.client_input.pointer_focus.is_none() {
            return false;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let time = event_time(self);
        let ((h, v), v120) = axis_values(delta);
        let mut frame = AxisFrame::new(time).source(match v120 {
            Some(_) => AxisSource::Wheel,
            None => AxisSource::Finger,
        });
        if h != 0.0 {
            frame = frame.value(Axis::Horizontal, h);
        }
        if v != 0.0 {
            frame = frame.value(Axis::Vertical, v);
        }
        if let Some((h120, v120)) = v120 {
            if h120 != 0 {
                frame = frame.v120(Axis::Horizontal, h120);
            }
            if v120 != 0 {
                frame = frame.v120(Axis::Vertical, v120);
            }
        }
        if v120.is_none()
            && matches!(
                phase,
                winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
            )
        {
            frame = frame.stop(Axis::Horizontal).stop(Axis::Vertical);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
        true
    }

    /// Forward one touchpad gesture to the focused client over
    /// `zwp_pointer_gestures_v1` — only gestures the shell did *not* claim reach here, since the finger-count split happens once, in [`crate::compositor::backend::drm::gestures`].
    pub fn forward_client_gesture(&mut self, gesture: BackendGestureEvent) {
        if self.client_input.pointer_focus.is_none() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = mint_serial(self);
        let time = event_time(self);
        match gesture {
            BackendGestureEvent::SwipeBegin { fingers } => pointer.gesture_swipe_begin(
                self,
                &GestureSwipeBeginEvent {
                    serial,
                    time,
                    fingers,
                },
            ),
            BackendGestureEvent::SwipeUpdate { dx, dy } => pointer.gesture_swipe_update(
                self,
                &GestureSwipeUpdateEvent {
                    time,
                    delta: SmithayPoint::from((dx, dy)),
                },
            ),
            BackendGestureEvent::SwipeEnd { cancelled } => pointer.gesture_swipe_end(
                self,
                &GestureSwipeEndEvent {
                    serial,
                    time,
                    cancelled,
                },
            ),
            BackendGestureEvent::PinchBegin { fingers } => pointer.gesture_pinch_begin(
                self,
                &GesturePinchBeginEvent {
                    serial,
                    time,
                    fingers,
                },
            ),
            BackendGestureEvent::PinchUpdate {
                dx,
                dy,
                scale,
                rotation,
            } => pointer.gesture_pinch_update(
                self,
                &GesturePinchUpdateEvent {
                    time,
                    delta: SmithayPoint::from((dx, dy)),
                    scale,
                    rotation,
                },
            ),
            BackendGestureEvent::PinchEnd { cancelled } => pointer.gesture_pinch_end(
                self,
                &GesturePinchEndEvent {
                    serial,
                    time,
                    cancelled,
                },
            ),
            BackendGestureEvent::HoldBegin { fingers } => pointer.gesture_hold_begin(
                self,
                &GestureHoldBeginEvent {
                    serial,
                    time,
                    fingers,
                },
            ),
            BackendGestureEvent::HoldEnd { cancelled } => pointer.gesture_hold_end(
                self,
                &GestureHoldEndEvent {
                    serial,
                    time,
                    cancelled,
                },
            ),
        }
    }

    /// Record a direct-touch down that the compositor kept away from every
    /// client (a locked session, or chrome capturing input).
    pub fn note_touch_down_withheld_from_clients(&mut self) {
        self.client_input.touch_down_reached_client = false;
    }

    /// Whether the latest touch-down reached a client that takes touch over
    /// `wl_touch`. Such a contact is that client's alone; any other is turned
    /// into pointer input (see [`crate::compositor::touch_pointer`]).
    pub fn touch_down_reached_client(&self) -> bool {
        self.client_input.touch_down_reached_client
    }

    /// Forward one touch point to the focused client over `wl_touch`.
    /// `target` is `(surface id, tile origin)` for a `Down` landing inside a client tile; touch focus is set on `down` only (per protocol).
    pub fn forward_client_touch(
        &mut self,
        touch: BackendTouchEvent,
        target: Option<(u64, (f64, f64))>,
    ) {
        let handle = self.seat.get_touch();
        let focus: Option<(WlSurface, SmithayPoint<f64, Logical>)> =
            target.and_then(|(id, origin)| {
                self.client_input
                    .surfaces
                    .get(&id)
                    .cloned()
                    .map(|s| (s, SmithayPoint::from(origin)))
            });
        if matches!(touch, BackendTouchEvent::Down { .. })
            && let Some((id, _)) = target
        {
            self.clients_given_input
                .insert(crate::compositor::state::client_index_of(id));
        }
        if matches!(touch, BackendTouchEvent::Down { .. }) {
            // A client that never asked for a `wl_touch` would receive
            // nothing; it gets the contact as pointer input instead.
            self.client_input.touch_down_reached_client = handle.is_some()
                && focus.as_ref().is_some_and(|(surface, _)| {
                    surface.client().is_some_and(|client| {
                        client
                            .get_data::<crate::compositor::state::ClientState>()
                            .is_some_and(|data| {
                                data.touch_bound.load(std::sync::atomic::Ordering::Acquire)
                            })
                    })
                });
        }
        let Some(handle) = handle else {
            return;
        };
        let serial = mint_serial(self);
        let time = event_time(self);
        match touch {
            BackendTouchEvent::Down { slot, x, y } => handle.down(
                self,
                focus,
                &TouchDownEvent {
                    slot: Some(slot).into(),
                    location: SmithayPoint::from((x, y)),
                    serial,
                    time,
                },
            ),
            BackendTouchEvent::Motion { slot, x, y } => handle.motion(
                self,
                focus,
                &TouchMotionEvent {
                    slot: Some(slot).into(),
                    location: SmithayPoint::from((x, y)),
                    time,
                },
            ),
            BackendTouchEvent::Up { slot } => handle.up(
                self,
                &TouchUpEvent {
                    slot: Some(slot).into(),
                    serial,
                    time,
                },
            ),
            BackendTouchEvent::Cancel => handle.cancel(self),
            BackendTouchEvent::Frame => handle.frame(self),
        }
    }
}

/// Which client (if any) a touch point should be delivered to — only a `Down` establishes touch focus; the rest of the sequence belongs to whatever the down landed on.
pub fn touch_target(
    state: &State,
    fb: &FrameBuilder,
    touch: &BackendTouchEvent,
) -> Option<(u64, (f64, f64))> {
    let BackendTouchEvent::Down { x, y, .. } = touch else {
        return None;
    };
    surface_at(state, fb, (*x, *y))
}

/// The client surface at global `point` and its global origin: the
/// keyboard, popups, fullscreen and floating windows before tiles, and a
/// tile's subsurface before the tile.
fn surface_at(state: &State, fb: &FrameBuilder, point: (f64, f64)) -> Option<(u64, (f64, f64))> {
    let (x, y) = (&point.0, &point.1);
    if let Some((surface_id, rect)) = fb.osk_surface_hit((*x, *y))
        && state.osk_surface == Some(surface_id)
        && state.client_input.surfaces.contains_key(&surface_id)
    {
        return Some((
            surface_id,
            crate::compositor::foreign::store().surface_origin(surface_id, rect, point),
        ));
    }
    if let Some((surface_id, rect)) = fb.transient_surface_hit((*x, *y))
        && state.client_input.surfaces.contains_key(&surface_id)
    {
        return Some((
            surface_id,
            crate::compositor::foreign::store().surface_origin(surface_id, rect, point),
        ));
    }
    if let Some((surface_id, rect)) = fb.fullscreen_surface_hit((*x, *y))
        && state.client_input.surfaces.contains_key(&surface_id)
    {
        return Some((
            surface_id,
            crate::compositor::foreign::store().surface_origin(surface_id, rect, point),
        ));
    }
    if let Some((surface_id, rect)) = fb.floating_surface_hit((*x, *y))
        && state.client_input.surfaces.contains_key(&surface_id)
    {
        return Some((
            surface_id,
            crate::compositor::foreign::store().surface_origin(surface_id, rect, point),
        ));
    }
    let hit = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        foreign_tile_at(&tree, fb.host(), (*x, *y))?
    };
    if !state.client_input.surfaces.contains_key(&hit.surface_id) {
        return None;
    }
    route_tile_point(hit.surface_id, hit.rect, (*x, *y))
}

/// The surface a global point on a foreign tile reaches and that surface's
/// global origin: the tile's toplevel, one of its subsurfaces, or a sheet
/// over it. `None` when a modal sheet swallows the point.
fn route_tile_point(root: u64, tile: Rect, (x, y): (f64, f64)) -> Option<(u64, (f64, f64))> {
    let store = crate::compositor::foreign::store();
    let origin = store.surface_origin(root, tile, (x, y));
    match store.route_point(root, x - origin.0, y - origin.1) {
        crate::compositor::foreign::PointRoute::Root => Some((root, origin)),
        crate::compositor::foreign::PointRoute::Surface {
            surface,
            x: local_x,
            y: local_y,
        } => Some((surface, (x - local_x, y - local_y))),
        crate::compositor::foreign::PointRoute::Blocked => None,
    }
}

/// Direct touch follows the same click-to-focus policy as pointer input.
/// App-owned overlays have no workspace address and are focused directly;
/// tiled clients promote their exact group before the normal keyboard-focus
/// reconciliation. Touching the OSK must preserve the edited app's focus.
pub fn focus_touch_target(state: &mut State, fb: &mut FrameBuilder, surface_id: u64) {
    if state.osk_surface == Some(surface_id) {
        return;
    }
    if state.is_output_presentation_surface(surface_id) {
        state.focus_output_presentation_surface(surface_id);
        sync_keyboard_focus(state, fb);
        return;
    }
    state.clear_output_presentation_focus();
    if state.is_floating_toplevel(surface_id)
        || state.is_transient_surface(surface_id)
        || fb
            .fullscreen_surface_rect()
            .is_some_and(|(fullscreen_id, _)| fullscreen_id == surface_id)
    {
        state.set_client_keyboard_focus(Some(surface_id));
        return;
    }
    let placement = state.client_input.placement(surface_id).or_else(|| {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        tree.desktop.workspaces.iter().find_map(|workspace| {
            workspace.strips.iter().find_map(|strip| {
                strip.window_groups.iter().find_map(|group| {
                    group
                        .tabs
                        .iter()
                        .any(|tab| {
                            matches!(
                                tab.tile,
                                crate::shell::state::TileContent::Foreign {
                                    surface_id: id,
                                    ..
                                } if id == surface_id
                            )
                        })
                        .then_some((strip.id, group.id))
                })
            })
        })
    });
    if let Some((strip, group)) = placement {
        if raise_group(fb, strip, group) {
            state.client_input.focus_persistence_pending = true;
        }
        sync_keyboard_focus(state, fb);
    }
}

// Run-loop entry points.

/// Reconcile the seat's keyboard focus with the shell's focused tab. Called every tick: chrome capture drops client focus outright; otherwise focus follows the active tab.
pub fn sync_keyboard_focus(state: &mut State, fb: &FrameBuilder) {
    let captured = chrome_captures_input(fb);
    // Overlay clicks assign focus directly because they are not represented in
    // the workspace tree. Preserve that assignment across the next tick while
    // the surface is still mapped; otherwise this reconciliation immediately
    // steals focus back for the active tiled tab. A dismissed popup/floating
    // surface falls through to the ordinary presentation/tree focus below.
    let current_focus = state.client_input.keyboard_focus();
    let overlay_focus = retain_live_overlay_focus(
        current_focus,
        current_focus.filter(|id| {
            state.is_floating_toplevel(*id) && fb.floating_surface_rect(*id).is_some()
        }),
        current_focus.filter(|id| {
            state.is_transient_surface(*id) && fb.transient_surface_rect(*id).is_some()
        }),
        current_focus.filter(|id| {
            fb.fullscreen_surface_rect()
                .is_some_and(|(fullscreen_id, _)| fullscreen_id == *id)
        }),
    );
    // A window the person focused while showing it on another device of
    // theirs types there until they use this device again.
    let candidate = state
        .focused_output_presentation_surface()
        .or_else(|| state.headless_keyboard_focus())
        .or_else(|| {
            // A system-modal prompt owns the keyboard while it is shown.
            state
                .top_system_modal()
                .filter(|id| fb.transient_surface_rect(*id).is_some())
        })
        .or(overlay_focus)
        .or_else(|| state.personal_keyboard_focus())
        .or_else(|| {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            active_foreign_surface(&tree).filter(|id| {
                // Do not grant a tiled tab keyboard focus before its xdg
                // surface has a buffer. Keeping the focus cache empty until
                // that map transition also makes the first post-commit
                // reconciliation issue the enter rather than treating it as
                // an idempotent no-op. This is intentionally a protocol-map
                // check, not the visual `presented` gate: a client which is
                // still resizing from an old buffer needs input to answer its
                // configure.
                state.client_input.surfaces.contains_key(id)
                    && crate::compositor::foreign::store().is_mapped(*id)
            })
        });
    state.set_activated_toplevel(candidate);
    state.set_client_keyboard_focus(gate_keyboard_focus(candidate, captured));
}

/// Rebuild `app_ids` from every foreign tab in `tree`, reusing its entries.
/// The first tab showing a surface wins, as in
/// [`crate::shell::tab_binding::app_id_for_surface`].
fn refresh_app_ids(app_ids: &mut HashMap<u64, String>, tree: &WorkspaceTree) {
    let tabs = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| match &tab.tile {
            TileContent::Foreign { surface_id, app_id } => Some((*surface_id, app_id)),
            _ => None,
        });
    let mut seen = HashSet::new();
    for (surface, app_id) in tabs {
        if !seen.insert(surface) {
            continue;
        }
        if app_ids.get(&surface) != Some(app_id) {
            app_ids.insert(surface, app_id.clone());
        }
    }
    app_ids.retain(|surface, _| seen.contains(surface));
}

fn placements_for_live_surfaces(
    tree: &WorkspaceTree,
    live: &HashSet<u64>,
) -> HashMap<u64, (StripId, GroupId)> {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| {
            strip.window_groups.iter().flat_map(move |group| {
                group.tabs.iter().filter_map(move |tab| match tab.tile {
                    TileContent::Foreign { surface_id, .. } if live.contains(&surface_id) => {
                        Some((surface_id, (strip.id, group.id)))
                    }
                    _ => None,
                })
            })
        })
        .collect()
}

/// Advance the compositor seat's keyboard state while a trusted shell
/// surface (currently the lock screen) owns the visible interaction.
///
/// The lock must never forward input to an application, but it cannot simply
/// drop physical edges: a modifier pressed before lock and released while
/// locked would otherwise remain pressed in Smithay after unlock.  Smithay
/// only retires its forwarded-key set when the filter says `Forward`, so this
/// path deliberately forwards every capture edge *after* revoking focus.
/// With focus `None` no client sees it, while a pre-lock client key and a key
/// held across unlock get the same coherent seat bookkeeping.
pub fn consume_keyboard_while_captured(state: &mut State, event: &WindowEvent) {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return;
    };
    let Some(code) = evdev_keycode(key) else {
        return;
    };
    state.set_client_keyboard_focus(None);
    let pressed = key.state == ElementState::Pressed;
    if !state.remote_seats.local_key_transition(code, pressed)
        && (pressed || state.remote_seats.key_holder_count(code) != 0)
    {
        return;
    }
    // A press may predate participant tracking (or enter through a direct seat
    // delivery). Its captured release still retires both client ledgers when
    // no participant holds the key. Never release another participant's hold.
    let _ = track_forwarded_key(&mut state.client_input.forwarded_keys, code, pressed, false);
    // `Intercept` updates XKB but does not remove a previously forwarded
    // key from Smithay's focus-enter key array.  Focus is already `None`, so
    // `Forward` advances that internal ledger without emitting to an app.
    let _ = state.deliver_client_key(code, pressed, true);
}

/// Decide what the pointer shows this tick: nothing while touch is the input
/// in use (a finger needs no cursor, and touch never moves it), the focused
/// client's request while the pointer is over its surface, otherwise the
/// cursor the chrome under the pointer names (the default arrow when it names
/// none). The lock screen is not chrome and never shows a client's cursor.
///
/// Records the result on the frame builder, where captures and the state
/// dump read it, and returns it only when it changed so the caller hands it
/// to the backend once.
pub fn sync_pointer_image(
    state: &State,
    fb: &mut FrameBuilder,
    session_locked: bool,
    touch_in_use: bool,
) -> Option<crate::compositor::cursor::PointerImage> {
    use crate::compositor::cursor::{CursorShape, PointerImage};
    let image = if touch_in_use {
        PointerImage::Hidden
    } else if session_locked {
        PointerImage::default()
    } else if state.client_input.pointer_focus().is_some() {
        state.client_cursor.image()
    } else {
        fb.capture_pointer_position()
            .and_then(|point| fb.chrome_cursor_at(point))
            .map_or_else(PointerImage::default, |icon| {
                PointerImage::Shape(CursorShape::from_icon(icon))
            })
    };
    if *fb.pointer_image() == image {
        return None;
    }
    fb.set_pointer_image(image.clone());
    Some(image)
}

/// Route one pen event. A client that took a tablet seat gets the tool over
/// `zwp_tablet_tool_v2`; for anything else — other apps, shell chrome, the
/// lock screen — the pen stands in for the pointer, and the returned events
/// go through the ordinary pointer path.
///
/// Over a tablet-aware client the pen still is the pointer as far as the
/// desktop goes: the cursor follows it, touching down focuses and raises the
/// window as a click does (and closes a popup it lands outside of), and a
/// stroke stays with the window it started in until the tip lifts, as a held
/// button keeps the pointer with its client.
pub fn route_tablet_event(
    state: &mut State,
    fb: &mut FrameBuilder,
    input: &crate::compositor::backend::TabletInput,
    session_locked: bool,
) -> Vec<crate::compositor::backend::BackendEvent> {
    use crate::compositor::backend::TabletPhase;
    let usable = !session_locked && !chrome_captures_input(fb);
    let under = usable
        .then(|| surface_at(state, fb, input.position))
        .flatten();
    let target = match state.tablet_clients.stroke.filter(|_| usable) {
        Some((held, origin)) => Some(match under {
            Some((surface_id, now)) if surface_id == held => (held, now),
            _ => (held, origin),
        }),
        None => under.filter(|(surface_id, _)| state.surface_takes_tablet_input(*surface_id)),
    };
    let mut input = std::borrow::Cow::Borrowed(input);
    if target.is_some() && input.phase != TabletPhase::ProximityOut {
        state.client_input.cursor = Some(input.position);
        fb.host_mut()
            .note_pointer(vello::kurbo::Point::new(input.position.0, input.position.1));
        if input.phase == TabletPhase::TipDown && state.tablet_clients.stroke.is_none() {
            let hit = current_pointer_target(state, fb, false);
            if state.dismiss_popup_grab_outside(hit.map(|hit| hit.surface_id)) {
                // Like a click outside a popup, the touch only closes it.
                input.to_mut().phase = TabletPhase::Motion;
            } else {
                state.tablet_clients.stroke = target;
                if let Some(hit) = hit {
                    focus_pressed(state, fb, hit);
                }
            }
        }
    }
    state.forward_tablet(&input, target);
    if !usable || matches!(input.phase, TabletPhase::TipUp | TabletPhase::ProximityOut) {
        state.tablet_clients.stroke = None;
    }
    if target.is_some() {
        return Vec::new();
    }
    crate::compositor::tablet::tablet_pointer_events(&input)
        .into_iter()
        .map(crate::compositor::backend::BackendEvent::Input)
        .collect()
}

/// Click-to-focus: raise the pressed window group so the keyboard follows
/// the pointer, or the pen, into the client.
fn focus_pressed(state: &mut State, fb: &mut FrameBuilder, h: TileHit) {
    if state.is_output_presentation_surface(h.surface_id) {
        // Audience focus is explicit and never mutates the
        // presenter's workspace selection.
        state.focus_output_presentation_surface(h.surface_id);
        sync_keyboard_focus(state, fb);
    } else {
        state.clear_output_presentation_focus();
    }
    let overlay_focus = state.is_floating_toplevel(h.surface_id)
        || state.is_transient_surface(h.surface_id)
        || fb
            .fullscreen_surface_rect()
            .is_some_and(|(surface_id, _)| surface_id == h.surface_id);
    if overlay_focus && state.osk_surface != Some(h.surface_id) {
        state.set_client_keyboard_focus(Some(h.surface_id));
    }
    // Tapping the keyboard must keep the editable field's
    // keyboard focus. Pointer focus moves to the OSK, but
    // raising/focusing it would deactivate the very
    // text-input session the tap is meant to feed.
    if !state.is_output_presentation_surface(h.surface_id)
        && state.osk_surface != Some(h.surface_id)
        && !state.is_floating_toplevel(h.surface_id)
        && !state.is_transient_surface(h.surface_id)
    {
        if raise_group(fb, h.strip, h.group) {
            state.client_input.focus_persistence_pending = true;
        }
        sync_keyboard_focus(state, fb);
    }
}

/// Route one tablet pad event. Pads follow keyboard focus: a focused client
/// that took a tablet seat gets the buttons, rings and strips over
/// `zwp_tablet_pad_v2`; otherwise — another app, shell chrome, the lock
/// screen — the returned fallback (what the pad's settings map the control
/// to) goes through the ordinary input path.
pub fn route_pad_event(
    state: &mut State,
    fb: &FrameBuilder,
    event: crate::compositor::backend::PadEvent,
    session_locked: bool,
) -> Vec<crate::compositor::backend::BackendEvent> {
    let target = pad_focus_target(state, fb, session_locked);
    if state.forward_tablet_pad(&event, target) {
        return Vec::new();
    }
    event.fallback
}

/// The surface a tablet pad's controls go to: the keyboard-focused one,
/// when it took a tablet seat and neither the lock screen nor shell chrome
/// is taking input.
pub fn pad_focus_target(state: &State, fb: &FrameBuilder, session_locked: bool) -> Option<u64> {
    (!session_locked && !chrome_captures_input(fb))
        .then(|| state.client_input.keyboard_focus())
        .flatten()
        .filter(|surface_id| state.surface_takes_tablet_input(*surface_id))
}

/// Grant, adjust or withdraw a client's pointer lock/confinement for this
/// tick. Only the surface that both holds keyboard focus and sits under the
/// local pointer may hold it, and never while the session is locked or
/// chrome is taking input. Returns what the backend must apply.
pub fn sync_pointer_constraint(
    state: &mut State,
    fb: &FrameBuilder,
    session_locked: bool,
) -> Option<crate::compositor::pointer_lock::ConstraintChange> {
    let target = (!session_locked && !chrome_captures_input(fb))
        .then(|| state.client_input.pointer_focus())
        .flatten()
        .filter(|surface_id| state.client_input.keyboard_focus() == Some(*surface_id))
        .and_then(|surface_id| {
            let rect = {
                let chrome = fb.chrome();
                let tree = chrome.tree();
                foreign_tile_for_surface(&tree, fb.host(), surface_id).map(|hit| hit.rect)
            }
            .or_else(|| fb.floating_surface_rect(surface_id))?;
            Some(crate::compositor::pointer_lock::GrantTarget {
                surface_id,
                rect: (rect.x0, rect.y0, rect.x1, rect.y1),
            })
        });
    state.sync_pointer_constraint(target)
}

/// Tell the remote-input firewall what is on screen. Called every tick,
/// immediately after [`sync_keyboard_focus`], so a remote event is judged against what the local user is looking at.
///
/// The tile list comes from [`foreign_tiles`], the same enumeration
/// [`foreign_tile_at`] resolves local clicks with. `session_locked` must be
/// passed in since the lock surface isn't part of the tiled tree — without it a guest's click would pass through to a password-protected window.
pub fn publish_remote_input_view(state: &mut State, fb: &FrameBuilder, session_locked: bool) {
    let (tiles, chrome_regions) = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        let mut tiles = foreign_tiles(&tree, fb.host())
            .into_iter()
            // A tab whose surface the compositor no longer holds is not a place input can land, for the local user or a remote one.
            .filter(|hit| state.client_input.surfaces.contains_key(&hit.surface_id))
            .map(|hit| (hit.surface_id, hit.rect))
            .collect::<Vec<_>>();
        let chrome_regions = foreign_tiles(&tree, fb.host())
            .iter()
            .filter_map(|hit| {
                fb.host()
                    .group_resize_rect(hit.group)
                    .map(|rect| (hit.surface_id, rect))
            })
            .collect();
        drop(tree);
        drop(chrome);
        tiles.extend(
            fb.floating_surface_rects()
                .into_iter()
                .filter(|(surface_id, _)| state.client_input.surfaces.contains_key(surface_id)),
        );
        tiles.extend(state.output_presentation_remote_tiles());
        let offscreen = state.offscreen_stream_tiles(&tiles);
        tiles.extend(offscreen);
        (tiles, chrome_regions)
    };
    let view = crate::network::input_firewall::ShellView::new(
        tiles,
        // Viewers address the streamed tile; Wayland enters its modal
        // sheet. Compare against the tile's root while delivering to the
        // real focused child, so Save dialogs keep the same guarded door.
        state
            .client_input
            .keyboard_focus()
            .map(|surface| crate::compositor::foreign::store().sheet_root(surface)),
        chrome_captures_input(fb) || fb.host().dictation_mutes_remote_input(),
        fb.host().palette_visible(),
    )
    .chrome_regions(chrome_regions)
    .locked(session_locked)
    .pointer_held(state.pointer_lock.is_engaged());
    state.remote_input_firewall.publish_view(view);
}

/// Stop every peer from driving this seat, and say so on screen when there
/// was anything to stop.
///
/// Action half of the `Super+Esc` kill switch, split from the backend loop
/// (a `-> !` function) so it can be driven from a test. Returns how many
/// shares lost write access.
///
/// The chord always latches the firewall, but the toast is only for a
/// person who just cut off someone who was connected (`other_control_live`
/// covers a device-control session the registry does not see). With nobody
/// connected, `Super+Esc` is just a stray chord (Esc to leave the palette
/// with Super still held) and a "stopped" warning would report an event
/// nobody saw start. When shown, the toast bypasses Do-Not-Disturb
/// ([`Notification::bypassing_dnd`]) and is persistent.
pub fn stop_remote_input(
    state: &mut State,
    fb: &mut FrameBuilder,
    other_control_live: bool,
) -> usize {
    let ended = state.end_all_remote_input();
    if ended.live == 0 && !other_control_live {
        return ended.revoked;
    }
    fb.with_chrome(|chrome| {
        chrome.push_notification_full(
            Notification::new(
                crate::shell::i18n::t("notification-remote-input-stopped"),
                match ended.revoked {
                    0 => crate::shell::i18n::t("notification-remote-input-stopped-sessions"),
                    n => crate::shell::i18n::t_args(
                        "notification-remote-input-stopped-shares",
                        &[("count", n.into())],
                    ),
                },
            )
            .with_severity(NotificationSeverity::Warning)
            .with_icon_kind(NotificationIconKind::Network)
            .with_dismiss_after(PERSISTENT_DISMISS)
            .bypassing_dnd(),
        );
    });
    ended.revoked
}

/// Route one backend input event. Returns `true` when consumed by a client
/// surface and must **not** also be dispatched into the shell chrome;
/// `false` for everything the shell owns.
///
/// Compositor chords are checked before any forwarding, so a focused client can never swallow `Super+L`, the search palette, or workspace switching.
pub fn route_input(
    state: &mut State,
    fb: &mut FrameBuilder,
    event: &WindowEvent,
    modifiers: &Modifiers,
) -> bool {
    // The host's own input takes priority over every remote peer for the next half second; stamped here since both the backend loop and the test-control socket push local input through this function.
    // An agent is one more participant, not the host, and wins no priority.
    if state.client_input.participant.is_none() {
        state
            .remote_input_firewall
            .note_local_input(std::time::Instant::now());
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            state.end_remote_typing();
        }
        state.release_personal_keyboard_focus();
        // Another device of the person last placed this seat's pointer, on a
        // window it shows. The host's cursor never left its spot, so the
        // pointer goes back to what is under it before this input lands.
        if state.take_personal_pointer_hold()
            && !matches!(event, WindowEvent::CursorMoved { .. })
            && let Some((x, y)) = state.client_input.pointer_position()
        {
            let _ = route_cursor_moved(
                state,
                fb,
                chrome_captures_input(fb),
                winit::dpi::PhysicalPosition::new(x, y),
            );
        }
    }
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            // Capture depends on the pointer's *new* location (notably the
            // Restore/Close controls painted over a maximized client). Record
            // it before deciding ownership; otherwise crossing from the
            // client into shell chrome routes this motion to the client and
            // leaves the widget hit-test one event behind.
            fb.host_mut()
                .note_pointer(vello::kurbo::Point::new(position.x, position.y));
            route_cursor_moved(state, fb, chrome_captures_input(fb), *position)
        }
        WindowEvent::CursorLeft { .. } => route_cursor_left(state),
        WindowEvent::MouseInput {
            state: element,
            button,
            ..
        } => route_mouse_input(state, fb, chrome_captures_input(fb), *element, *button),
        WindowEvent::MouseWheel { delta, phase, .. } => {
            route_mouse_wheel(state, fb, *delta, *phase, *modifiers)
        }
        WindowEvent::KeyboardInput { .. } => {
            route_keyboard_input(state, fb, chrome_captures_input(fb), event, *modifiers)
        }
        _ => false,
    }
}

/// `WindowEvent::CursorMoved` half of [`route_input`]: track the new global
/// position, keep the chrome's own pointer idea current, and hand the motion to whichever client (if any) is under it.
fn route_cursor_moved(
    state: &mut State,
    fb: &mut FrameBuilder,
    captured: bool,
    position: winit::dpi::PhysicalPosition<f64>,
) -> bool {
    state.client_input.cursor = Some((position.x, position.y));
    // Withheld from the chrome's own dispatch below (belongs to the client) — without this the chrome's pointer idea freezes at its last chrome-side position.
    // During a client-originated content drag, tiled window chrome/body is a
    // shell placement target rather than an offer to the app currently under
    // it. Withdrawing pointer focus here makes Smithay report `target: None`
    // on release, which is the protocol boundary used by the shell drop pump.
    let point = vello::kurbo::Point::new(position.x, position.y);
    fb.host_mut().note_pointer(point);
    // Resolve placement once: `has_content_drop_target` was the same walk
    // as `content_drop_placement`, so a drag over chrome paid for it twice
    // on every motion event.
    let shell_drop_placement = state
        .selection
        .dnd_active
        .then(|| fb.host().content_drop_placement(point))
        .flatten();
    let shell_content_target = shell_drop_placement.is_some();
    if state.selection.dnd_active {
        fb.host_mut()
            .set_content_drop_hover(shell_content_target.then_some(point));
        state.selection.shell_drop_placement = shell_drop_placement;
        state.client_input.content_drop_hover_active = shell_content_target;
    }
    let hit = if shell_content_target {
        None
    } else {
        current_pointer_target(state, fb, captured)
    };
    deliver_pointer(state, hit);
    // During a chrome drag the file manager keeps drawing the ghost under the cursor, so it sees every move even over a client.
    // A captured move must continue to `FrameBuilder::dispatch_event` so
    // shell widgets update their hover/hit-test cursor. Returning `true`
    // here was why the following Restore press saw the last client-side
    // coordinate instead of the visible button.
    hit.is_some() && state.selection.chrome_drag.is_none() && !captured
}

/// `WindowEvent::CursorLeft` half of [`route_input`]: the pointer left the
/// output entirely, so any client focus and implicit grab go with it.
fn route_cursor_left(state: &mut State) -> bool {
    // A backend may not deliver matching button-up edges after pointer leave
    // or device loss. Retire the host's ownership while the existing client
    // focus/grab is still present. Refcounting leaves a remote participant's
    // overlapping drag intact.
    for button in state.remote_seats.release_local_buttons() {
        let _ = state.forward_client_pointer_button(button, false);
    }
    let global = state.client_input.cursor();
    state.forward_client_pointer_motion(global, None);
    state.client_input.grab = None;
    state.client_input.buttons_down = 0;
    false
}

/// `WindowEvent::MouseInput` half of [`route_input`]: latches the implicit
/// grab on the first button down, raises + focuses the clicked window, and forwards the button to whichever side owns the grab.
fn route_mouse_input(
    state: &mut State,
    fb: &mut FrameBuilder,
    captured: bool,
    element: ElementState,
    button: MouseButton,
) -> bool {
    let pressed = element == ElementState::Pressed;
    if pressed {
        if state.client_input.buttons_down == 0 {
            // Latch the implicit grab from a fresh hit test, so a drag that leaves the tile stays with its owner.
            let hit = current_pointer_target(state, fb, captured);
            // An xdg popup grab is a protocol modal boundary: the first
            // click outside its transient chain dismisses it before that
            // click is offered to the underlying tile/chrome.
            if state.dismiss_popup_grab_outside(hit.map(|hit| hit.surface_id)) {
                // `popup_done` is asynchronous from the client's destroy.
                // Clear the old popup pointer focus and consume this press so
                // it cannot activate the surface behind the modal popup.
                let global = state.client_input.cursor();
                state.forward_client_pointer_motion(global, None);
                state.client_input.grab = None;
                return true;
            }
            deliver_pointer(state, hit);
            // Smithay's xdg-popup grab must receive the outside press even
            // when the pointer has left every client surface. Do not layer
            // our ordinary implicit grab over that protocol-modal grab.
            let smithay_grabbed = state
                .seat
                .get_pointer()
                .is_some_and(|pointer| pointer.is_grabbed());
            if !smithay_grabbed {
                state.client_input.grab = Some(match hit {
                    Some(h) => PointerOwner::Client(h.surface_id),
                    None => PointerOwner::Chrome,
                });
            }
            if let Some(h) = hit {
                focus_pressed(state, fb, h);
            }
        }
        state.client_input.buttons_down = state.client_input.buttons_down.saturating_add(1);
    } else {
        state.client_input.buttons_down = state.client_input.buttons_down.saturating_sub(1);
    }
    // A chrome drag's release is the drop: the server-side grab smithay holds only ends on a button event, whether or not a client is under the pointer.
    let chrome_drag = state.selection.chrome_drag.is_some();
    let smithay_grabbed = state
        .seat
        .get_pointer()
        .is_some_and(|pointer| pointer.is_grabbed());
    let to_client = smithay_grabbed
        || (matches!(state.client_input.grab, Some(PointerOwner::Client(_)))
            && state.client_input.pointer_focus.is_some())
        || (chrome_drag && !pressed);
    let consumed = if to_client {
        match evdev_button(button) {
            Some(code) => {
                if admit_local_pointer_edge(state, code, pressed, chrome_drag) {
                    state.forward_client_pointer_button(code, pressed)
                } else {
                    // The shared legacy pointer already has this button in
                    // the requested state. It still belongs to the client
                    // grab; do not let the duplicate edge fall through to
                    // shell chrome.
                    true
                }
            }
            None => false,
        }
    } else {
        false
    };
    if state.client_input.buttons_down == 0 {
        state.client_input.grab = None;
        if !pressed {
            // Re-resolve focus now the grab is gone — the release may have happened over a different tile (or over chrome).
            let hit = current_pointer_target(state, fb, captured);
            deliver_pointer(state, hit);
        }
    }
    // The file manager has to see the release too, to end its own side of the gesture without moving anything.
    consumed && !chrome_drag
}

/// Decide whether one local pointer edge reaches the shared Wayland seat.
///
/// Ordinary client gestures are refcounted with remote participants. A drag
/// promoted from compositor chrome is different: its press belonged to the
/// in-process widget and was therefore never entered in that ledger, but the
/// server-side `DnD` grab still requires the matching release to finish a drop.
fn admit_local_pointer_edge(
    state: &mut State,
    button: u32,
    pressed: bool,
    chrome_drag: bool,
) -> bool {
    let final_shared_edge = match state.client_input.participant {
        Some(peer) => state.remote_seats.button_transition(peer, button, pressed),
        None => state.remote_seats.local_button_transition(button, pressed),
    };
    final_shared_edge || (chrome_drag && !pressed)
}

/// `WindowEvent::MouseWheel` half of [`route_input`]: Alt-modified wheel is a
/// compositor navigation gesture (Niri parity), grabbed before client
/// forwarding like every shell chord, and reported as consumed either way:
///   Alt+wheel        → switch workspace
///   Alt+Shift+wheel  → jump focus to the neighbouring group
/// Anything else forwards straight to the focused client.
fn route_mouse_wheel(
    state: &mut State,
    fb: &mut FrameBuilder,
    delta: MouseScrollDelta,
    phase: winit::event::TouchPhase,
    modifiers: Modifiers,
) -> bool {
    let mods = modifiers.state();
    if mods.alt_key() && !mods.control_key() && !mods.super_key() {
        let detents = wheel_jump_detents(&delta);
        let steps = state.client_input.jump_wheel.push(detents, Instant::now());
        // Diagnostic for gesture-feel reports. `RUST_LOG=shell=debug`.
        tracing::debug!(
            target: "shell::wheel_jump",
            ?delta,
            detents,
            steps,
            shift = mods.shift_key(),
            "alt-wheel jump gesture"
        );
        if steps != 0 {
            if mods.shift_key() {
                // Same primitive as Super+Left/Right — one implementation of "next window along the strip".
                if fb.host_mut().step_focus_along_strip(steps) {
                    sync_keyboard_focus(state, fb);
                }
            } else {
                // Positive detents rotate DOWN the workspace wheel; `switch_workspace` stops at the wheel's end.
                let dir = steps.signum();
                fb.with_chrome(|chrome| {
                    for _ in 0..steps.unsigned_abs().min(4) {
                        if !chrome.switch_workspace(dir) {
                            break;
                        }
                    }
                });
            }
        }
        return true;
    }
    if state.client_input.pointer_focus.is_some() {
        state.forward_client_pointer_axis(delta, phase)
    } else {
        false
    }
}

/// `WindowEvent::KeyboardInput` half of [`route_input`]: every key goes
/// through here (shell-owned ones too, to keep the seat's xkb state in
/// sync), forwarded to the focused client only when the shell doesn't own the chord and a client actually holds keyboard focus.
fn route_keyboard_input(
    state: &mut State,
    fb: &FrameBuilder,
    captured: bool,
    event: &WindowEvent,
    modifiers: Modifiers,
) -> bool {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return false;
    };
    let pressed = key.state == ElementState::Pressed;
    // DRM mirrors winit's repeat events for in-process Tilo widgets. Native
    // Wayland clients already synthesize repeats from `wl_keyboard.repeat_info`;
    // sending another pressed edge would double every held key. Shell-owned
    // repeats must still reach the host so navigation can repeat and the host
    // can absorb edge-triggered actions (close/open/toggle) without running
    // them again.
    if key.repeat {
        let overlay_open = fb.host().palette_visible() || fb.host().wheel_summoned();
        let configured_shortcut = fb.host().owns_configured_chord(&key.logical_key, modifiers);
        let shell_owns = captured
            || fb.host().dictation_owns_key(&key.logical_key)
            || (!shortcuts_inhibited(state)
                && (configured_shortcut || is_shell_chord(event, &modifiers, overlay_open)))
            || state.global_shortcuts.claims(&key.physical_key, modifiers);
        return if shell_owns {
            false
        } else {
            !captured && state.client_input.keyboard_focus.is_some()
        };
    }
    let Some(code) = evdev_keycode(key) else {
        return false;
    };
    // Anything the shell has up that Escape would dismiss — the summoned workspace wheel counts, since Escape must reach the shell, not a focused client.
    let overlay_open = fb.host().palette_visible() || fb.host().wheel_summoned();
    // Static safety chords and user-edited shortcuts share the same
    // client-first boundary. Without the host-side check, a valid binding
    // such as Alt+G is shown and saved by Settings but disappears whenever a
    // real app holds focus because that app consumes the press first.
    let configured_shortcut =
        pressed && fb.host().owns_configured_chord(&key.logical_key, modifiers);
    // A VM or remote desktop that inhibits shortcuts gets the shell's
    // chords too; the compositor's own reserved keys never reach here.
    let shell_chord = !shortcuts_inhibited(state)
        && (configured_shortcut || is_shell_chord(event, &modifiers, overlay_open));
    // An approved GlobalShortcuts grant fires over whatever app holds
    // focus: shell chords win, then the grant, then the client. Its release
    // is withheld too (the press never reached the client) and sends
    // `Deactivated`, which push-to-talk style clients wait for.
    let granted = if pressed {
        !shell_chord && state.global_shortcuts.press(&key.physical_key, modifiers)
    } else {
        state.global_shortcuts.release(&key.physical_key)
    };
    let shell_owns =
        captured || shell_chord || granted || fb.host().dictation_owns_key(&key.logical_key);
    let forward = track_forwarded_key(
        &mut state.client_input.forwarded_keys,
        code,
        pressed,
        !shell_owns && state.client_input.keyboard_focus.is_some(),
    );
    let admitted = match state.client_input.participant {
        Some(peer) => state.remote_seats.key_transition(peer, code, pressed),
        None => state.remote_seats.local_key_transition(code, pressed),
    };
    if !admitted {
        return false;
    }
    // Runs for shell-owned keys too: the seat's xkb state has to track every key, or its modifier view drifts (see `deliver_client_key`).
    let delivered = state.deliver_client_key(code, pressed, forward);
    // A held chord ends on its modifier's release: Alt+Tab commits when Alt
    // comes up. The client that saw the press still gets its release, and
    // the shell sees it too, whoever holds keyboard focus.
    let ends_held_chord =
        !pressed && is_modifier_key(&key.logical_key) && fb.host().window_switcher_active();
    delivered && !ends_held_chord
}

fn shortcuts_inhibited(state: &State) -> bool {
    state
        .client_input
        .keyboard_focus
        .is_some_and(|surface_id| state.shortcut_inhibit.inhibits(surface_id))
}

/// Promote a file drag from the hosted file manager into a Wayland drag.
/// Called every tick: once the chrome's in-process drag gesture has the
/// pointer over a foreign tile, the compositor claims it, starts smithay's
/// server-side drag offering the file, and delivers the enter. From then on `current_pointer_target` hit-tests normally so the client under the pointer receives `wl_data_device` enter/motion/drop; the release is the drop — Files does not act on the release itself.
#[allow(
    clippy::too_many_lines,
    reason = "the hand-off is one ordered drag state-machine transition"
)]
pub fn pump_chrome_drag(state: &mut State, fb: &mut FrameBuilder) {
    if state.selection.chrome_drag.is_some() || state.selection.dnd_active {
        return;
    }
    let Some(pending) = file_explorer::external_drag::unclaimed() else {
        if state.client_input.content_drop_hover_active {
            fb.host_mut().set_content_drop_hover(None);
            state.client_input.content_drop_hover_active = false;
        }
        return;
    };
    // Files is hosted in-process, so its drag does not pass through the
    // Wayland data-device callbacks which normally populate the compositor's
    // carried-content visual. Prime that visual as soon as the host can see
    // the gesture; it is then the same preview whether the eventual target is
    // another client or shell chrome.
    let label = crate::compositor::drag_feedback::label_for_paths(&pending.paths);
    if state.selection.dnd_label.get().as_deref() != Some(label.as_str()) {
        state.selection.dnd_label.set(Some(label));
        crate::compositor::selection::request_path_drag_preview(
            &pending.paths,
            state.selection.dnd_label.clone(),
        );
    }
    let (x, y) = state.client_input.cursor();
    let at = vello::kurbo::Point::new(x, y);
    if fb.host().has_content_drop_target(at) {
        // Shell placement wins over offering the payload to the app beneath
        // the pointer. Keep the hosted drag unclaimed until button-up so
        // `finish_chrome_drag_into_shell` can commit this exact target.
        fb.host_mut().set_content_drop_hover(Some(at));
        state.client_input.content_drop_hover_active = true;
        return;
    }
    if state.client_input.content_drop_hover_active {
        fb.host_mut().set_content_drop_hover(None);
        state.client_input.content_drop_hover_active = false;
    }
    tracing::debug!(
        target: "shell::selection",
        grab = ?state.client_input.grab,
        captured = chrome_captures_input(fb),
        cursor = ?state.client_input.cursor(),
        paths = ?pending.paths,
        "files drag pending"
    );
    if state.client_input.grab != Some(PointerOwner::Chrome) {
        tracing::debug!(
            target: "shell::selection",
            grab = ?state.client_input.grab,
            paths = ?pending.paths,
            "files drag pending but the chrome does not hold the pointer grab"
        );
        return;
    }
    if chrome_captures_input(fb) {
        return;
    }
    let hit = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        foreign_tile_at(&tree, fb.host(), state.client_input.cursor())
            .filter(|h| state.client_input.surfaces.contains_key(&h.surface_id))
    };
    let Some(hit) = hit else {
        return;
    };
    let Some(drag) = file_explorer::external_drag::hand_off() else {
        return;
    };
    let drag = crate::compositor::selection::ChromeDrag::new(drag.paths);
    tracing::info!(
        target: "shell::selection",
        paths = ?drag.paths,
        mime_types = ?drag.mime_types,
        surface = hit.surface_id,
        "starting a chrome drag over a client"
    );
    let metadata = smithay::wayland::selection::data_device::SourceMetadata {
        mime_types: drag.mime_types.clone(),
        dnd_action:
            smithay::reexports::wayland_server::protocol::wl_data_device_manager::DndAction::Copy,
    };
    // This drag's file names are known, so the picture the compositor draws over the desktop can say which file rather than which kind.
    state
        .selection
        .dnd_label
        .set(Some(crate::compositor::drag_feedback::label_for_paths(
            &drag.paths,
        )));
    crate::compositor::selection::request_path_drag_preview(
        &drag.paths,
        state.selection.dnd_label.clone(),
    );
    state.selection.chrome_drag = Some(drag);
    let serial = mint_serial(state);
    let location = SmithayPoint::from(state.client_input.cursor());
    // The gesture began on chrome, so there is no surface focus to record; the grab learns its target from the motion delivered right after.
    let start = smithay::input::pointer::GrabStartData {
        focus: None,
        button: BTN_LEFT,
        location,
    };
    let dh = state.display_handle.clone();
    let seat = state.seat.clone();
    smithay::wayland::selection::data_device::start_dnd(
        &dh,
        &seat,
        state,
        serial,
        Some(start),
        None,
        metadata,
    );
    deliver_pointer(state, Some(hit));
}

/// Finish a hosted Files drag on shell-owned window chrome/content.
///
/// This runs before the release is dispatched back into Files. Claiming the
/// process-global drag first makes Files cancel its own folder move, while the
/// paths are opened through the exact same retained-geometry resolver used by
/// client `DnD` and `WindowEvent::DroppedFile`.
pub fn finish_chrome_drag_into_shell(
    state: &mut State,
    fb: &mut FrameBuilder,
    event: &WindowEvent,
) -> bool {
    if !matches!(
        event,
        WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        }
    ) || state.selection.chrome_drag.is_some()
        || state.selection.dnd_active
    {
        return false;
    }
    let Some(pending) = file_explorer::external_drag::unclaimed() else {
        return false;
    };
    let (x, y) = state.client_input.cursor();
    let at = vello::kurbo::Point::new(x, y);
    let Some(placement) = fb.host().content_drop_placement(at) else {
        return false;
    };
    let Some(drag) = file_explorer::external_drag::hand_off() else {
        return false;
    };
    debug_assert_eq!(drag.paths, pending.paths);
    let changed = drag
        .paths
        .iter()
        .enumerate()
        .fold(false, |changed, (offset, path)| {
            let placement = match placement {
                crate::shell::tab_binding::Placement::TabAt(group, index) => {
                    crate::shell::tab_binding::Placement::TabAt(group, index.saturating_add(offset))
                }
                crate::shell::tab_binding::Placement::GroupAt(strip, index) => {
                    crate::shell::tab_binding::Placement::GroupAt(
                        strip,
                        index.saturating_add(offset),
                    )
                }
                other => other,
            };
            fb.host().open_dropped_path_with_placement(path, placement) || changed
        });
    state.selection.dnd_label.set(None);
    changed
}

/// Where the pointer should be focused right now: the foreign tile under the cursor, or the implicit grab's owner while a button is held.
#[allow(
    clippy::too_many_lines,
    reason = "Pointer targeting is an ordered routing policy: separating the Chrome, client drag-and-drop, implicit-grab, and capture cases could reorder focus delivery during a held gesture."
)]
fn current_pointer_target(state: &State, fb: &FrameBuilder, captured: bool) -> Option<TileHit> {
    if state.client_input.grab.is_none()
        && fb.host().dictation_pointer_at(state.client_input.cursor())
    {
        return None;
    }
    match state.client_input.grab {
        // A file carried out of the hosted file manager: the drop target is whichever client the pointer is over, so hit-test normally, as for a client's own drag below.
        Some(PointerOwner::Chrome) if state.selection.chrome_drag.is_some() => {
            if captured {
                return None;
            }
            let chrome = fb.chrome();
            let tree = chrome.tree();
            foreign_tile_at(&tree, fb.host(), state.client_input.cursor())
                .filter(|h| state.client_input.surfaces.contains_key(&h.surface_id))
        }
        // A grab owned by the chrome keeps the client out of the loop even when the cursor wanders across a tile mid-drag.
        Some(PointerOwner::Chrome) => None,
        // Drag-and-drop is the one held-button gesture *supposed* to leave
        // the surface it started on — pinning focus to the source would mean no `wl_data_device.enter` reaches another client, so it hit-tests normally in flight.
        Some(PointerOwner::Client(_)) if state.selection.dnd_active => {
            if captured {
                return None;
            }
            let chrome = fb.chrome();
            let tree = chrome.tree();
            foreign_tile_at(&tree, fb.host(), state.client_input.cursor())
                .filter(|h| state.client_input.surfaces.contains_key(&h.surface_id))
        }
        Some(PointerOwner::Client(surface_id)) => {
            if let Some(rect) = state.output_presentation_rect(surface_id) {
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            if state.osk_surface == Some(surface_id) {
                let (surface_id, rect) = fb.osk_surface_hit(state.client_input.cursor())?;
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            if state.is_floating_toplevel(surface_id) {
                let rect = fb.floating_surface_rect(surface_id)?;
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            if state.is_transient_surface(surface_id) {
                let rect = fb.transient_surface_rect(surface_id)?;
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            if let Some((fullscreen_id, rect)) = fb.fullscreen_surface_rect()
                && fullscreen_id == surface_id
            {
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            let (strip, group) = *state.client_input.placements.get(&surface_id)?;
            let rect = fb.host().active_tile_rect(group)?;
            Some(TileHit {
                surface_id,
                strip,
                group,
                rect,
            })
        }
        None => {
            if captured {
                return None;
            }
            if let Some(hit) = state.output_presentation_hit(state.client_input.cursor()) {
                return Some(hit);
            }
            if let Some((surface_id, rect)) = fb.osk_surface_hit(state.client_input.cursor())
                && state.osk_surface == Some(surface_id)
                && state.client_input.surfaces.contains_key(&surface_id)
            {
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            if let Some((surface_id, rect)) = fb.transient_surface_hit(state.client_input.cursor())
                && state.client_input.surfaces.contains_key(&surface_id)
            {
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            if let Some((surface_id, rect)) = fb.fullscreen_surface_hit(state.client_input.cursor())
                && state.client_input.surfaces.contains_key(&surface_id)
            {
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            if let Some((surface_id, rect)) = fb.floating_surface_hit(state.client_input.cursor())
                && state.client_input.surfaces.contains_key(&surface_id)
            {
                return Some(TileHit {
                    surface_id,
                    strip: StripId::default(),
                    group: 0,
                    rect,
                });
            }
            let chrome = fb.chrome();
            let tree = chrome.tree();
            foreign_tile_at(&tree, fb.host(), state.client_input.cursor())
                .filter(|h| state.client_input.surfaces.contains_key(&h.surface_id))
        }
    }
}

/// Push the current cursor position at `hit` (or clear client focus when `None`), producing the enter/motion/leave sequence.
fn deliver_pointer(state: &mut State, hit: Option<TileHit>) {
    let global = state.client_input.cursor();
    let target = hit.and_then(|h| route_tile_point(h.surface_id, h.rect, global));
    if state.selection.dnd_active {
        tracing::debug!(
            target: "shell::selection",
            cursor = ?global,
            over_surface = ?target.map(|(id, _)| id),
            "routing pointer during a drag"
        );
    }
    state.forward_client_pointer_motion(global, target);
}

/// Make `group` the active group of `strip` in the chrome's tree so the shell's focus rules agree with what the user just clicked.
fn raise_group(fb: &mut FrameBuilder, strip: StripId, group: GroupId) -> bool {
    let mut changed = false;
    fb.with_chrome(|chrome| {
        {
            let mut tree = chrome.tree_mut();
            let workspace = tree.desktop.active;
            changed = tree.desktop.active_strip.get(&workspace).copied() != Some(strip)
                || tree.desktop.active_group_for(strip) != Some(group);
            if changed {
                tree.desktop.active_strip.insert(workspace, strip);
                tree.desktop.set_active_group(strip, group);
            }
        }
        if changed {
            chrome.bump();
        }
    });
    changed
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic pointer-coordinate fixtures use exactly representable values"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;
    use crate::shell::persist;
    use winit::event::DeviceId;
    use winit::keyboard::{Key, NamedKey};

    fn key_event(logical: Key, pressed: bool) -> WindowEvent {
        let ev = ui::synthetic_input::key_event(
            logical,
            None,
            if pressed {
                ElementState::Pressed
            } else {
                ElementState::Released
            },
        );
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed::<DeviceId>() },
            event: ev,
            is_synthetic: false,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn pointer_focus_routing_between_clients_and_chrome() {
        // Phase `moving_from_a_client_into_maximized_chrome_reaches_the_widget`.
        {
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let mut seed = crate::shell::state::WorkspaceTree::bootstrap_demo();
            let first = &mut seed.desktop.workspaces[0].strips[0].window_groups[0];
            first.maximized = true;
            let group = first.id;
            let tree = seed.shared();
            let mut fb = FrameBuilder::with_handle(tree.clone(), false, (1280, 800));
            fb.with_chrome(|chrome| chrome.set_maximized_group(Some(group)));
            let _ = fb.paint_settled();

            let event_loop: EventLoop<'static, crate::compositor::state::State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<crate::compositor::state::State> =
                Display::new().expect("wayland display");
            let mut state = crate::compositor::state::State::new_with_workspace_tree(
                event_loop.handle(),
                display.handle(),
                tree,
            );
            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1210.0, 70.0),
            };

            assert!(
                !route_input(&mut state, &mut fb, &moved, &Modifiers::default(),),
                "captured motion must fall through to shell widget dispatch"
            );
            assert_eq!(
                fb.host().pointer_position(),
                Some(vello::kurbo::Point::new(1210.0, 70.0)),
            );
            assert!(chrome_captures_input(&fb));
        }
        // Phase `clicking_a_window_defers_focus_persistence_until_after_input_flush`.
        {
            let state_dir = tempfile::tempdir().expect("temporary state directory");
            let state_path = state_dir.path().join("state.json");
            let mut fb = FrameBuilder::new((1280, 800));
            let (strip_id, first_group, second_group) = {
                let chrome = fb.chrome();
                let mut tree = chrome.tree_mut();
                tree.persist_path = Some(state_path.clone());
                let (strip_id, first_group) =
                    insert_foreign_group(&mut tree, 710_001, Some("org.example.first"))
                        .expect("first foreign group");
                let (_, second_group) =
                    insert_foreign_group(&mut tree, 710_002, Some("org.example.second"))
                        .expect("second foreign group");
                tree.desktop.set_active_group(strip_id, first_group);
                tree.queue_save();
                drop(tree);
                (strip_id, first_group, second_group)
            };

            assert!(raise_group(&mut fb, strip_id, second_group));

            let before_flush = persist::load_tree_from(&state_path).expect("reload old layout");
            assert_eq!(
                before_flush.desktop.active_group_for(strip_id),
                Some(first_group),
                "durable I/O must not run in the pointer-to-focus critical path"
            );

            // The native loop performs this only after `display.flush_clients()`.
            fb.chrome().tree().queue_save();

            let restarted = persist::load_tree_from(&state_path).expect("reload focused layout");
            assert_eq!(
                restarted.desktop.active_group_for(strip_id),
                Some(second_group),
                "the window focused by a click must remain selected after restart"
            );
            assert_eq!(
                restarted
                    .desktop
                    .active_strip
                    .get(&restarted.desktop.active)
                    .copied(),
                Some(strip_id),
                "the clicked group's strip is part of the durable focus address"
            );
            assert_ne!(
                restarted.desktop.active_group_for(strip_id),
                Some(first_group),
                "restart must not restore the group focused before the click"
            );
        }
        // Phase `focus_persistence_request_is_coalesced_and_edge_triggered`.
        {
            let mut input = ClientInput {
                focus_persistence_pending: true,
                ..ClientInput::default()
            };
            // A second request before the flush coalesces into the first.
            input.focus_persistence_pending = true;

            assert!(input.take_focus_persistence_pending());
            assert!(
                !input.take_focus_persistence_pending(),
                "an idle tick must not rewrite and fsync the workspace tree again"
            );
        }
        // Phase `touching_a_tiled_window_promotes_its_exact_group`.
        {
            use smithay::reexports::calloop::EventLoop;
            use smithay::reexports::wayland_server::Display;

            let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
            let mut fb = FrameBuilder::with_handle(tree.clone(), false, (1280, 800));
            let (strip_id, first_group, second_group, second_surface) = {
                let chrome = fb.chrome();
                let mut tree = chrome.tree_mut();
                let (strip_id, first_group) =
                    insert_foreign_group(&mut tree, 720_001, Some("org.example.first"))
                        .expect("first foreign group");
                let (_, second_group) =
                    insert_foreign_group(&mut tree, 720_002, Some("org.example.second"))
                        .expect("second foreign group");
                tree.desktop.set_active_group(strip_id, first_group);
                drop(tree);
                (strip_id, first_group, second_group, 720_002)
            };
            let event_loop: EventLoop<'static, crate::compositor::state::State> =
                EventLoop::try_new().expect("calloop event loop");
            let display: Display<crate::compositor::state::State> =
                Display::new().expect("wayland display");
            let mut state = crate::compositor::state::State::new_with_workspace_tree(
                event_loop.handle(),
                display.handle(),
                tree,
            );

            focus_touch_target(&mut state, &mut fb, second_surface);

            let chrome = fb.chrome();
            assert_eq!(
                chrome.tree().desktop.active_group_for(strip_id),
                Some(second_group),
                "direct touch must focus the tiled group under the finger"
            );
            assert_ne!(
                chrome.tree().desktop.active_group_for(strip_id),
                Some(first_group),
                "touch must not leave keyboard focus attached to the previous tiled app"
            );
        }
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "tree is read for every probe in the loop below; the block already ends \
                  right after the loop, so there's no earlier point to drop the guard"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn published_remote_input_view_matches_local_state() {
        // Phase `onboarding_mutes_wayland_focus_and_the_remote_input_view`.
        {
            const HIDDEN_SURFACE: u64 = 424_242;
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            fb.host_mut()
                .enable_onboarding(std::path::PathBuf::from("/tmp/tilo-oobe-input-test"));
            let _ = fb.paint_settled();

            assert!(
                chrome_captures_input(&fb),
                "compositor-owned OOBE must capture every client input route"
            );

            assert_eq!(
                gate_keyboard_focus(Some(HIDDEN_SURFACE), chrome_captures_input(&fb)),
                None,
                "a hidden Wayland client cannot retain keyboard focus"
            );
            let remote_view = crate::network::input_firewall::ShellView::new(
                vec![(HIDDEN_SURFACE, Rect::new(0.0, 0.0, 400.0, 300.0))],
                gate_keyboard_focus(Some(HIDDEN_SURFACE), chrome_captures_input(&fb)),
                chrome_captures_input(&fb),
                false,
            );
            assert!(
                remote_view.chrome_captures_input(),
                "remote peers must see OOBE as occluding trusted chrome"
            );
        }
        // Phase `the_firewall_view_and_local_hit_testing_resolve_the_same_surface`.
        // The remote-input firewall's z-order check and local pointer routing must never disagree about what is on top — the disagreement *is* the clickjacking hole; they share [`foreign_tiles`].
        {
            use crate::network::input_firewall::ShellView;

            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            fb.with_chrome(|chrome| {
                {
                    let mut tree = chrome.tree_mut();
                    insert_foreign_group(&mut tree, 900_001, Some("org.example.one"));
                    insert_foreign_group(&mut tree, 900_002, Some("org.example.two"));
                }
                chrome.bump();
            });
            let _ = fb.paint_settled();

            let tiles = {
                let chrome = fb.chrome();
                let tree = chrome.tree();
                foreign_tiles(&tree, fb.host())
            };
            assert_eq!(
                tiles.len(),
                2,
                "both mirrored clients must be laid out; got {tiles:?}"
            );

            let view = ShellView::new(
                tiles.iter().map(|h| (h.surface_id, h.rect)).collect(),
                None,
                false,
                false,
            )
            .chrome_regions(
                tiles
                    .iter()
                    .filter_map(|hit| {
                        fb.host()
                            .group_resize_rect(hit.group)
                            .map(|rect| (hit.surface_id, rect))
                    })
                    .collect(),
            );

            // Probe each tile's inside and just past each edge — a hit-test discrepancy shows up at a boundary first, derived from the layout's actual rects.
            let mut probes: Vec<(f64, f64)> = Vec::new();
            for hit in &tiles {
                let r = hit.rect;
                let c = r.center();
                probes.extend([
                    (c.x, c.y),
                    (r.x0 + 0.5, r.y0 + 0.5),
                    (r.x1 - 0.5, r.y1 - 0.5),
                    (r.x0 - 0.5, c.y),
                    (r.x1 + 0.5, c.y),
                    (c.x, r.y0 - 0.5),
                    (c.x, r.y1 + 0.5),
                ]);
            }
            // Coarse sweep of the whole output (mostly chrome): every one must resolve to nothing on both sides, or the firewall would clear a click on the top bar.
            for gx in 0..80 {
                for gy in 0..45 {
                    probes.push((f64::from(gx) * 20.0 + 10.0, f64::from(gy) * 20.0 + 10.0));
                }
            }

            // Wherever local input would land on a client, the firewall must name that same client; wherever it lands on chrome, the firewall must name nothing.
            let mut inside = 0;
            {
                let chrome = fb.chrome();
                let tree = chrome.tree();
                for p in probes {
                    let local = foreign_tile_at(&tree, fb.host(), p).map(|h| h.surface_id);
                    assert_eq!(
                        view.topmost_at(p),
                        local,
                        "at {p:?} the firewall and the pointer router disagree",
                    );
                    if local.is_some() {
                        inside += 1;
                    }
                }
            }
            assert!(
                inside >= tiles.len() * 3,
                "only {inside} probes landed inside a tile across {} tiles, so \
                 agreement on a hit was never actually tested",
                tiles.len(),
            );
        }
        // Phase `a_tab_whose_surface_the_compositor_does_not_hold_is_not_published`.
        // A tab is a place input can land only while the compositor still holds
        // the client's surface — one left in the tree after its toplevel went
        // away is a rectangle nothing is behind, and publishing it would hand a
        // guest a coordinate the local user can't even click on. Drives the real publisher against the real laid-out tree, then asks the firewall the question a peer's event asks.
        {
            use crate::network::input_firewall::Denial;
            use crate::network::streaming::{InputEvent, StreamingShare};

            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            fb.with_chrome(|chrome| {
                {
                    let mut tree = chrome.tree_mut();
                    insert_foreign_group(&mut tree, ORPHAN_SURFACE, Some("org.example.gone"));
                }
                chrome.bump();
            });
            let _ = fb.paint_settled();

            // The tile is laid out — the tree says there is a window there.
            assert!(
                foreign_tiles(&fb.chrome().tree(), fb.host())
                    .iter()
                    .any(|hit| hit.surface_id == ORPHAN_SURFACE),
                "the mirrored tab has to be laid out, or this proves nothing",
            );

            // The compositor never registered a toplevel for it.
            let mut state = state_sharing(ORPHAN_SURFACE);
            state
                .share_registry
                .insert(KILL_PEER, ORPHAN_SURFACE, StreamingShare::new());
            publish_remote_input_view(&mut state, &fb, false);

            let view = state.remote_input_firewall.view();
            assert!(view.observed(), "the publisher has to publish something");
            assert!(
                view.rect(ORPHAN_SURFACE).is_none(),
                "a tab with no live surface behind it was offered as an input target",
            );
            assert_eq!(
                state.review_remote_input(
                    KILL_PEER,
                    ORPHAN_SURFACE,
                    InputEvent::PointerMotion { x: 5.0, y: 5.0 }
                ),
                Err(Denial::NotOnScreen),
            );
        }
        // Phase `a_locked_session_is_carried_into_the_published_view`.
        // The lock surface is drawn by the session, not the tiled tree, so tiles
        // beneath it stay laid out and topmost as far as the tree knows — the publisher must carry the lock across, or a guest keeps driving a password-protected window.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            fb.with_chrome(|chrome| {
                {
                    let mut tree = chrome.tree_mut();
                    insert_foreign_group(&mut tree, ORPHAN_SURFACE, Some("org.example.locked"));
                }
                chrome.bump();
            });
            let _ = fb.paint_settled();

            let mut state = state_sharing(ORPHAN_SURFACE);
            publish_remote_input_view(&mut state, &fb, false);
            assert!(
                !state.remote_input_firewall.view().session_locked(),
                "an unlocked session must not publish itself as locked",
            );

            publish_remote_input_view(&mut state, &fb, true);
            assert!(state.remote_input_firewall.view().session_locked());
        }
        // Local microphone decisions mute remote control without giving up
        // the original local editable field's keyboard owner.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            let mut state = state_sharing(ORPHAN_SURFACE);
            publish_remote_input_view(&mut state, &fb, false);
            assert!(!state.remote_input_firewall.view().chrome_captures_input());

            fb.host_mut()
                .dictation_surface_mut()
                .failure(crate::compositor::dictation::DictationError::NoEditableField);
            assert!(
                !chrome_captures_input(&fb),
                "dictation controls must retain the local field's IME owner"
            );
            publish_remote_input_view(&mut state, &fb, false);
            assert!(
                state.remote_input_firewall.view().chrome_captures_input(),
                "a peer cannot operate the local microphone controls"
            );

            fb.host_mut()
                .dictation_surface_mut()
                .status(crate::compositor::dictation::DictationStatus::Idle);
            publish_remote_input_view(&mut state, &fb, false);
            assert!(
                !state.remote_input_firewall.view().chrome_captures_input(),
                "closing dictation must restore normal shared-seat policy"
            );
        }
    }

    const ORPHAN_SURFACE: u64 = 900_003;

    #[test]
    fn chrome_pointer_position_tracks_routed_motion() {
        // Phase `a_pointer_that_has_never_moved_has_no_position`.
        // A session nobody has pointed at holds no pointer position, and holds
        // one the instant a real motion is routed through. The two accessors
        // disagree by design: `cursor()` answers the origin (hit-testing needs a
        // point), `pointer_position` answers `None` — publishing the origin would announce a cursor for somebody who isn't there.
        {
            let mut state = state_sharing(9_000_101);
            let mut fb = FrameBuilder::new((1600, 900));
            let mods = Modifiers::default();

            assert!(
                state.client_input.pointer_position().is_none(),
                "nothing has moved a pointer through this session, so it has no position to report",
            );
            assert_eq!(
                state.client_input.cursor(),
                (0.0, 0.0),
                "while the routing accessor still answers the origin - the very confusion the other one exists to prevent",
            );

            // The one thing that gives a session a pointer, through the function every source of motion goes through.
            route_input(
                &mut state,
                &mut fb,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(640.0, 610.0),
                },
                &mods,
            );

            assert_eq!(
                state.client_input.pointer_position(),
                Some((640.0, 610.0)),
                "and once it has been somewhere, that is where it is",
            );
        }
        // Phase `routing_a_motion_keeps_the_chrome_pointer_current`.
        // Routing a motion tells the chrome where the pointer went, whether or
        // not the chrome is the one that gets to read the event — the chrome
        // hit-tests a press against the last position it was given, since
        // `MouseInput` carries none of its own. Motion over a client window is withheld from the chrome's own dispatch, so without this its pointer idea would freeze at its last chrome-side position.
        {
            let mut state = state_sharing(9_000_103);
            let mut fb = FrameBuilder::new((1600, 900));
            let mods = Modifiers::default();

            assert!(
                fb.host().pointer_position().is_none(),
                "nothing has moved a pointer through this session yet",
            );

            route_input(
                &mut state,
                &mut fb,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(523.0, 339.0),
                },
                &mods,
            );
            assert_eq!(
                fb.host().pointer_position(),
                Some(vello::kurbo::Point::new(523.0, 339.0)),
                "the chrome has to know where the pointer is to hit-test the press that follows it",
            );

            // And it follows the pointer, rather than only recording the first place it was seen.
            route_input(
                &mut state,
                &mut fb,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(880.0, 300.0),
                },
                &mods,
            );
            assert_eq!(
                fb.host().pointer_position(),
                Some(vello::kurbo::Point::new(880.0, 300.0)),
            );
        }
    }

    /// A `State` with a live seat, one peer holding write access to `surface`, and that surface on screen with the keyboard.
    fn state_sharing(surface: u64) -> crate::compositor::state::State {
        use crate::network::input_firewall::ShellView;
        use crate::network::streaming::StreamingShare;
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        let event_loop: EventLoop<'static, crate::compositor::state::State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<crate::compositor::state::State> =
            Display::new().expect("wayland display");
        let mut state = crate::compositor::state::State::new(event_loop.handle(), display.handle());
        state
            .share_registry
            .insert(KILL_PEER, surface, StreamingShare::new());
        state.remote_input_firewall.publish_view(ShellView::new(
            vec![(surface, Rect::new(0.0, 0.0, 400.0, 300.0))],
            Some(surface),
            false,
            false,
        ));
        state
    }

    /// A bare production seat used to test the lock/device-loss key-state
    /// boundary without inventing an in-process text widget.
    fn seat_state() -> crate::compositor::state::State {
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        let event_loop: EventLoop<'static, crate::compositor::state::State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<crate::compositor::state::State> =
            Display::new().expect("wayland display");
        crate::compositor::state::State::new(event_loop.handle(), display.handle())
    }

    fn seat_has_evdev_key(state: &crate::compositor::state::State, evdev: u32) -> bool {
        state
            .seat
            .get_keyboard()
            .expect("seat keyboard")
            .pressed_keys()
            .iter()
            .any(|code| u32::from(*code) == evdev.saturating_add(8))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn remote_and_local_holds_retire_independently() {
        // Phase `focus_change_retires_remote_keys_before_the_next_legacy_app`.
        // A remote key held on app A must be retired before the shared Wayland
        // seat focuses app B. The peer keeps its logical caret target on A, and
        // a late network release is harmless rather than landing in B.
        {
            use crate::network::input_firewall::{Denial, ShellView};
            use crate::network::streaming::InputEvent;

            const SHARED: u64 = 808;
            const PRIVATE: u64 = 909;
            const LEFT_SHIFT: u32 = 42;

            let mut state = state_sharing(SHARED);
            state.client_input.keyboard_focus = Some(SHARED);
            let _ = state.remote_seats.seat_mut(KILL_PEER).enter(SHARED);
            state
                .review_remote_input(
                    KILL_PEER,
                    SHARED,
                    InputEvent::Key {
                        keysym: LEFT_SHIFT,
                        pressed: true,
                        modifiers: 0,
                    },
                )
                .expect("focused collaborator presses Shift");
            assert!(seat_has_evdev_key(&state, LEFT_SHIFT));

            state.set_client_keyboard_focus(None);
            assert!(!seat_has_evdev_key(&state, LEFT_SHIFT));
            assert_eq!(
                state
                    .remote_seats
                    .seat(KILL_PEER)
                    .and_then(|seat| seat.focused_surface),
                Some(SHARED),
                "temporary host focus changes preserve the remote caret target"
            );

            state.remote_input_firewall.publish_view(ShellView::new(
                vec![(SHARED, Rect::new(0.0, 0.0, 400.0, 300.0))],
                Some(PRIVATE),
                false,
                false,
            ));
            assert_eq!(
                state.review_remote_input(
                    KILL_PEER,
                    SHARED,
                    InputEvent::Key {
                        keysym: LEFT_SHIFT,
                        pressed: false,
                        modifiers: 0,
                    },
                ),
                Err(Denial::FocusMuted)
            );
            assert!(!seat_has_evdev_key(&state, LEFT_SHIFT));
        }
        // Phase `overlapping_remote_button_holds_keep_one_legacy_drag_alive`.
        // Two remote pointers are multiplexed into one legacy `wl_pointer`.
        // Releasing one participant's left button must not end the other one's
        // implicit drag; only the last release reaches Smithay.
        {
            use crate::network::streaming::{InputEvent, StreamingShare};

            const SHARED: u64 = 808;
            const SECOND_PEER: u64 = 78;
            const LEFT: u32 = 272;

            let mut state = state_sharing(SHARED);
            state
                .share_registry
                .insert(SECOND_PEER, SHARED, StreamingShare::new());
            for peer in [KILL_PEER, SECOND_PEER] {
                state
                    .review_remote_input(
                        peer,
                        SHARED,
                        InputEvent::PointerMotion { x: 20.0, y: 20.0 },
                    )
                    .expect("participant positions their pointer in the shared surface");
                state
                    .review_remote_input(
                        peer,
                        SHARED,
                        InputEvent::PointerButton {
                            button: LEFT,
                            pressed: true,
                        },
                    )
                    .expect("participant begins a drag");
            }
            assert_eq!(state.remote_seats.button_holder_count(LEFT), 2);

            state
                .review_remote_input(
                    KILL_PEER,
                    SHARED,
                    InputEvent::PointerButton {
                        button: LEFT,
                        pressed: false,
                    },
                )
                .expect("first participant releases");
            assert_eq!(
                state.remote_seats.button_holder_count(LEFT),
                1,
                "the second participant still owns the legacy drag"
            );

            state
                .review_remote_input(
                    SECOND_PEER,
                    SHARED,
                    InputEvent::PointerButton {
                        button: LEFT,
                        pressed: false,
                    },
                )
                .expect("final participant releases");
            assert_eq!(state.remote_seats.button_holder_count(LEFT), 0);
        }
        // Phase `remote_click_restores_that_participants_cursor_after_another_peer_moves`.
        {
            use crate::network::streaming::{InputEvent, StreamingShare};

            const SHARED: u64 = 808;
            const SECOND_PEER: u64 = 78;
            const LEFT: u32 = 272;

            let mut state = state_sharing(SHARED);
            state
                .share_registry
                .insert(SECOND_PEER, SHARED, StreamingShare::new());
            state
                .review_remote_input(
                    KILL_PEER,
                    SHARED,
                    InputEvent::PointerMotion { x: 20.0, y: 30.0 },
                )
                .expect("first peer positions cursor");
            state
                .review_remote_input(
                    SECOND_PEER,
                    SHARED,
                    InputEvent::PointerMotion { x: 170.0, y: 180.0 },
                )
                .expect("second peer moves the shared Wayland pointer last");
            assert_eq!(
                state
                    .remote_seats
                    .seat(KILL_PEER)
                    .and_then(|seat| seat.pointer_surface),
                Some(SHARED),
                "accepted motion attributes pointer-only control for later revocation"
            );
            assert_eq!(
                state
                    .seat
                    .get_pointer()
                    .expect("pointer")
                    .current_location(),
                (170.0, 180.0).into()
            );

            state
                .review_remote_input(
                    KILL_PEER,
                    SHARED,
                    InputEvent::PointerButton {
                        button: LEFT,
                        pressed: true,
                    },
                )
                .expect("first peer clicks at their own retained cursor");
            assert_eq!(
                state
                    .seat
                    .get_pointer()
                    .expect("pointer")
                    .current_location(),
                (20.0, 30.0).into(),
                "the click must not land at the other participant's cursor"
            );
        }
        // Phase `remote_pointer_leave_ends_drag_but_preserves_caret_focus`.
        {
            use crate::network::streaming::InputEvent;

            const SHARED: u64 = 808;
            const LEFT: u32 = 272;
            let mut state = state_sharing(SHARED);
            for event in [
                InputEvent::PointerEnter { surface: SHARED },
                InputEvent::PointerMotion { x: 20.0, y: 30.0 },
                InputEvent::PointerButton {
                    button: LEFT,
                    pressed: true,
                },
            ] {
                state
                    .review_remote_input(KILL_PEER, SHARED, event)
                    .expect("remote pointer setup");
            }
            assert_eq!(state.remote_seats.button_holder_count(LEFT), 1);

            state
                .review_remote_input(
                    KILL_PEER,
                    SHARED,
                    InputEvent::PointerLeave { surface: SHARED },
                )
                .expect("remote pointer leaves");
            assert_eq!(state.remote_seats.button_holder_count(LEFT), 0);
            assert_eq!(
                state
                    .remote_seats
                    .seat(KILL_PEER)
                    .and_then(|seat| seat.focused_surface),
                Some(SHARED),
                "pointer leave must not destroy the legacy text caret"
            );
        }
        // Phase `pointer_leave_retires_local_hold_without_ending_remote_drag`.
        {
            const PEER: u64 = 77;
            const LEFT: u32 = 272;

            let mut state = seat_state();
            assert!(state.remote_seats.local_button_transition(LEFT, true));
            assert!(!state.remote_seats.button_transition(PEER, LEFT, true));
            state.client_input.buttons_down = 1;

            assert!(!route_cursor_left(&mut state));
            assert_eq!(state.client_input.buttons_down, 0);
            assert_eq!(
                state.remote_seats.button_holder_count(LEFT),
                1,
                "pointer loss retires only the host's ownership"
            );
            assert!(state.remote_seats.button_transition(PEER, LEFT, false));
        }
        // Phase `chrome_drag_release_bypasses_an_absent_client_press`.
        {
            const LEFT: u32 = 272;

            let mut state = seat_state();
            assert_eq!(state.remote_seats.button_holder_count(LEFT), 0);
            assert!(
                admit_local_pointer_edge(&mut state, LEFT, false, true),
                "the DnD grab must receive the release even though Chrome owned the press"
            );
            assert_eq!(
                state.remote_seats.button_holder_count(LEFT),
                0,
                "the exceptional release must not invent shared button ownership"
            );
            assert!(
                !admit_local_pointer_edge(&mut state, LEFT, false, false),
                "ordinary duplicate releases remain suppressed"
            );
        }
    }

    #[test]
    fn captured_keyboard_edges_retire_only_their_own_keys() {
        // Phase `captured_keyboard_edges_retire_modifiers_and_editing_keys`.
        // A lock is a trusted input-capture boundary, not permission to stop
        // advancing the seat XKB state.  This is the direct regression for a
        // held Shift/Ctrl/Backspace whose release happens while the lock is up.
        {
            let mut state = seat_state();
            for (key, evdev) in [
                (Key::Named(NamedKey::Shift), 42),
                (Key::Named(NamedKey::Control), 29),
                (Key::Named(NamedKey::Backspace), 14),
            ] {
                consume_keyboard_while_captured(&mut state, &key_event(key.clone(), true));
                assert!(
                    seat_has_evdev_key(&state, evdev),
                    "captured press for {key:?} must still reach the seat XKB state"
                );
                consume_keyboard_while_captured(&mut state, &key_event(key, false));
                assert!(
                    !seat_has_evdev_key(&state, evdev),
                    "captured release for evdev {evdev} must clear the seat state"
                );
            }
        }
        // Phase `captured_release_retires_a_preexisting_client_key_ledger_entry`.
        // A lock/modal can begin after a client saw the press but before its
        // physical release. The captured release must consume the same routing
        // ledger entry, otherwise a later unrelated release could be forwarded
        // as an unmatched key-up after focus returns.
        {
            let mut state = seat_state();
            state.client_input.forwarded_keys.insert(14);
            consume_keyboard_while_captured(
                &mut state,
                &key_event(Key::Named(NamedKey::Backspace), false),
            );
            assert!(
                state.client_input.forwarded_keys.is_empty(),
                "the captured Backspace release must retire the prior client press"
            );
        }
        // Phase `captured_release_preserves_another_participants_held_modifier`.
        {
            const PEER: u64 = 77;
            const SHIFT: u32 = 42;
            let mut state = seat_state();
            assert!(state.remote_seats.key_transition(PEER, SHIFT, true));
            state.deliver_client_key(SHIFT, true, true);
            state.client_input.forwarded_keys.insert(SHIFT);
            // A local release with no recorded local press must not release the
            // peer's modifier merely because capture cleared application focus.
            consume_keyboard_while_captured(
                &mut state,
                &key_event(Key::Named(NamedKey::Shift), false),
            );
            assert!(seat_has_evdev_key(&state, SHIFT));
            assert!(state.client_input.forwarded_keys.contains(&SHIFT));
            assert_eq!(state.remote_seats.key_holder_count(SHIFT), 1);

            consume_keyboard_while_captured(
                &mut state,
                &key_event(Key::Named(NamedKey::Shift), true),
            );
            assert_eq!(state.remote_seats.key_holder_count(SHIFT), 2);
            consume_keyboard_while_captured(
                &mut state,
                &key_event(Key::Named(NamedKey::Shift), false),
            );
            assert!(seat_has_evdev_key(&state, SHIFT));
            assert!(state.client_input.forwarded_keys.contains(&SHIFT));
            assert_eq!(state.remote_seats.key_holder_count(SHIFT), 1);
        }
        // Phase `keyboard_state_reset_clears_all_pressed_keys_without_client_focus`.
        // VT/device loss has no reliable release edge.  Resetting the seat,
        // rather than only stopping repeat, prevents that lost Backspace/Shift
        // from leaking into the next focused application after resume.
        {
            let mut state = seat_state();
            consume_keyboard_while_captured(
                &mut state,
                &key_event(Key::Named(NamedKey::Shift), true),
            );
            consume_keyboard_while_captured(
                &mut state,
                &key_event(Key::Named(NamedKey::Backspace), true),
            );
            assert!(seat_has_evdev_key(&state, 42));
            assert!(seat_has_evdev_key(&state, 14));

            state.reset_client_keyboard_state();

            assert!(
                state
                    .seat
                    .get_keyboard()
                    .expect("seat keyboard")
                    .pressed_keys()
                    .is_empty(),
                "an input-source reset must leave no latent modifier or repeat key"
            );
            assert!(
                state.client_input.forwarded_keys.is_empty(),
                "the compositor routing ledger must retire with Smithay's seat state"
            );
            assert_eq!(state.client_input.keyboard_focus(), None);
        }
    }

    const KILL_PEER: u64 = 77;
    const KILL_SURFACE: u64 = 808;

    fn guest_motion(
        state: &mut crate::compositor::state::State,
    ) -> Result<(), crate::network::input_firewall::Denial> {
        state.review_remote_input(
            KILL_PEER,
            KILL_SURFACE,
            crate::network::streaming::InputEvent::PointerMotion { x: 10.0, y: 10.0 },
        )
    }

    #[test]
    fn stopping_remote_input_is_confirmed_on_screen_whatever_else_is_shown() {
        // Phase `stopping_remote_input_halts_the_guest_and_says_so_on_screen`.
        // The panic key, end to end from the action the chord triggers: a guest is driving the seat, the host presses it, the guest stops, and the host is told so.
        {
            use crate::network::input_firewall::Denial;

            let mut fb = FrameBuilder::new((1600, 900));
            let mut state = state_sharing(KILL_SURFACE);

            guest_motion(&mut state).expect("the guest is driving the seat to begin with");

            let revoked = stop_remote_input(&mut state, &mut fb, false);
            assert_eq!(
                revoked, 1,
                "the one writable share had to lose write access"
            );

            assert_eq!(
                guest_motion(&mut state),
                Err(Denial::KillSwitch),
                "the guest still reached the seat after the kill switch",
            );

            let chrome = fb.chrome();
            let toast = chrome
                .notifications()
                .iter()
                .find(|n| n.title == "Remote input stopped")
                .expect("the host got no confirmation that anything happened");
            assert!(
                toast.body.contains("Remote control") && toast.body.contains("sharing was stopped"),
                "the confirmation has to say what it stopped; body was {:?}",
                toast.body,
            );
            assert!(
                toast.dismiss_after >= std::time::Duration::from_secs(60),
                "a stop that lasts until the host lifts it cannot be confirmed by a \
                 toast that slides away in five seconds",
            );
        }
        // Phase `the_kill_switch_confirmation_is_shown_under_do_not_disturb`.
        // Do-Not-Disturb is a request for quiet, not to be kept uninformed about the machine's own security state — a suppressed confirmation makes a working kill switch look broken.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut state = state_sharing(KILL_SURFACE);
            fb.with_chrome(|chrome| chrome.set_dnd(true));

            stop_remote_input(&mut state, &mut fb, false);

            let chrome = fb.chrome();
            assert!(
                chrome.dnd(),
                "the test has to actually be under DND or it proves nothing",
            );
            assert!(
                chrome
                    .notifications()
                    .iter()
                    .any(|n| n.title == "Remote input stopped"),
                "the confirmation was held back into history by Do-Not-Disturb",
            );
        }
        // Phase `a_flood_of_app_toasts_cannot_push_the_confirmation_off_the_screen`.
        // The notification stack is capped, and a guest driving a client can make that client post toasts — filling the stack must not hide that the panic key worked.
        {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut state = state_sharing(KILL_SURFACE);
            fb.with_chrome(|chrome| {
                for i in 0..tilo_desktop::notifications::MAX_NOTIFICATIONS * 3 {
                    chrome.push_notification(format!("App toast {i}"), "noise");
                }
            });

            stop_remote_input(&mut state, &mut fb, false);

            let chrome = fb.chrome();
            assert!(
                chrome
                    .notifications()
                    .iter()
                    .any(|n| n.title == "Remote input stopped"),
                "a flood of app toasts buried the kill-switch confirmation",
            );
        }
    }

    #[test]
    fn stopping_remote_input_with_nobody_connected_stays_silent() {
        // A stray Super+Esc (leaving the palette with Super still held) must
        // not warn about a session nobody saw start, but still latches.
        let mut fb = FrameBuilder::new((1600, 900));
        let mut state = state_sharing(KILL_SURFACE);
        state.share_registry.clear();
        assert_eq!(state.live_remote_input_participants(), 0);

        assert_eq!(stop_remote_input(&mut state, &mut fb, false), 0);
        assert!(
            fb.chrome().notifications().is_empty(),
            "a kill switch with nothing connected raised a toast"
        );
        assert!(state.remote_input_firewall.kill_switch_engaged());

        // A device-control session outside the registry still counts.
        let mut fb = FrameBuilder::new((1600, 900));
        stop_remote_input(&mut state, &mut fb, true);
        assert!(
            fb.chrome()
                .notifications()
                .iter()
                .any(|n| n.title == "Remote input stopped")
        );
    }

    #[test]
    fn client_input_registry_survives_lifecycle_and_protocol_adoption() {
        // Phase `client_input_registry_tracks_lifecycle_without_a_surface`.
        {
            // The registry half that doesn't need a live `WlSurface`: pending adoption/removal bookkeeping and the focus reset on destroy.
            let mut ci = ClientInput::default();
            assert!(ci.is_empty());
            ci.pending_adoptions.push(PendingAdoption {
                surface_id: 5,
                app_id: None,
            });
            ci.note_app_id(5, "org.example.app".into());
            assert_eq!(
                ci.pending_adoptions[0].app_id.as_deref(),
                Some("org.example.app"),
                "an app-id that beats the mirror folds into the pending adoption"
            );
            // An app-id for an already-mirrored surface queues a rename.
            ci.note_app_id(6, "org.example.other".into());
            assert_eq!(
                ci.pending_renames,
                vec![(6, "org.example.other".to_string())]
            );
            // Destroy clears focus + grab bookkeeping.
            ci.keyboard_focus = Some(5);
            ci.pointer_focus = Some(5);
            ci.grab = Some(PointerOwner::Client(5));
            ci.unregister_toplevel(5);
            assert_eq!(ci.keyboard_focus, None);
            assert_eq!(ci.pointer_focus, None);
            assert_eq!(ci.grab, None);
            assert!(ci.pending_adoptions.is_empty());
            assert_eq!(ci.pending_removals, vec![5]);
        }
        // Phase `authoritative_tree_restores_input_placement_after_protocol_adoption`.
        {
            let mut tree = WorkspaceTree::bootstrap_default();
            let surface = 91_337;
            crate::shell::tab_binding::adopt_toplevel_at(
                &mut tree,
                surface,
                Some("anon-91337".into()),
                crate::shell::tab_binding::Placement::NewGroup,
            );
            let live = HashSet::from([surface]);

            let placements = placements_for_live_surfaces(&tree, &live);
            let (strip, group) = placements
                .get(&surface)
                .copied()
                .expect("adopted live surface has an input address");
            assert_eq!(
                tree.desktop.active_strip.get(&tree.desktop.active),
                Some(&strip)
            );
            assert_eq!(tree.desktop.active_group_for(strip), Some(group));
            assert!(
                placements_for_live_surfaces(&tree, &HashSet::new()).is_empty(),
                "dead protocol surfaces must not retain routable placement"
            );
        }
    }
}
