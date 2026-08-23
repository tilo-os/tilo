//! Deliver local keyboard / pointer input to foreign Wayland clients.
//!
//! The compositor draws its own chrome (top bar, tab strip, wheel disc,
//! palette, overlays) and, since the shell is not a Wayland client, that
//! chrome consumes winit events directly through
//! [`crate::compositor::frame::FrameBuilder::dispatch_event`]. A foreign
//! `xdg_toplevel` adopted into a tab is a *real* client though: it only
//! ever sees input that the compositor explicitly pushes into the
//! `wl_seat`.
//!
//! This module is that push. It owns three things:
//!
//! 1. **The surface registry** — `surface protocol id -> WlSurface`, filled
//!    from [`smithay::wayland::shell::xdg::XdgShellHandler::new_toplevel`]
//!    and drained on destroy. Without it there is no way back from the
//!    `u64` the workspace tree stores to the protocol object the seat
//!    needs.
//! 2. **Focus arbitration** — which side (shell chrome or a client
//!    surface) currently owns the keyboard, and which surface the pointer
//!    is over. Chrome always wins: an open overlay / palette, a
//!    compositor chord, or a pointer position outside every tile rect all
//!    keep input on the shell side.
//! 3. **Event translation** — winit `WindowEvent`s into the evdev
//!    keycodes, surface-local coordinates, button codes and axis frames
//!    smithay's `KeyboardHandle` / `PointerHandle` expect, with a fresh
//!    serial and a monotonic timestamp per event.
//!
//! Geometry note: a foreign toplevel is mounted as the sole tab of its
//! own window group, so the group's laid-out *tile* rect (everything
//! below the tab bar) is exactly the surface's on-screen rectangle. That
//! rect's origin is what smithay subtracts from the global pointer
//! location to produce surface-local coordinates — see
//! [`crate::shell::overlay_host::ShellOverlayHost::active_tile_rect`].

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ui::widget::{WheelJumpAccum, wheel_jump_detents};

use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, MotionEvent,
};
use smithay::input::touch::{
    DownEvent as TouchDownEvent, MotionEvent as TouchMotionEvent, UpEvent as TouchUpEvent,
};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point as SmithayPoint, Serial};
use vello::kurbo::{Point as KurboPoint, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey, PhysicalKey};
use winit::platform::scancode::PhysicalKeyExtScancode;

use crate::compositor::backend::{
    GestureEvent as BackendGestureEvent, TouchEvent as BackendTouchEvent,
};
use crate::compositor::frame::FrameBuilder;
use crate::compositor::state::State;
use crate::shell::notification::{
    Notification, NotificationIconKind, NotificationSeverity, PERSISTENT_DISMISS,
};
use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::{
    GroupId, StripId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree, next_id,
};

/// Linux `BTN_LEFT`. The Wayland pointer protocol carries raw evdev button
/// codes, not an enum, so the winit `MouseButton` has to be mapped back.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// Scroll distance, in surface-local units, that one notch of a discrete
/// wheel maps to. Matches the value every mainstream compositor sends so
/// clients that scale `wl_pointer.axis` by a fixed factor behave the same
/// here as they do under Mutter / KWin / niri.
const WHEEL_STEP: f64 = 15.0;

/// Which side of the compositor owns the pointer for the duration of an
/// implicit grab (first button down → last button up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerOwner {
    /// The shell's own chrome widgets.
    Chrome,
    /// A foreign client surface (protocol id).
    Client(u64),
}

/// A foreign toplevel waiting to be mirrored into the shell chrome's
/// workspace tree so it gets a tab, a laid-out tile rect, and therefore a
/// place for input to land.
#[derive(Clone, Debug)]
struct PendingAdoption {
    surface_id: u64,
    app_id: Option<String>,
}

/// Per-compositor state for routing local input into client surfaces.
///
/// Lives on [`State`] because the seat lives there; the run loop reaches
/// it through the free functions at the bottom of this module.
#[derive(Default)]
pub struct ClientInput {
    /// Live foreign surfaces keyed by their `wl_surface` protocol id — the
    /// same id [`crate::shell::state::TileContent::Foreign`] stores.
    surfaces: HashMap<u64, WlSurface>,
    /// Surface currently holding the seat's keyboard focus, or `None` when
    /// the shell chrome owns the keyboard.
    keyboard_focus: Option<u64>,
    /// Surface the pointer is currently inside (and therefore focused on
    /// at the seat), or `None` when the pointer is over chrome.
    pointer_focus: Option<u64>,
    /// Last known cursor position in global (output) coordinates, or
    /// `None` until the pointer has actually been somewhere.
    ///
    /// The distinction is not bookkeeping. Peer presence is published
    /// level-triggered from the frame loop, so whatever this says is
    /// announced to the room on every pass. A plain `(0.0, 0.0)` here
    /// is indistinguishable from a pointer resting in the top-left
    /// corner, and a session that has never had one moved through it
    /// would therefore put a cursor on every peer's screen for
    /// somebody who is not there — permanently, for a session that
    /// never moves a mouse at all.
    ///
    /// Written in exactly one place: the `CursorMoved` arm of
    /// [`route_input`], which every source of motion goes through — a
    /// real seat, the test-control socket, a peer's injected input.
    cursor: Option<(f64, f64)>,
    /// When input last reached this session — any input, not just a
    /// pointer: a key, a wheel, a button, a motion.
    ///
    /// A position and a *claim about that position* are different
    /// facts, and this is what separates them. Where the pointer was
    /// put stays true forever; "somebody is pointing there" stops being
    /// true when nobody is at the machine any more. Publishing is
    /// level-triggered with a keepalive, so without this the first is
    /// broadcast as the second until the session exits — one press,
    /// years later, still announcing itself.
    ///
    /// Any input at all counts, deliberately. Somebody typing has not
    /// touched their mouse, and their pointer is still under their
    /// hand; the question this answers is whether anyone is *there*,
    /// which is what a cursor on a peer's screen asserts.
    ///
    /// Stamped in [`route_input`], which its own comment names as the
    /// one function every local input goes through.
    last_input: Option<Instant>,
    /// Implicit pointer grab owner, latched on the first button press and
    /// released when the last button comes up. Without this a drag that
    /// starts on a client and leaves its tile would silently switch to the
    /// chrome mid-gesture (and vice versa).
    grab: Option<PointerOwner>,
    /// Number of physical pointer buttons currently held.
    buttons_down: u32,
    /// Foreign toplevels announced by the xdg-shell handler but not yet
    /// mirrored into the chrome's workspace tree.
    pending_adoptions: Vec<PendingAdoption>,
    /// Surfaces whose toplevel was destroyed but whose chrome tab has not
    /// been reaped yet.
    pending_removals: Vec<u64>,
    /// App-id updates that arrived after adoption (`set_app_id` races
    /// `get_toplevel`), waiting to be applied to the chrome tab.
    pending_renames: Vec<(u64, String)>,
    /// Where each mirrored surface ended up: `surface id -> (strip, group)`.
    placements: HashMap<u64, (StripId, GroupId)>,
    /// evdev codes whose *press* was forwarded to a client. A release is
    /// forwarded if and only if its press was, which keeps two things in
    /// step: a client never sees a release for a key it never saw pressed
    /// (a shell chord), and smithay's `forwarded_pressed_keys` never keeps
    /// a phantom entry that every later focus `enter` would replay (a key
    /// pressed with a client focused and released after an overlay took
    /// the keyboard).
    forwarded_keys: HashSet<u32>,
    /// Accumulator for the Alt+Shift wheel group-jump gesture, so a
    /// touchpad's stream of small pixel deltas fires one discrete jump
    /// per [`JUMP_PX_PER_DETENT`] of travel while a clicky wheel fires
    /// one per detent. See [`WheelJumpAccum`].
    jump_wheel: WheelJumpAccum,
}

/// Resolve where an Alt+Shift wheel jump of `steps` windows lands. A
/// "window" is a tab: the walk flattens the active strip's groups into
/// their tabs in strip order, so from the boot state's single
/// four-tab group (Notes, Files, Terminal, Browser) one step right of
/// Notes is Files, and past a group's last tab the walk continues into
/// the next group. Clamped at the strip's ends. `None` when there is
/// nothing to do — no workspace, no windows, or the walk is already
/// clamped at the edge it is pushing against.
///
/// Pure tree walk, split from the event plumbing so tests can pin the
/// navigation against a bare [`WorkspaceTree`].
pub(crate) fn step_active_window(
    tree: &WorkspaceTree,
    steps: i32,
) -> Option<(StripId, GroupId, u64)> {
    let ws = tree.active_workspace()?;
    // Focused strip, else the first — the same fallback `place_tab`
    // applies when a recorded pointer has gone stale.
    let strip = tree
        .active_strip
        .get(&ws.id)
        .copied()
        .and_then(|id| ws.strips.iter().find(|s| s.id == id))
        .or_else(|| ws.strips.first())?;
    // Every window along the strip, flattened in visual order.
    let windows: Vec<(GroupId, u64)> = strip
        .window_groups
        .iter()
        .flat_map(|g| g.tabs.iter().map(|t| (g.id, t.id)))
        .collect();
    if windows.is_empty() {
        return None;
    }
    // Current position: the focused group's active tab (with the same
    // first-group fallback focus resolution uses everywhere else).
    let cur_group = tree
        .active_group_for(strip.id)
        .and_then(|g| strip.window_groups.iter().find(|wg| wg.id == g))
        .or_else(|| strip.window_groups.first())?;
    let cur = windows
        .iter()
        .position(|&(g, t)| g == cur_group.id && t == cur_group.active_tab)
        .unwrap_or(0);
    let last = windows.len() as i64 - 1;
    let idx = (cur as i64 + steps as i64).clamp(0, last) as usize;
    if idx == cur {
        return None;
    }
    let (group, tab) = windows[idx];
    Some((strip.id, group, tab))
}

impl ClientInput {
    /// Record a freshly-created foreign toplevel. Called from
    /// `XdgShellHandler::new_toplevel`; the chrome-side tab is spliced in
    /// by [`sync_foreign_tabs`] on the next run-loop tick.
    pub fn register_toplevel(
        &mut self,
        surface_id: u64,
        surface: WlSurface,
        app_id: Option<String>,
    ) {
        self.surfaces.insert(surface_id, surface);
        self.pending_adoptions
            .push(PendingAdoption { surface_id, app_id });
    }

    /// Record a destroyed foreign toplevel. Drops the surface handle
    /// immediately (so no further input can be routed to it) and queues
    /// the chrome-side tab removal.
    /// Drop any queued adoption / removal / rename bookkeeping.
    ///
    /// The rendered tab tree is mirrored by a diff of the protocol tree
    /// (`foreign::reconcile_foreign_tabs`), which is idempotent and does not
    /// consume these queues. They are still fed by the toplevel lifecycle
    /// hooks, so the run loop drains them each tick to keep them bounded.
    pub fn clear_pending(&mut self) {
        self.pending_adoptions.clear();
        self.pending_removals.clear();
        self.pending_renames.clear();
    }

    pub fn unregister_toplevel(&mut self, surface_id: u64) {
        self.surfaces.remove(&surface_id);
        self.pending_adoptions
            .retain(|p| p.surface_id != surface_id);
        self.pending_removals.push(surface_id);
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

    /// The surface the seat's keyboard is currently focused on, if any.
    pub fn keyboard_focus(&self) -> Option<u64> {
        self.keyboard_focus
    }

    /// The surface the seat's pointer is currently focused on, if any.
    pub fn pointer_focus(&self) -> Option<u64> {
        self.pointer_focus
    }

    /// Whether `surface_id` is a live foreign toplevel this compositor can
    /// route input to.
    pub fn is_known(&self, surface_id: u64) -> bool {
        self.surfaces.contains_key(&surface_id)
    }

    /// The protocol object for a surface id, if it is still live.
    pub fn surface(&self, surface_id: u64) -> Option<&WlSurface> {
        self.surfaces.get(&surface_id)
    }

    /// Every mirrored surface and the group it is in, so the run loop can
    /// resolve each one's tile rect and derive its preferred scale.
    pub fn placements(&self) -> impl Iterator<Item = (u64, GroupId)> + '_ {
        self.placements.iter().map(|(id, (_, group))| (*id, *group))
    }

    /// Number of live foreign toplevels.
    pub fn len(&self) -> usize {
        self.surfaces.len()
    }

    /// True when no foreign toplevel is registered.
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Last cursor position the router observed, in global coordinates.
    ///
    /// Falls back to the origin, because the hit-testing and drag
    /// callers want a point rather than a question: there is nowhere
    /// else for them to look. Use [`Self::pointer_position`] wherever
    /// "has never been anywhere" is a different answer from "is at the
    /// origin" — publishing this session's pointer to its peers is.
    pub fn cursor(&self) -> (f64, f64) {
        self.cursor.unwrap_or_default()
    }

    /// Where the pointer is, or `None` if it has never been anywhere.
    ///
    /// The honest form of [`Self::cursor`], for the one caller that
    /// must tell the two apart. A pointer is for pointing at something,
    /// so a session that has never moved one has nothing to say about
    /// where it is — and saying the origin instead is a cursor painted
    /// where nobody is.
    pub fn pointer_position(&self) -> Option<(f64, f64)> {
        self.cursor
    }

    /// Where this session is pointing *right now*, or `None` if that is
    /// not a thing it can currently say.
    ///
    /// [`Self::pointer_position`] answers where the pointer was put.
    /// This answers the question a cursor on a peer's screen actually
    /// asks — *is somebody pointing there* — and the two come apart the
    /// moment a session goes quiet. A pointer is an instrument for
    /// pointing at something you are looking at, so a session nobody
    /// has touched for `idle` has nothing to say about where it is,
    /// however real the position it holds.
    ///
    /// This is the difference between a position and a claim, and it is
    /// the one the keepalive erases: a session restates a stationary
    /// pointer every second so a peer who joins late is not shown an
    /// empty desktop, and a position from one press at start-up was
    /// therefore restated for the whole life of the session.
    ///
    /// Any input revives it on the next frame, so the failure mode is a
    /// cursor that is briefly missing for somebody genuinely idle —
    /// never a stale one that cannot be got rid of.
    pub fn live_pointer(&self, now: Instant, idle: Duration) -> Option<(f64, f64)> {
        let at = self.cursor?;
        let last = self.last_input?;
        (now.saturating_duration_since(last) < idle).then_some(at)
    }

    /// When input last reached this session, if it ever has.
    pub fn last_input(&self) -> Option<Instant> {
        self.last_input
    }

    /// Place the pointer and record the input that placed it, as the
    /// `CursorMoved` arm of [`route_input`] does.
    ///
    /// Test-only, and it writes both fields for the same reason
    /// `route_input` does: in production a position exists *because*
    /// somebody did something, so a fixture that set one without the
    /// other would be a session state the compositor cannot reach. That
    /// `route_input` really writes both is proved separately, against
    /// the real event, in this module's own tests.
    #[cfg(test)]
    pub(crate) fn note_input_for_test(&mut self, at: Option<(f64, f64)>, when: Instant) {
        self.cursor = at;
        self.last_input = Some(when);
    }
}

// ---------------------------------------------------------------------
// Pure helpers — no seat, no widgets. Unit-testable in isolation.
// ---------------------------------------------------------------------

/// Resolve the foreign surface hosted by the *focused* tab: the active tab
/// of the active group of the active strip of the active workspace.
/// Returns `None` when that tab holds shell-owned content (a native app,
/// a stub) or when the tree is empty.
///
/// This is the keyboard-focus rule: keyboard input follows the active tab,
/// so switching tabs / groups / workspaces moves the seat focus with it.
pub fn active_foreign_surface(tree: &WorkspaceTree) -> Option<u64> {
    let ws = tree.active_workspace()?;
    let strip = match tree.active_strip.get(&ws.id) {
        Some(sid) => ws
            .strips
            .iter()
            .find(|s| s.id == *sid)
            .or_else(|| ws.strips.first())?,
        None => ws.strips.first()?,
    };
    let group = match tree.active_group_for(strip.id) {
        Some(gid) => strip
            .window_groups
            .iter()
            .find(|g| g.id == gid)
            .or_else(|| strip.window_groups.first())?,
        None => strip.window_groups.first()?,
    };
    let tab = group
        .tabs
        .iter()
        .find(|t| t.id == group.active_tab)
        .or_else(|| group.tabs.first())?;
    match &tab.tile {
        TileContent::Foreign { surface_id, .. } => Some(*surface_id),
        _ => None,
    }
}

/// A foreign tile under the cursor: its surface id, the strip + group that
/// own it, and its screen-space rect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileHit {
    pub surface_id: u64,
    pub strip: StripId,
    pub group: GroupId,
    pub rect: Rect,
}

/// Hit-test `point` (global coordinates) against every foreign tile in the
/// active workspace, using the live laid-out rects from the widget tree.
///
/// Chrome keeps priority implicitly: the tile rect is the tab's *content*
/// area, so the top bar, the tab strip, the sidebar wheel and the inter-group
/// gutters are all outside it and never resolve to a client. Overlays that
/// paint *over* a tile (palette, overview, quick settings, …) are excluded by
/// the caller via [`chrome_captures_input`].
pub fn foreign_tile_at(
    tree: &WorkspaceTree,
    host: &ShellOverlayHost,
    point: (f64, f64),
) -> Option<TileHit> {
    let p = KurboPoint::new(point.0, point.1);
    foreign_tiles(tree, host)
        .into_iter()
        .find(|hit| hit.rect.contains(p))
}

/// Every foreign tile currently on screen, in the compositor's own
/// front-to-back order — the order [`foreign_tile_at`] resolves ties in,
/// so the first entry whose rect contains a point is what local input
/// would be routed to.
///
/// Exposed because remote input has to answer the same question
/// ("what is actually on top at this point?") from a different thread of
/// control, and answering it from a second traversal is how a
/// clickjacking check ends up disagreeing with where the click really
/// lands. See [`crate::network::input_firewall`].
pub fn foreign_tiles(tree: &WorkspaceTree, host: &ShellOverlayHost) -> Vec<TileHit> {
    let Some(ws) = tree.active_workspace() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for strip in &ws.strips {
        for group in &strip.window_groups {
            let Some(tab) = group.tabs.iter().find(|t| t.id == group.active_tab) else {
                continue;
            };
            let TileContent::Foreign { surface_id, .. } = &tab.tile else {
                continue;
            };
            let Some(rect) = host.active_tile_rect(group.id) else {
                continue;
            };
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            out.push(TileHit {
                surface_id: *surface_id,
                strip: strip.id,
                group: group.id,
                rect,
            });
        }
    }
    out
}

/// Surface-local coordinates for a global `point` inside `rect`.
///
/// Smithay does this subtraction itself when handed the focus origin, but
/// the translation is the load-bearing part of pointer routing so it is
/// exposed (and asserted on) separately.
pub fn surface_local(point: (f64, f64), rect: Rect) -> (f64, f64) {
    (point.0 - rect.x0, point.1 - rect.y0)
}

/// Bare modifier keys. These are never treated as compositor chords: a
/// client's xkb state has to see them or its own `Shift`/`Ctrl` handling
/// desynchronises from the seat's.
fn is_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::CapsLock
                | NamedKey::AltGraph
                | NamedKey::NumLock
        )
    )
}

/// Fold a bare modifier key press / release into `modifiers`.
///
/// winit reports modifier state through a *separate* `ModifiersChanged`
/// event, so a backend that only synthesises `KeyboardInput` (the DRM
/// libinput bridge before its own mask update, the headless test-control
/// socket) would leave the running mask stale — and a stale mask means
/// [`is_shell_chord`] misses `Ctrl+K` and hands it to the client instead.
/// Deriving the mask from the key stream as well closes that hole.
///
/// Returns `true` when the mask changed. A backend that *does* emit
/// `ModifiersChanged` already agrees with the key stream, so this is a
/// no-op there and the richer left/right state winit tracks is preserved.
pub fn apply_modifier_key(modifiers: &mut Modifiers, key: &KeyEvent) -> bool {
    use winit::keyboard::ModifiersState;
    let bit = match &key.logical_key {
        Key::Named(NamedKey::Shift) => ModifiersState::SHIFT,
        Key::Named(NamedKey::Control) => ModifiersState::CONTROL,
        Key::Named(NamedKey::Alt) | Key::Named(NamedKey::AltGraph) => ModifiersState::ALT,
        Key::Named(NamedKey::Super) | Key::Named(NamedKey::Meta) => ModifiersState::SUPER,
        _ => return false,
    };
    let mut mask = modifiers.state();
    if key.state == ElementState::Pressed {
        mask |= bit;
    } else {
        mask -= bit;
    }
    if mask == modifiers.state() {
        return false;
    }
    *modifiers = Modifiers::from(mask);
    true
}

/// A key reduced to the shape chord classification cares about.
///
/// Exists so the chord table can be consulted from two directions. Local
/// input arrives as a winit [`Key`]; input injected by a remote peer
/// arrives as a bare evdev keycode off the wire. Both narrow to this enum
/// and ask [`shell_owns_chord`], so the shell's list of grabbed chords and
/// the list a guest is refused cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordKey {
    Space,
    Tab,
    ArrowUp,
    ArrowDown,
    Escape,
    Delete,
    /// `F1`..`F12`, numbered from 1.
    Function(u8),
    /// A single printable character, already lowercased.
    Char(char),
    /// A bare modifier. Never a chord on its own — a client's xkb state
    /// has to see modifier transitions or it desynchronises from the seat.
    Modifier,
    /// Anything the chord table has no opinion about.
    Other,
}

/// The modifier mask a chord is evaluated against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChordMods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

impl ChordMods {
    /// The mask carried by an xkbcommon-style bitfield — the encoding the
    /// peer-sync wire protocol uses for
    /// [`crate::network::streaming::InputEvent::Key`].
    pub fn from_xkb_bits(bits: u32) -> Self {
        Self {
            shift: bits & 0x1 != 0,
            ctrl: bits & 0x4 != 0,
            alt: bits & 0x8 != 0,
            logo: bits & 0x40 != 0,
        }
    }

    /// Union of two masks. Used where a mask is assembled from more than
    /// one source and any bit set in either has to count.
    pub fn union(self, other: Self) -> Self {
        Self {
            shift: self.shift || other.shift,
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            logo: self.logo || other.logo,
        }
    }
}

impl From<&Modifiers> for ChordMods {
    fn from(m: &Modifiers) -> Self {
        let s = m.state();
        Self {
            shift: s.shift_key(),
            ctrl: s.control_key(),
            alt: s.alt_key(),
            logo: s.super_key(),
        }
    }
}

/// The shell's chord table, expressed once.
///
/// Covers the shell's whole chord set: every `Super` chord (Super is the
/// compositor modifier — `Super+L` lock, `Super+K` / `Super+Space` search,
/// `Super+Tab` overview, `Super+Arrow` workspace switch, `Super+Esc` the
/// remote-input kill switch), `Ctrl+K` / `Alt+Space` search, `Ctrl+Tab`
/// overview, `Ctrl+Arrow` workspace switch, and `Escape` *only while an
/// overlay is open* (an unconditional Escape grab would break every modal
/// editor running as a client).
pub fn shell_owns_chord(key: ChordKey, mods: ChordMods, overlay_open: bool) -> bool {
    if key == ChordKey::Modifier {
        return false;
    }
    if mods.logo {
        return true;
    }
    match key {
        ChordKey::Space => mods.alt,
        ChordKey::Tab => mods.ctrl,
        ChordKey::ArrowUp | ChordKey::ArrowDown => mods.ctrl,
        ChordKey::Escape => overlay_open,
        ChordKey::Char(c) => mods.ctrl && c == 'k',
        ChordKey::Delete | ChordKey::Function(_) | ChordKey::Other | ChordKey::Modifier => false,
    }
}

/// Narrow a winit logical key to its [`ChordKey`].
pub fn chord_key_for_logical(key: &Key) -> ChordKey {
    if is_modifier_key(key) {
        return ChordKey::Modifier;
    }
    match key {
        Key::Named(NamedKey::Space) => ChordKey::Space,
        Key::Named(NamedKey::Tab) => ChordKey::Tab,
        Key::Named(NamedKey::ArrowUp) => ChordKey::ArrowUp,
        Key::Named(NamedKey::ArrowDown) => ChordKey::ArrowDown,
        Key::Named(NamedKey::Escape) => ChordKey::Escape,
        Key::Named(NamedKey::Delete) => ChordKey::Delete,
        Key::Named(NamedKey::F1) => ChordKey::Function(1),
        Key::Named(NamedKey::F2) => ChordKey::Function(2),
        Key::Named(NamedKey::F3) => ChordKey::Function(3),
        Key::Named(NamedKey::F4) => ChordKey::Function(4),
        Key::Named(NamedKey::F5) => ChordKey::Function(5),
        Key::Named(NamedKey::F6) => ChordKey::Function(6),
        Key::Named(NamedKey::F7) => ChordKey::Function(7),
        Key::Named(NamedKey::F8) => ChordKey::Function(8),
        Key::Named(NamedKey::F9) => ChordKey::Function(9),
        Key::Named(NamedKey::F10) => ChordKey::Function(10),
        Key::Named(NamedKey::F11) => ChordKey::Function(11),
        Key::Named(NamedKey::F12) => ChordKey::Function(12),
        Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                // A layout that delivers the space bar as text rather than
                // `NamedKey::Space` must still reach the `Alt+Space` arm.
                (Some(' '), None) => ChordKey::Space,
                (Some(c), None) => ChordKey::Char(c.to_ascii_lowercase()),
                _ => ChordKey::Other,
            }
        }
        _ => ChordKey::Other,
    }
}

/// Narrow an evdev keycode to its [`ChordKey`].
///
/// The inverse of [`evdev_from_logical`] over the range that table covers;
/// `chord_key_agrees_across_both_directions` in this module's tests pins
/// the two together so a key added to one is not missed by the other.
pub fn chord_key_for_evdev(code: u32) -> ChordKey {
    match code {
        1 => ChordKey::Escape,
        15 => ChordKey::Tab,
        29 | 97 => ChordKey::Modifier,  // left / right Ctrl
        42 | 54 => ChordKey::Modifier,  // left / right Shift
        56 | 100 => ChordKey::Modifier, // Alt / AltGr
        57 => ChordKey::Space,
        58 => ChordKey::Modifier, // CapsLock
        59..=68 => ChordKey::Function((code - 58) as u8),
        69 => ChordKey::Modifier, // NumLock
        87 => ChordKey::Function(11),
        88 => ChordKey::Function(12),
        103 => ChordKey::ArrowUp,
        108 => ChordKey::ArrowDown,
        111 => ChordKey::Delete,
        125 | 126 => ChordKey::Modifier, // left / right Super
        _ => match char_from_evdev(code) {
            Some(c) => ChordKey::Char(c),
            None => ChordKey::Other,
        },
    }
}

/// Is this key event a compositor-level binding that must be handled by the
/// shell *before* a focused client ever sees it?
///
/// Thin winit adapter over [`shell_owns_chord`] — see there for the table.
pub fn is_shell_chord(event: &WindowEvent, modifiers: &Modifiers, overlay_open: bool) -> bool {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return false;
    };
    shell_owns_chord(
        chord_key_for_logical(&key.logical_key),
        ChordMods::from(modifiers),
        overlay_open,
    )
}

/// The evdev keycode for a winit key event.
///
/// Prefers the physical scancode (winit reports the raw Linux keycode on
/// Wayland / X11, which is exactly what `wl_keyboard.key` carries). Falls
/// back to a logical-key table for events synthesised without a physical
/// key — the DRM backend's libinput bridge and the test-control socket both
/// build `KeyEvent`s that way.
pub fn evdev_keycode(key: &KeyEvent) -> Option<u32> {
    // Only an identified physical key carries a trustworthy scancode:
    // winit maps `PhysicalKey::Unidentified` back to `KEY_UNKNOWN` (240),
    // which would otherwise be handed to the client as a real keycode and
    // make every synthesised keystroke arrive as the same dead key.
    if let PhysicalKey::Code(code) = key.physical_key {
        if let Some(scancode) = code.to_scancode() {
            if scancode != 0 {
                return Some(scancode);
            }
        }
    }
    evdev_from_logical(&key.logical_key)
}

/// Logical-key → evdev keycode fallback. Values are from
/// `linux/input-event-codes.h`.
pub fn evdev_from_logical(key: &Key) -> Option<u32> {
    let named = match key {
        Key::Named(n) => match n {
            NamedKey::Escape => Some(1),
            NamedKey::Backspace => Some(14),
            NamedKey::Tab => Some(15),
            NamedKey::Enter => Some(28),
            NamedKey::Control => Some(29),
            NamedKey::Shift => Some(42),
            NamedKey::Alt => Some(56),
            NamedKey::Space => Some(57),
            NamedKey::CapsLock => Some(58),
            NamedKey::F1 => Some(59),
            NamedKey::F2 => Some(60),
            NamedKey::F3 => Some(61),
            NamedKey::F4 => Some(62),
            NamedKey::F5 => Some(63),
            NamedKey::F6 => Some(64),
            NamedKey::F7 => Some(65),
            NamedKey::F8 => Some(66),
            NamedKey::F9 => Some(67),
            NamedKey::F10 => Some(68),
            NamedKey::F11 => Some(87),
            NamedKey::F12 => Some(88),
            NamedKey::Home => Some(102),
            NamedKey::ArrowUp => Some(103),
            NamedKey::PageUp => Some(104),
            NamedKey::ArrowLeft => Some(105),
            NamedKey::ArrowRight => Some(106),
            NamedKey::End => Some(107),
            NamedKey::ArrowDown => Some(108),
            NamedKey::PageDown => Some(109),
            NamedKey::Insert => Some(110),
            NamedKey::Delete => Some(111),
            NamedKey::Super | NamedKey::Meta => Some(125),
            _ => None,
        },
        Key::Character(s) => {
            let mut chars = s.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            evdev_from_char(c)
        }
        _ => None,
    };
    named
}

/// Single-character → evdev keycode (US layout positions).
fn evdev_from_char(c: char) -> Option<u32> {
    let lower = c.to_ascii_lowercase();
    let code = match lower {
        'a' => 30,
        'b' => 48,
        'c' => 46,
        'd' => 32,
        'e' => 18,
        'f' => 33,
        'g' => 34,
        'h' => 35,
        'i' => 23,
        'j' => 36,
        'k' => 37,
        'l' => 38,
        'm' => 50,
        'n' => 49,
        'o' => 24,
        'p' => 25,
        'q' => 16,
        'r' => 19,
        's' => 31,
        't' => 20,
        'u' => 22,
        'v' => 47,
        'w' => 17,
        'x' => 45,
        'y' => 21,
        'z' => 44,
        '1' => 2,
        '2' => 3,
        '3' => 4,
        '4' => 5,
        '5' => 6,
        '6' => 7,
        '7' => 8,
        '8' => 9,
        '9' => 10,
        '0' => 11,
        '-' => 12,
        '=' => 13,
        '[' => 26,
        ']' => 27,
        ';' => 39,
        '\'' => 40,
        '`' => 41,
        '\\' => 43,
        ',' => 51,
        '.' => 52,
        '/' => 53,
        ' ' => 57,
        _ => return None,
    };
    Some(code)
}

/// evdev keycode → the single character [`evdev_from_char`] would have
/// mapped to it, in lowercase. `None` for codes that carry no character.
fn char_from_evdev(code: u32) -> Option<char> {
    Some(match code {
        30 => 'a',
        48 => 'b',
        46 => 'c',
        32 => 'd',
        18 => 'e',
        33 => 'f',
        34 => 'g',
        35 => 'h',
        23 => 'i',
        36 => 'j',
        37 => 'k',
        38 => 'l',
        50 => 'm',
        49 => 'n',
        24 => 'o',
        25 => 'p',
        16 => 'q',
        19 => 'r',
        31 => 's',
        20 => 't',
        22 => 'u',
        47 => 'v',
        17 => 'w',
        45 => 'x',
        21 => 'y',
        44 => 'z',
        2 => '1',
        3 => '2',
        4 => '3',
        5 => '4',
        6 => '5',
        7 => '6',
        8 => '7',
        9 => '8',
        10 => '9',
        11 => '0',
        12 => '-',
        13 => '=',
        26 => '[',
        27 => ']',
        39 => ';',
        40 => '\'',
        41 => '`',
        43 => '\\',
        51 => ',',
        52 => '.',
        53 => '/',
        _ => return None,
    })
}

/// evdev button code for a winit mouse button.
pub fn evdev_button(button: MouseButton) -> Option<u32> {
    Some(match button {
        MouseButton::Left => BTN_LEFT,
        MouseButton::Right => BTN_RIGHT,
        MouseButton::Middle => BTN_MIDDLE,
        MouseButton::Back => BTN_SIDE,
        MouseButton::Forward => BTN_EXTRA,
        MouseButton::Other(_) => return None,
    })
}

/// Translate a winit scroll delta into the `(value, v120)` pair per axis
/// that `wl_pointer.axis` / `axis_value120` carry.
///
/// Winit reports "up / left" as positive; Wayland reports "down / right" as
/// positive, so both components are negated. Discrete wheels also carry a
/// `v120` step count (120 units = one notch) that clients use for
/// stepped scrolling.
pub fn axis_values(delta: MouseScrollDelta) -> ((f64, f64), Option<(i32, i32)>) {
    match delta {
        MouseScrollDelta::LineDelta(h, v) => (
            (-(h as f64) * WHEEL_STEP, -(v as f64) * WHEEL_STEP),
            Some((
                (-(h as f64) * 120.0).round() as i32,
                (-(v as f64) * 120.0).round() as i32,
            )),
        ),
        MouseScrollDelta::PixelDelta(p) => ((-p.x, -p.y), None),
    }
}

/// Whether the shell chrome is currently claiming all input: an overlay or
/// the command palette is open, the peer-sync card is up, the
/// workspace-name editor has a buffer in flight, or a permission prompt is
/// pending. In any of those states a client must neither receive keys nor
/// hold pointer focus.
pub fn chrome_captures_input(fb: &FrameBuilder) -> bool {
    if fb.host().palette_visible() {
        return true;
    }
    // The portal's file chooser, for the same reason as the peer-sync
    // card below. It is modal — drawn over a scrim, above every other
    // overlay — but "drawn above" and "receives the click" are separate
    // things, and this predicate is the one that decides the second.
    // Without it, a chooser raised over a focused window was painted
    // full-screen while every click in it hit-tested through to the app
    // underneath: an app could ask for a file, the dialog would appear,
    // and neither the row nor Open nor Escape could be reached with a
    // mouse or a keyboard. The app asks, the OS chooses — but only if
    // the OS is the one being clicked.
    if fb.host().file_picker().is_some() {
        return true;
    }
    let chrome = fb.chrome();
    chrome.overlays.overview
        || chrome.overlays.quick_settings
        || chrome.overlays.ws_settings
        || chrome.overlays.collaborators
        || chrome.overlays.room_activity
        // The peer-sync card, on the same two fields the overlay host
        // paints it from. Its handler runs before every other overlay
        // and its doc comment says every key belongs to it while it is
        // up — but that only holds for events that reach the overlay
        // host at all, and this predicate is what decides whether they
        // do. Without it a card raised over a focused client sent its
        // Escape to the client instead, so the card could not be
        // dismissed: it sat over the middle of the screen for the rest
        // of the session with a live invite token legible on it, and
        // every later window opened behind it.
        || chrome.peer_sync_invite_token.is_some()
        || chrome.peer_sync_connect_input.is_some()
        || chrome.peer_sync_invite_name.is_some()
        || chrome.editing_workspace_name.is_some()
        || !chrome.pending_grants.is_empty()
}

// ---------------------------------------------------------------------
// Seat delivery — the actual `wl_keyboard` / `wl_pointer` traffic.
// ---------------------------------------------------------------------

/// Mint a fresh input serial. Wayland clients ignore events whose serial
/// goes backwards, so every delivered event gets its own.
fn mint_serial(state: &mut State) -> Serial {
    state.serial_counter = state.serial_counter.wrapping_add(1);
    Serial::from(state.serial_counter)
}

/// Milliseconds since compositor start — the timestamp base every input
/// event on this seat shares.
fn event_time(state: &State) -> u32 {
    state.start_time.elapsed().as_millis() as u32
}

impl State {
    /// Point the seat's keyboard at `surface_id` (or at nothing when
    /// `None`), sending the `leave` / `enter` pair and the keymap the
    /// client needs before it can interpret any keycode.
    ///
    /// Idempotent: a repeat call with the current focus is a no-op, so the
    /// run loop can call it unconditionally every tick.
    pub fn set_client_keyboard_focus(&mut self, surface_id: Option<u64>) {
        if self.client_input.keyboard_focus == surface_id {
            return;
        }
        let target = match surface_id {
            Some(id) => match self.client_input.surfaces.get(&id) {
                Some(s) => Some((id, s.clone())),
                // Unknown id: treat as "no client focus" rather than
                // leaving the seat pointed at a destroyed surface.
                None => None,
            },
            None => None,
        };
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let serial = mint_serial(self);
        self.client_input.keyboard_focus = target.as_ref().map(|(id, _)| *id);
        // Clipboard and primary selection follow the keyboard, in that order:
        // smithay refuses `set_selection` from a client that does not hold
        // keyboard focus, and only offers the current selection to the focused
        // client. Moving one without the other would leave a focused window
        // able to type but unable to paste.
        let selection_client = target
            .as_ref()
            .and_then(|(_, surface)| surface.client());
        self.set_selection_focus(selection_client);
        keyboard.set_focus(self, target.map(|(_, s)| s), serial);
    }

    /// Push one key press / release through the seat, forwarding it to the
    /// focused client only when `forward` is set.
    ///
    /// **Every** key the compositor sees goes through here, including the
    /// ones the shell keeps for itself. Smithay's xkb state machine derives
    /// modifier state from the keycode stream it is fed, so skipping the
    /// shell's own keys would desynchronise it: a `Ctrl` press delivered to
    /// a client followed by a `Ctrl` release swallowed by an open overlay
    /// leaves the seat believing `Ctrl` is still held, and the next focus
    /// `enter` reports a phantom modifier. `FilterResult::Intercept`
    /// advances the state without emitting anything on the wire, which is
    /// exactly the distinction needed.
    ///
    /// `keycode` is an evdev code; smithay wants the xkb form (evdev + 8)
    /// and subtracts the offset again on the wire, so the client receives
    /// exactly the code the kernel produced.
    ///
    /// Returns `true` only when the event actually reached a client.
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
        let intercepted: Option<()> = keyboard.input(
            self,
            (keycode + 8).into(),
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
        forward && has_focus && intercepted.is_none()
    }

    /// Move the seat pointer to `global` and focus it on `target`.
    ///
    /// `target` is `(surface id, tile origin)`. Smithay subtracts the
    /// origin from the global location to produce the surface-local
    /// coordinates the client sees, and derives the `enter` / `leave` /
    /// `motion` sequence from the focus transition — passing `None` is
    /// therefore how the pointer *leaves* a client.
    pub fn forward_client_pointer_motion(
        &mut self,
        global: (f64, f64),
        target: Option<(u64, (f64, f64))>,
    ) {
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
        self.client_input.pointer_focus = focus.as_ref().and(target.map(|(id, _)| id));
        let serial = mint_serial(self);
        let time = event_time(self);
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location: SmithayPoint::from(global),
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// Forward a pointer button press / release to the surface the pointer
    /// is focused on. No-op when the pointer is over chrome.
    pub fn forward_client_pointer_button(&mut self, button: u32, pressed: bool) -> bool {
        // A chrome drag's grab needs the release even with no client under
        // the pointer — that is how it learns the drag was cancelled.
        if self.client_input.pointer_focus.is_none() && self.selection.chrome_drag.is_none() {
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
        true
    }

    /// Forward a scroll event to the surface the pointer is focused on.
    pub fn forward_client_pointer_axis(&mut self, delta: MouseScrollDelta) -> bool {
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
        if h == 0.0 {
            frame = frame.stop(Axis::Horizontal);
        }
        if v == 0.0 {
            frame = frame.stop(Axis::Vertical);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
        true
    }

    /// Forward one touchpad gesture to the focused client over
    /// `zwp_pointer_gestures_v1`.
    ///
    /// The compositor has advertised that global since the protocol
    /// plumbing landed and never emitted a single event into it, so a
    /// client that bound it and waited for a pinch waited forever. Only
    /// gestures the shell did *not* claim reach here — the finger-count
    /// split is made in [`crate::compositor::backend::drm::gestures`], one
    /// place, so a gesture can never be handled twice.
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

    /// Forward one touch point to the focused client over `wl_touch`.
    ///
    /// `target` is `(surface id, tile origin)` for a `Down` that landed
    /// inside a client tile — smithay subtracts the origin to produce
    /// surface-local coordinates, exactly as for the pointer. Touch focus
    /// is set on `down` only (the protocol says so), so `Motion` carries
    /// the target purely for drag-target resolution.
    pub fn forward_client_touch(
        &mut self,
        touch: BackendTouchEvent,
        target: Option<(u64, (f64, f64))>,
    ) {
        let Some(handle) = self.seat.get_touch() else {
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

/// Which client (if any) a touch point should be delivered to.
///
/// Only a `Down` establishes touch focus, so only a `Down` resolves a
/// target; the rest of the sequence belongs to whatever the down landed
/// on and smithay tracks that itself.
pub fn touch_target(
    state: &State,
    fb: &FrameBuilder,
    touch: &BackendTouchEvent,
) -> Option<(u64, (f64, f64))> {
    let BackendTouchEvent::Down { x, y, .. } = touch else {
        return None;
    };
    let chrome = fb.chrome();
    let tree = chrome.tree();
    let hit = foreign_tile_at(&tree, fb.host(), (*x, *y))?;
    if !state.client_input.surfaces.contains_key(&hit.surface_id) {
        return None;
    }
    Some((hit.surface_id, (hit.rect.x0, hit.rect.y0)))
}

// ---------------------------------------------------------------------
// Run-loop entry points.
// ---------------------------------------------------------------------

/// Mirror foreign toplevels adopted at the protocol layer into the shell
/// chrome's workspace tree, and reap the tabs of destroyed ones.
///
/// Each foreign toplevel becomes its own window group in the active
/// strip — that is what gives it a tab pill, a laid-out tile rect (which
/// the pointer router needs), and a place in the active-group rotation
/// (which the keyboard router needs). The chrome's per-frame
/// `resync_strips_from_tree` diff splices the new group into the live
/// widget row on the next relayout.
///
/// Returns `true` when the tree changed.
pub fn sync_foreign_tabs(state: &mut State, fb: &mut FrameBuilder) -> bool {
    let adoptions = std::mem::take(&mut state.client_input.pending_adoptions);
    let removals = std::mem::take(&mut state.client_input.pending_removals);
    let renames = std::mem::take(&mut state.client_input.pending_renames);
    if adoptions.is_empty() && removals.is_empty() && renames.is_empty() {
        return false;
    }
    let mut changed = false;
    let mut placements: Vec<(u64, StripId, GroupId)> = Vec::new();

    fb.with_chrome(|chrome| {
        {
            let mut tree = chrome.tree_mut();
            for adoption in &adoptions {
                if let Some((strip, group)) =
                    insert_foreign_group(&mut tree, adoption.surface_id, adoption.app_id.as_deref())
                {
                    placements.push((adoption.surface_id, strip, group));
                    changed = true;
                }
            }
            for surface_id in &removals {
                if remove_foreign_tab(&mut tree, *surface_id) {
                    changed = true;
                }
            }
            for (surface_id, app_id) in &renames {
                if rename_foreign_tab(&mut tree, *surface_id, app_id) {
                    changed = true;
                }
            }
        }
        if changed {
            chrome.bump();
        }
    });

    for (surface_id, strip, group) in placements {
        state
            .client_input
            .placements
            .insert(surface_id, (strip, group));
    }
    for surface_id in &removals {
        state.client_input.placements.remove(surface_id);
    }
    changed
}

/// Splice a foreign toplevel into the active workspace as its own window
/// group, and make it the active group of its strip so keyboard focus
/// follows the newly-mapped window.
///
/// Returns the `(strip, group)` it landed in, or `None` when the active
/// workspace has no strip to land in.
fn insert_foreign_group(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<&str>,
) -> Option<(StripId, GroupId)> {
    let active_ws = tree.active;
    let preferred_strip = tree.active_strip.get(&active_ws).copied();
    let ws = tree.workspaces.iter_mut().find(|w| w.id == active_ws)?;
    // Index first, then borrow once — `or_else` over two `&mut` borrows of
    // the same vec doesn't type-check.
    let idx = preferred_strip
        .and_then(|sid| ws.strips.iter().position(|s| s.id == sid))
        .unwrap_or(0);
    let strip = ws.strips.get_mut(idx)?;
    let resolved = app_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("anon-{surface_id}"));
    let tab = Tab {
        id: next_id(),
        title: foreign_tab_title(&resolved),
        tile: TileContent::Foreign {
            app_id: resolved,
            surface_id,
        },
    };
    let tab_id = tab.id;
    let group = WindowGroup {
        id: next_id(),
        tabs: vec![tab],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
    };
    let group_id = group.id;
    let strip_id = strip.id;
    strip.window_groups.push(group);
    tree.set_active_group(strip_id, group_id);
    Some((strip_id, group_id))
}

/// Drop the tab bound to `surface_id`, and the group that hosted it when
/// that leaves the group empty. Also advances the strip's active-group
/// pointer off a group that just disappeared.
fn remove_foreign_tab(tree: &mut WorkspaceTree, surface_id: u64) -> bool {
    let mut vacated: Option<(StripId, GroupId)> = None;
    for ws in tree.workspaces.iter_mut() {
        for strip in ws.strips.iter_mut() {
            let mut hit_group: Option<GroupId> = None;
            for group in strip.window_groups.iter_mut() {
                let Some(pos) = group.tabs.iter().position(|t| {
                    matches!(&t.tile, TileContent::Foreign { surface_id: s, .. } if *s == surface_id)
                }) else {
                    continue;
                };
                let removed = group.tabs.remove(pos);
                if group.active_tab == removed.id {
                    group.active_tab = group.tabs.last().map(|t| t.id).unwrap_or(0);
                }
                hit_group = Some(group.id);
                break;
            }
            let Some(gid) = hit_group else { continue };
            let strip_id = strip.id;
            strip
                .window_groups
                .retain(|g| g.id != gid || !g.tabs.is_empty());
            let group_gone = !strip.window_groups.iter().any(|g| g.id == gid);
            if group_gone {
                vacated = Some((strip_id, gid));
            }
            if let Some((sid, gone)) = vacated {
                let fallback = strip.window_groups.last().map(|g| g.id);
                if tree.active_group.get(&sid) == Some(&gone) {
                    match fallback {
                        Some(next) => {
                            tree.active_group.insert(sid, next);
                        }
                        None => {
                            tree.active_group.remove(&sid);
                        }
                    }
                }
            }
            return true;
        }
    }
    false
}

/// Rewrite the app-id + title of a mirrored foreign tab after a late
/// `xdg_toplevel.set_app_id`.
fn rename_foreign_tab(tree: &mut WorkspaceTree, surface_id: u64, new_app_id: &str) -> bool {
    for ws in tree.workspaces.iter_mut() {
        for strip in ws.strips.iter_mut() {
            for group in strip.window_groups.iter_mut() {
                for tab in group.tabs.iter_mut() {
                    if let TileContent::Foreign {
                        surface_id: s,
                        app_id,
                    } = &mut tab.tile
                    {
                        if *s == surface_id {
                            *app_id = new_app_id.to_string();
                            tab.title = foreign_tab_title(new_app_id);
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Tab-bar-friendly label for a wayland app-id (`org.example.MyApp` →
/// `MyApp`), capped so a pathological id can't blow out the tab strip.
fn foreign_tab_title(app_id: &str) -> String {
    let trimmed = app_id.split('.').next_back().unwrap_or(app_id);
    if trimmed.len() > 32 {
        trimmed[..32].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Reconcile the seat's keyboard focus with the shell's focused tab.
///
/// Called every tick: chrome capture (open overlay / palette / prompt)
/// drops client focus outright; otherwise focus follows the active tab, so
/// switching tabs, groups or workspaces moves the keyboard with it and a
/// closed window releases it.
pub fn sync_keyboard_focus(state: &mut State, fb: &FrameBuilder) {
    let target = if chrome_captures_input(fb) {
        None
    } else {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        active_foreign_surface(&tree).filter(|id| state.client_input.surfaces.contains_key(id))
    };
    state.set_client_keyboard_focus(target);
}

/// Tell the remote-input firewall what is on screen.
///
/// Called every tick, immediately after [`sync_keyboard_focus`], so a
/// remote event is judged against the screen the local user is actually
/// looking at — including which surface holds the keyboard, which is what
/// mutes a guest's keystrokes the instant the host focuses something
/// private.
///
/// The tile list comes from [`foreign_tiles`], the same enumeration
/// [`foreign_tile_at`] resolves local clicks with. That shared traversal
/// is the point: a clickjacking check that walked the tree separately
/// would eventually disagree with where a click really lands, and the
/// disagreement would be the hole.
///
/// `session_locked` has to be passed in because the lock surface is not
/// part of the tiled tree: the tiles underneath it are still laid out
/// and still topmost as far as the tree is concerned, so without this
/// the firewall would clear a guest's click on a window the local user
/// cannot reach without a password.
pub fn publish_remote_input_view(state: &mut State, fb: &FrameBuilder, session_locked: bool) {
    let tiles: Vec<(u64, Rect)> = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        foreign_tiles(&tree, fb.host())
            .into_iter()
            // A tab whose surface the compositor no longer holds is not a
            // place input can land, for the local user or a remote one.
            .filter(|hit| state.client_input.surfaces.contains_key(&hit.surface_id))
            .map(|hit| (hit.surface_id, hit.rect))
            .collect()
    };
    let view = crate::network::input_firewall::ShellView::new(
        tiles,
        state.client_input.keyboard_focus(),
        chrome_captures_input(fb),
        fb.host().palette_visible(),
    )
    .locked(session_locked);
    state.remote_input_firewall.publish_view(view);
}

/// Stop every peer from driving this seat, and say so on screen.
///
/// The action half of the `Super+Esc` kill switch; the backend loop
/// recognises the chord and calls this. Split out from that loop so the
/// behaviour can be driven from a test — the loop itself is a `-> !`
/// function that cannot be.
///
/// The confirmation is not decoration. Someone reaches for this key
/// because a pointer is moving on their screen and they did not move it;
/// what they need next is to know it stopped. So the toast is pushed
/// with [`Notification::bypassing_dnd`] — Do-Not-Disturb is a request
/// for quiet, and silently swallowing the answer to a panic key makes a
/// working kill switch indistinguishable from a broken one — and it is
/// persistent, because the stop is: nothing lifts it but the host.
///
/// Returns how many shares lost write access, which is what the toast
/// counts.
pub fn stop_remote_input(state: &mut State, fb: &mut FrameBuilder) -> usize {
    let revoked = state.engage_remote_input_kill_switch();
    fb.with_chrome(|chrome| {
        chrome.push_notification_full(
            Notification::new(
                "Remote input stopped",
                match revoked {
                    0 => "No peer was driving this session.".to_string(),
                    1 => "1 shared window is now view-only.".to_string(),
                    n => format!("{n} shared windows are now view-only."),
                },
            )
            .with_severity(NotificationSeverity::Warning)
            .with_icon_kind(NotificationIconKind::Network)
            .with_dismiss_after(PERSISTENT_DISMISS)
            .bypassing_dnd(),
        );
    });
    revoked
}

/// Route one backend input event.
///
/// Returns `true` when the event was consumed by a client surface and must
/// **not** also be dispatched into the shell chrome. Returns `false` for
/// everything the shell owns — chrome chords, overlay-captured input, and
/// any pointer position outside a client tile.
///
/// Compositor chords are checked before any forwarding, so a focused client
/// can never swallow `Super+L`, the search palette, or workspace switching.
pub fn route_input(
    state: &mut State,
    fb: &mut FrameBuilder,
    event: &WindowEvent,
    modifiers: &Modifiers,
) -> bool {
    // The host touching their own keyboard or mouse takes priority over
    // every remote peer for the next half second. Stamped here because
    // this is the one function both the live backend loop and the
    // test-control socket push local input through.
    state
        .remote_input_firewall
        .note_local_input(std::time::Instant::now());
    // Somebody is at this session. Stamped here rather than beside the
    // firewall's own note above because it answers a different question
    // — that one is "did the host just take the controls back from a
    // peer", this one is "is anyone here at all" — and the pointer
    // publisher reads it to decide whether it still has something to
    // say. Deliberately every input, not just the pointer arms: a
    // person typing has not moved their mouse and is plainly present.
    state.client_input.last_input = Some(std::time::Instant::now());
    let captured = chrome_captures_input(fb);
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            state.client_input.cursor = Some((position.x, position.y));
            let hit = current_pointer_target(state, fb, captured);
            deliver_pointer(state, hit);
            // During a chrome drag the file manager keeps drawing the ghost
            // under the cursor, so it sees every move even over a client.
            hit.is_some() && state.selection.chrome_drag.is_none()
        }
        WindowEvent::CursorLeft { .. } => {
            let global = state.client_input.cursor();
            state.forward_client_pointer_motion(global, None);
            state.client_input.grab = None;
            state.client_input.buttons_down = 0;
            false
        }
        WindowEvent::MouseInput {
            state: element,
            button,
            ..
        } => {
            let pressed = *element == ElementState::Pressed;
            if pressed {
                if state.client_input.buttons_down == 0 {
                    // Latch the implicit grab from a fresh hit test, so a
                    // drag that leaves the tile stays with its owner.
                    let hit = current_pointer_target(state, fb, captured);
                    deliver_pointer(state, hit);
                    state.client_input.grab = Some(match hit {
                        Some(h) => PointerOwner::Client(h.surface_id),
                        None => PointerOwner::Chrome,
                    });
                    // Click-to-focus: raise the clicked window group so the
                    // keyboard follows the pointer into the client.
                    if let Some(h) = hit {
                        raise_group(fb, h.strip, h.group);
                        sync_keyboard_focus(state, fb);
                    }
                }
                state.client_input.buttons_down += 1;
            } else {
                state.client_input.buttons_down = state.client_input.buttons_down.saturating_sub(1);
            }
            // A chrome drag's release is the drop: the server-side grab
            // smithay holds only ends on a button event, whether or not a
            // client is under the pointer (no client → cancelled).
            let chrome_drag = state.selection.chrome_drag.is_some();
            let to_client = (matches!(state.client_input.grab, Some(PointerOwner::Client(_)))
                && state.client_input.pointer_focus.is_some())
                || (chrome_drag && !pressed);
            let consumed = if to_client {
                match evdev_button(*button) {
                    Some(code) => state.forward_client_pointer_button(code, pressed),
                    None => false,
                }
            } else {
                false
            };
            if state.client_input.buttons_down == 0 {
                state.client_input.grab = None;
                if !pressed {
                    // Re-resolve focus now the grab is gone — the release may
                    // have happened over a different tile (or over chrome).
                    let hit = current_pointer_target(state, fb, captured);
                    deliver_pointer(state, hit);
                }
            }
            // The file manager has to see the release too, to end its own
            // side of the gesture without moving anything.
            consumed && !chrome_drag
        }
        WindowEvent::MouseWheel { delta, .. } => {
            // Niri parity: an Alt-modified wheel is a compositor
            // navigation gesture, grabbed before client forwarding like
            // every shell chord, so it works with the cursor over a
            // client window too — and reported as consumed either way
            // so the chrome never double-handles the same event.
            //
            //   Alt+wheel        → switch workspace (Niri's
            //                      `Mod+WheelScrollDown → focus-workspace-down`)
            //   Alt+Shift+wheel  → jump focus to the neighbouring group
            //                      of the active strip (Niri's
            //                      `Mod+Shift+WheelScrollDown → focus-column-right`)
            let mods = modifiers.state();
            if mods.alt_key() && !mods.control_key() && !mods.super_key() {
                let detents = wheel_jump_detents(delta);
                let steps = state.client_input.jump_wheel.push(detents, Instant::now());
                // Diagnostic for gesture-feel reports: shows exactly what
                // the host session delivers per wheel event (LineDelta
                // notches vs touchpad/hi-res pixel fractions) and how
                // many jumps it produced. `RUST_LOG=shell=debug`.
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
                        let target = {
                            let chrome = fb.chrome();
                            let tree = chrome.tree();
                            step_active_window(&tree, steps)
                        };
                        if let Some((strip, group, tab)) = target {
                            // Select the window (its tab) and focus its
                            // group — the same state a click on the tab
                            // pill produces — then hand the keyboard to
                            // whatever the tab hosts. The per-tick
                            // reconciler slides the strip to the group.
                            focus_window(fb, strip, group, tab);
                            sync_keyboard_focus(state, fb);
                        }
                    } else {
                        // Wheel-down (positive detents) rotates DOWN the
                        // workspace wheel, one workspace per detent.
                        // `switch_workspace` stops at the wheel's end,
                        // so the loop breaks rather than spinning.
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
                state.forward_client_pointer_axis(*delta)
            } else {
                false
            }
        }
        WindowEvent::KeyboardInput { event: key, .. } => {
            let pressed = key.state == ElementState::Pressed;
            let Some(code) = evdev_keycode(key) else {
                return false;
            };
            // Anything the shell has up that Escape would dismiss. The
            // summoned workspace wheel counts: it is dismissed with
            // Escape, so the key has to reach the shell rather than the
            // focused client while it is showing.
            let overlay_open = fb.host().palette_visible() || fb.host().wheel_summoned();
            let shell_owns = captured || is_shell_chord(event, modifiers, overlay_open);
            let forward = if pressed {
                let forward = !shell_owns && state.client_input.keyboard_focus.is_some();
                if forward {
                    state.client_input.forwarded_keys.insert(code);
                }
                forward
            } else {
                // A release mirrors its press exactly — see
                // `ClientInput::forwarded_keys`.
                state.client_input.forwarded_keys.remove(&code)
            };
            // Runs for shell-owned keys too: the seat's xkb state has to
            // track every key, or its modifier view drifts (see
            // `deliver_client_key`).
            state.deliver_client_key(code, pressed, forward)
        }
        _ => false,
    }
}

/// Promote a file drag from the hosted file manager into a Wayland drag.
///
/// Called every tick. Files publishes what it is carrying
/// (`file_explorer::external_drag`); the moment that drag — still an
/// in-process gesture under the chrome's implicit grab — has the pointer
/// over a foreign tile, the compositor claims it, starts smithay's
/// server-side drag offering the file, and delivers the enter. From then
/// on `current_pointer_target` hit-tests normally so the client under the
/// pointer receives `wl_data_device` enter/motion/drop, and the release is
/// the drop. Files learns it was handed off and does not act on the
/// release itself.
pub fn pump_chrome_drag(state: &mut State, fb: &FrameBuilder) {
    if state.selection.chrome_drag.is_some() || state.selection.dnd_active {
        return;
    }
    let Some(pending) = file_explorer::external_drag::unclaimed() else {
        return;
    };
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
        dnd_action: smithay::reexports::wayland_server::protocol::wl_data_device_manager::DndAction::Copy,
    };
    // This drag's file names are known, so the picture the compositor
    // draws over the desktop can say which file rather than which kind.
    state
        .selection
        .dnd_label
        .set(Some(crate::compositor::drag_feedback::label_for_paths(&drag.paths)));
    state.selection.chrome_drag = Some(drag);
    let serial = mint_serial(state);
    let location = SmithayPoint::from(state.client_input.cursor());
    // The gesture began on chrome, so there is no surface focus to record;
    // the grab learns its target from the motion delivered right after.
    let start = smithay::input::pointer::GrabStartData {
        focus: None,
        button: BTN_LEFT,
        location,
    };
    let dh = state.display_handle.clone();
    let seat = state.seat.clone();
    smithay::wayland::selection::data_device::start_dnd(&dh, &seat, state, serial, Some(start), None, metadata);
    deliver_pointer(state, Some(hit));
}

/// Where the pointer should be focused right now: the foreign tile under
/// the cursor, or the implicit grab's owner while a button is held.
fn current_pointer_target(state: &State, fb: &FrameBuilder, captured: bool) -> Option<TileHit> {
    match state.client_input.grab {
        // A file carried out of the hosted file manager: the chrome still
        // owns the gesture, but the drop target is whichever client the
        // pointer is over, so hit-test normally — exactly as for a client's
        // own drag below.
        Some(PointerOwner::Chrome) if state.selection.chrome_drag.is_some() => {
            if captured {
                return None;
            }
            let chrome = fb.chrome();
            let tree = chrome.tree();
            foreign_tile_at(&tree, fb.host(), state.client_input.cursor())
                .filter(|h| state.client_input.surfaces.contains_key(&h.surface_id))
        }
        // A grab owned by the chrome keeps the client out of the loop even
        // when the cursor wanders across a tile mid-drag.
        Some(PointerOwner::Chrome) => None,
        // A drag-and-drop is the one held-button gesture that is *supposed*
        // to leave the surface it started on: the drop target only learns
        // about the drag when the pointer is reported over it. Pinning focus
        // to the source, which is right for every other implicit grab, means
        // no `wl_data_device.enter` ever reaches another client and nothing
        // can be dropped anywhere. So while a client drag is in flight the
        // pointer hit-tests normally.
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
            let chrome = fb.chrome();
            let tree = chrome.tree();
            foreign_tile_at(&tree, fb.host(), state.client_input.cursor())
                .filter(|h| state.client_input.surfaces.contains_key(&h.surface_id))
        }
    }
}

/// Push the current cursor position at `hit` (or clear client focus when
/// `None`), producing the enter / motion / leave sequence.
fn deliver_pointer(state: &mut State, hit: Option<TileHit>) {
    let global = state.client_input.cursor();
    let target = hit.map(|h| (h.surface_id, (h.rect.x0, h.rect.y0)));
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

/// Focus one window: select `tab` inside `group` and make `group` the
/// active group of `strip` — the same state a click on the tab's pill
/// produces, so `sync_keyboard_focus` and the focus-follow reconciler
/// behave identically to a click.
///
/// The tab is selected on the WIDGET side first: the bar's `active_id`
/// is authoritative, and the per-frame `harvest_active_tab_writeback`
/// mirrors widgets into the tree — so a tree-only write would be
/// overwritten one frame later. The tree is then stamped too, so
/// keyboard focus and `state_dump` are correct within this same event.
fn focus_window(fb: &mut FrameBuilder, strip: StripId, group: GroupId, tab: u64) {
    fb.host_mut().focus_window_widget(strip, group, tab);
    let mut changed = false;
    fb.with_chrome(|chrome| {
        {
            let mut tree = chrome.tree_mut();
            if let Some(g) = tree
                .active_workspace_mut()
                .and_then(|ws| ws.strips.iter_mut().find(|s| s.id == strip))
                .and_then(|s| s.window_groups.iter_mut().find(|g| g.id == group))
            {
                if g.active_tab != tab {
                    g.active_tab = tab;
                    changed = true;
                }
            }
            if tree.active_group_for(strip) != Some(group) {
                tree.set_active_group(strip, group);
                changed = true;
            }
        }
        if changed {
            chrome.bump();
        }
    });
}

/// Make `group` the active group of `strip` in the chrome's tree so the
/// shell's focus rules (and therefore `sync_keyboard_focus`) agree with
/// what the user just clicked.
fn raise_group(fb: &mut FrameBuilder, strip: StripId, group: GroupId) {
    let mut changed = false;
    fb.with_chrome(|chrome| {
        {
            let mut tree = chrome.tree_mut();
            if tree.active_group_for(strip) != Some(group) {
                tree.set_active_group(strip, group);
                changed = true;
            }
        }
        if changed {
            chrome.bump();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{AppKind, Strip, Workspace, WorkspaceId};
    use std::time::Duration;
    use winit::event::{DeviceId, KeyEvent};
    use winit::keyboard::{KeyLocation, ModifiersState, PhysicalKey, SmolStr};

    fn key_event(logical: Key, pressed: bool) -> WindowEvent {
        // Mirrors the synthesis helpers already used by the test-control
        // executor: winit's `KeyEvent` has a private platform field, so the
        // struct is zero-initialised and every public field overwritten.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        };
        ev.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed::<DeviceId>() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn mods(state: ModifiersState) -> Modifiers {
        Modifiers::from(state)
    }

    /// A workspace tree with one strip carrying two groups: a shell-owned
    /// stub and a foreign client on `surface_id`.
    fn tree_with_foreign(surface_id: u64) -> (WorkspaceTree, StripId, GroupId) {
        let stub_tab = Tab {
            id: next_id(),
            title: "Terminal".into(),
            tile: TileContent::Stub {
                kind: AppKind::Terminal,
                label: "Terminal".into(),
            },
        };
        let stub_tab_id = stub_tab.id;
        let stub = WindowGroup {
            id: next_id(),
            tabs: vec![stub_tab],
            active_tab: stub_tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let foreign_tab = Tab {
            id: next_id(),
            title: "client".into(),
            tile: TileContent::Foreign {
                app_id: "org.example.client".into(),
                surface_id,
            },
        };
        let foreign_tab_id = foreign_tab.id;
        let foreign = WindowGroup {
            id: next_id(),
            tabs: vec![foreign_tab],
            active_tab: foreign_tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let foreign_group = foreign.id;
        let strip_id = StripId::from(next_id());
        let mut strip = Strip::new(strip_id, "main");
        strip.window_groups = vec![stub, foreign];
        let ws_id = WorkspaceId::from(next_id());
        let mut ws = Workspace::new(ws_id, "Home");
        ws.strips = vec![strip];
        let mut tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            ..Default::default()
        };
        tree.active_strip.insert(ws_id, strip_id);
        (tree, strip_id, foreign_group)
    }

    #[test]
    fn active_foreign_surface_follows_the_active_group() {
        let (mut tree, strip, foreign_group) = tree_with_foreign(4242);
        // Active group defaults to the first (shell-owned) group — no
        // client should hold the keyboard.
        assert_eq!(
            active_foreign_surface(&tree),
            None,
            "a shell-owned active tab must not hand the keyboard to a client"
        );
        // Switching the active group onto the foreign one moves focus.
        tree.set_active_group(strip, foreign_group);
        assert_eq!(active_foreign_surface(&tree), Some(4242));
    }

    #[test]
    fn step_active_window_walks_tabs_within_a_group() {
        // The boot-state shape: ONE group holding several windows as
        // tabs. One step right of the first window is the second — the
        // "Notes → Files" case the gesture is for.
        let tabs: Vec<Tab> = ["Notes", "Files", "Terminal"]
            .iter()
            .map(|name| Tab {
                id: next_id(),
                title: (*name).into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other((*name).into()),
                    label: (*name).into(),
                },
            })
            .collect();
        let ids: Vec<u64> = tabs.iter().map(|t| t.id).collect();
        let group = WindowGroup {
            id: next_id(),
            active_tab: ids[0],
            tabs,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let group_id = group.id;
        let strip_id = StripId::from(next_id());
        let mut strip = Strip::new(strip_id, "main");
        strip.window_groups = vec![group];
        let ws_id = WorkspaceId::from(next_id());
        let mut ws = Workspace::new(ws_id, "Home");
        ws.strips = vec![strip];
        let mut tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            ..Default::default()
        };
        tree.active_strip.insert(ws_id, strip_id);

        // Notes is active → one step right is Files.
        assert_eq!(
            step_active_window(&tree, 1),
            Some((strip_id, group_id, ids[1])),
            "one step right of the first tab must be the second"
        );
        // Apply it, step again → Terminal; then the right edge clamps.
        tree.workspaces[0].strips[0].window_groups[0].active_tab = ids[1];
        assert_eq!(step_active_window(&tree, 1), Some((strip_id, group_id, ids[2])));
        tree.workspaces[0].strips[0].window_groups[0].active_tab = ids[2];
        assert_eq!(step_active_window(&tree, 1), None, "right edge clamps");
        assert_eq!(step_active_window(&tree, 5), None, "overshoot clamps too");
        // And back left, two at once, lands on Notes; the left edge clamps.
        assert_eq!(step_active_window(&tree, -2), Some((strip_id, group_id, ids[0])));
        tree.workspaces[0].strips[0].window_groups[0].active_tab = ids[0];
        assert_eq!(step_active_window(&tree, -1), None, "left edge clamps");
    }

    #[test]
    fn step_active_window_continues_into_the_next_group() {
        let (mut tree, strip, foreign_group) = tree_with_foreign(1);
        // No explicit active pointer yet: the walk starts at the first
        // group's tab; one step right crosses into the foreign group.
        let (s, g, _t) = step_active_window(&tree, 1).expect("step into the next group");
        assert_eq!((s, g), (strip, foreign_group));
        tree.set_active_group(strip, foreign_group);
        // At the strip's last window the jump clamps.
        assert_eq!(step_active_window(&tree, 1), None);
        // Stepping back crosses into the first group again.
        let (s2, first, _t) = step_active_window(&tree, -1).expect("step back left");
        assert_eq!(s2, strip);
        assert_ne!(first, foreign_group);
    }

    #[test]
    fn wheel_jump_detents_map_wheel_down_to_a_rightward_jump() {
        use winit::dpi::PhysicalPosition;
        // Winit: positive y = wheel up. Niri: WheelScrollDown →
        // focus-column-right — so wheel-down must be a positive detent.
        assert_eq!(wheel_jump_detents(&MouseScrollDelta::LineDelta(0.0, -1.0)), 1.0);
        assert_eq!(wheel_jump_detents(&MouseScrollDelta::LineDelta(0.0, 1.0)), -1.0);
        // Touchpad pixels are fractional: one detent per JUMP_PX_PER_DETENT.
        assert_eq!(
            wheel_jump_detents(&MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -25.0))),
            0.5
        );
    }

    #[test]
    fn wheel_jump_accum_fires_whole_detents_and_survives_slow_scrolling() {
        let t0 = Instant::now();
        let mut acc = WheelJumpAccum::default();
        // A clicky wheel fires one step per notch, immediately.
        assert_eq!(acc.push(1.0, t0), 1);
        assert_eq!(acc.push(1.0, t0), 1);
        // Touchpad travel accumulates until a whole detent is reached...
        let mut acc = WheelJumpAccum::default();
        assert_eq!(acc.push(0.4, t0), 0);
        assert_eq!(acc.push(0.4, t0 + Duration::from_millis(50)), 0);
        assert_eq!(acc.push(0.4, t0 + Duration::from_millis(100)), 1);
        // ...and the remainder carries forward, not doubled.
        assert_eq!(acc.push(0.4, t0 + Duration::from_millis(150)), 0);
        // Reversing direction drops the partial balance outright.
        let mut acc = WheelJumpAccum::default();
        assert_eq!(acc.push(0.9, t0), 0);
        assert_eq!(acc.push(-0.9, t0 + Duration::from_millis(50)), 0);
        // SLOW deliberate scrolling still gets there: fractional detents
        // separated by long pauses must keep accumulating — a hi-res
        // wheel notching at 1 Hz is not a new gesture each notch. This
        // was the live "jump works once, then I can not scroll further"
        // bug: a quiet-time decay wiped the balance between notches.
        let mut acc = WheelJumpAccum::default();
        assert_eq!(acc.push(0.4, t0), 0);
        assert_eq!(acc.push(0.4, t0 + Duration::from_secs(1)), 0);
        assert_eq!(
            acc.push(0.4, t0 + Duration::from_secs(2)),
            1,
            "slow same-direction scrolling must still reach a whole detent"
        );
    }

    #[test]
    fn active_foreign_surface_ignores_inactive_workspaces() {
        let (mut tree, strip, foreign_group) = tree_with_foreign(7);
        tree.set_active_group(strip, foreign_group);
        assert_eq!(active_foreign_surface(&tree), Some(7));
        // Point the tree at a workspace that doesn't exist: nothing to focus.
        tree.active = crate::shell::state::WorkspaceId::from(u64::MAX);
        assert_eq!(
            active_foreign_surface(&tree),
            None,
            "focus must not leak across workspaces"
        );
    }

    #[test]
    fn surface_local_subtracts_the_tile_origin() {
        let rect = Rect::new(320.0, 96.0, 1220.0, 800.0);
        assert_eq!(surface_local((320.0, 96.0), rect), (0.0, 0.0));
        assert_eq!(surface_local((420.0, 196.0), rect), (100.0, 100.0));
        // A point outside the rect still translates linearly — the caller
        // is responsible for hit-testing first.
        assert_eq!(surface_local((300.0, 96.0), rect), (-20.0, 0.0));
    }

    #[test]
    fn super_chords_never_reach_a_client() {
        let m = mods(ModifiersState::SUPER);
        for key in [
            Key::Character(SmolStr::new("l")),
            Key::Character(SmolStr::new("k")),
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::Tab),
            Key::Named(NamedKey::ArrowUp),
            Key::Named(NamedKey::ArrowDown),
        ] {
            let ev = key_event(key.clone(), true);
            assert!(
                is_shell_chord(&ev, &m, false),
                "Super+{key:?} must be handled by the shell"
            );
        }
    }

    #[test]
    fn ctrl_and_alt_shell_chords_are_reserved() {
        let ctrl = mods(ModifiersState::CONTROL);
        let alt = mods(ModifiersState::ALT);
        assert!(is_shell_chord(
            &key_event(Key::Character(SmolStr::new("k")), true),
            &ctrl,
            false
        ));
        assert!(is_shell_chord(
            &key_event(Key::Named(NamedKey::Tab), true),
            &ctrl,
            false
        ));
        assert!(is_shell_chord(
            &key_event(Key::Named(NamedKey::ArrowDown), true),
            &ctrl,
            false
        ));
        assert!(is_shell_chord(
            &key_event(Key::Named(NamedKey::Space), true),
            &alt,
            false
        ));
    }

    #[test]
    fn plain_keys_and_bare_modifiers_are_forwarded() {
        let none = mods(ModifiersState::empty());
        let ctrl = mods(ModifiersState::CONTROL);
        // Unmodified typing goes to the client.
        assert!(!is_shell_chord(
            &key_event(Key::Character(SmolStr::new("a")), true),
            &none,
            false
        ));
        // Ctrl chords the shell does NOT own stay with the client (Ctrl+C
        // in a terminal must not be eaten).
        assert!(!is_shell_chord(
            &key_event(Key::Character(SmolStr::new("c")), true),
            &ctrl,
            false
        ));
        // A bare modifier is always forwarded so the client's xkb state
        // tracks the seat's, even while Super is held.
        assert!(!is_shell_chord(
            &key_event(Key::Named(NamedKey::Super), true),
            &mods(ModifiersState::SUPER),
            false
        ));
        assert!(!is_shell_chord(
            &key_event(Key::Named(NamedKey::Shift), true),
            &mods(ModifiersState::SHIFT),
            false
        ));
    }

    #[test]
    fn escape_only_belongs_to_the_shell_while_an_overlay_is_open() {
        let none = mods(ModifiersState::empty());
        let ev = key_event(Key::Named(NamedKey::Escape), true);
        assert!(
            !is_shell_chord(&ev, &none, false),
            "Escape must reach a focused client when no overlay is open"
        );
        assert!(
            is_shell_chord(&ev, &none, true),
            "Escape closes the open overlay instead of reaching the client"
        );
    }

    #[test]
    fn evdev_keycodes_match_the_kernel_table() {
        // Values from linux/input-event-codes.h — the codes
        // `wl_keyboard.key` carries on the wire.
        assert_eq!(
            evdev_from_logical(&Key::Character(SmolStr::new("a"))),
            Some(30)
        );
        assert_eq!(
            evdev_from_logical(&Key::Character(SmolStr::new("A"))),
            Some(30)
        );
        assert_eq!(
            evdev_from_logical(&Key::Character(SmolStr::new("q"))),
            Some(16)
        );
        assert_eq!(
            evdev_from_logical(&Key::Character(SmolStr::new("1"))),
            Some(2)
        );
        assert_eq!(
            evdev_from_logical(&Key::Character(SmolStr::new("0"))),
            Some(11)
        );
        assert_eq!(evdev_from_logical(&Key::Named(NamedKey::Enter)), Some(28));
        assert_eq!(evdev_from_logical(&Key::Named(NamedKey::Escape)), Some(1));
        assert_eq!(evdev_from_logical(&Key::Named(NamedKey::Space)), Some(57));
        assert_eq!(
            evdev_from_logical(&Key::Named(NamedKey::Backspace)),
            Some(14)
        );
        assert_eq!(
            evdev_from_logical(&Key::Named(NamedKey::ArrowLeft)),
            Some(105)
        );
        assert_eq!(evdev_from_logical(&Key::Named(NamedKey::Shift)), Some(42));
    }

    #[test]
    fn evdev_keycode_prefers_the_physical_scancode() {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        // `KeyA` is evdev 30; the logical key deliberately disagrees so a
        // fallback to the table would produce a different value.
        ev.physical_key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyA);
        ev.logical_key = Key::Character(SmolStr::new("z"));
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        assert_eq!(
            evdev_keycode(&ev),
            Some(30),
            "the physical scancode is authoritative when winit reports one"
        );
    }

    #[test]
    fn mouse_buttons_map_to_evdev_codes() {
        assert_eq!(evdev_button(MouseButton::Left), Some(0x110));
        assert_eq!(evdev_button(MouseButton::Right), Some(0x111));
        assert_eq!(evdev_button(MouseButton::Middle), Some(0x112));
        assert_eq!(evdev_button(MouseButton::Other(9)), None);
    }

    #[test]
    fn scroll_deltas_invert_for_the_wayland_axis_convention() {
        // Winit: positive y = scroll up. Wayland: positive = scroll down.
        let ((h, v), v120) = axis_values(MouseScrollDelta::LineDelta(0.0, 1.0));
        assert_eq!(h, 0.0);
        assert_eq!(v, -WHEEL_STEP);
        assert_eq!(v120, Some((0, -120)));
        let ((h, v), v120) = axis_values(MouseScrollDelta::PixelDelta(
            winit::dpi::PhysicalPosition::new(3.0, -7.0),
        ));
        assert_eq!((h, v), (-3.0, 7.0));
        assert_eq!(v120, None, "trackpad deltas carry no discrete steps");
    }

    #[test]
    fn insert_foreign_group_lands_in_the_active_strip_and_takes_focus() {
        let (mut tree, strip, _) = tree_with_foreign(1);
        let before = tree
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .find(|s| s.id == strip)
            .unwrap()
            .window_groups
            .len();
        let placed = insert_foreign_group(&mut tree, 99, Some("org.example.late"))
            .expect("adoption must land in the active strip");
        assert_eq!(placed.0, strip);
        let live = tree
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .find(|s| s.id == strip)
            .unwrap();
        assert_eq!(live.window_groups.len(), before + 1);
        assert_eq!(
            tree.active_group_for(strip),
            Some(placed.1),
            "a newly-mapped window takes the active-group slot"
        );
        assert_eq!(
            active_foreign_surface(&tree),
            Some(99),
            "and therefore the keyboard focus"
        );
    }

    #[test]
    fn remove_foreign_tab_drops_the_group_and_moves_focus_off_it() {
        let (mut tree, strip, foreign_group) = tree_with_foreign(1);
        let placed = insert_foreign_group(&mut tree, 99, None).unwrap();
        assert_eq!(active_foreign_surface(&tree), Some(99));
        assert!(remove_foreign_tab(&mut tree, 99));
        let live = tree
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .find(|s| s.id == strip)
            .unwrap();
        assert!(
            !live.window_groups.iter().any(|g| g.id == placed.1),
            "an emptied group must not linger in the strip"
        );
        assert_eq!(
            tree.active_group_for(strip),
            Some(foreign_group),
            "focus must move off the destroyed group onto a surviving sibling"
        );
        assert_eq!(
            active_foreign_surface(&tree),
            Some(1),
            "the keyboard lands on the surviving window, never on the dead one"
        );
        // A second removal is a no-op.
        assert!(!remove_foreign_tab(&mut tree, 99));

        // Draining the last foreign group leaves the shell-owned group
        // active, so no client holds the keyboard.
        assert!(remove_foreign_tab(&mut tree, 1));
        assert_eq!(
            active_foreign_surface(&tree),
            None,
            "no client holds the keyboard once every client window is gone"
        );
    }

    #[test]
    fn rename_rewrites_the_app_id_and_title() {
        let (mut tree, _, _) = tree_with_foreign(1);
        insert_foreign_group(&mut tree, 99, None).unwrap();
        assert!(rename_foreign_tab(&mut tree, 99, "org.example.Renamed"));
        let tab = tree
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .flat_map(|g| g.tabs.iter())
            .find(|t| matches!(&t.tile, TileContent::Foreign { surface_id: 99, .. }))
            .expect("mirrored tab still present");
        assert_eq!(tab.title, "Renamed");
        match &tab.tile {
            TileContent::Foreign { app_id, .. } => assert_eq!(app_id, "org.example.Renamed"),
            other => panic!("expected Foreign tile, got {other:?}"),
        }
        assert!(!rename_foreign_tab(&mut tree, 12345, "nope"));
    }

    /// The geometry contract the pointer router depends on: a foreign
    /// toplevel mirrored into the chrome's tree becomes a live window
    /// group whose *tile* rect is laid out, hit-testable, and translates
    /// a global cursor into surface-local coordinates. Runs against the
    /// real `FrameBuilder` (the same one the compositor paints with), so
    /// The peer-sync card must take the keyboard away from a focused
    /// client, or it cannot be dismissed.
    ///
    /// Its handler runs before every other overlay and claims that while
    /// it is up every key belongs to it — but a key only reaches the
    /// overlay host when this predicate says the chrome has taken input.
    /// It did not name the card, so a card raised over a focused client
    /// delivered its Escape to the client: the card stayed on screen for
    /// the rest of the session, showing a live invite token, with every
    /// window that opened afterwards drawn behind it.
    ///
    /// Asserted on `chrome_captures_input` itself — the function the
    /// delivery path actually consults — for both fields the card is
    /// painted from, since the connect prompt sets the other one.
    #[test]
    fn the_peer_sync_card_takes_input_from_a_focused_client() {
        for field in ["invite_token", "connect_input"] {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            assert!(
                !chrome_captures_input(&fb),
                "nothing is up yet, so a client should still own the keyboard"
            );

            fb.with_chrome(|chrome| {
                match field {
                    "invite_token" => {
                        chrome.peer_sync_invite_token = Some("a-signed-blob".into())
                    }
                    _ => chrome.peer_sync_connect_input = Some(String::new()),
                }
                chrome.bump();
            });
            let _ = fb.paint_settled();

            assert!(
                chrome_captures_input(&fb),
                "with peer_sync_{field} set the card is painted, so the chrome \
                 must hold the keyboard — otherwise Escape goes to the client \
                 and the card can never be dismissed"
            );
        }
    }

    /// The portal's file chooser is modal, so it must hold input while
    /// it is up.
    ///
    /// The same failure as the peer-sync card above, found the same way
    /// and worth pinning for the same reason: the dialog was painted
    /// full-screen over a scrim while `chrome_captures_input` said
    /// nothing was up, so every click in it hit-tested straight through
    /// to the app underneath. An app could ask for a file, the dialog
    /// would appear, and neither a row nor Open nor Escape could be
    /// reached with a mouse or a keyboard — the chooser was a picture of
    /// a chooser.
    ///
    /// Asserted on `chrome_captures_input` itself, with the dialog
    /// mounted the way the compositor mounts one (a request through the
    /// portal's own queue, drained by `pump_file_picker`), so this
    /// cannot pass against a dialog that production would never raise.
    #[test]
    fn the_file_chooser_takes_input_from_a_focused_client() {
        use crate::portal::FilePickerSpawner as _;

        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        assert!(
            !chrome_captures_input(&fb),
            "nothing is up yet, so a client should still own the pointer"
        );

        let queue = crate::portal::FilePickerQueue::new();
        let prompts = crate::portal::PromptQueueHandle::new(
            crate::portal::PromptResolver::AutoAllow,
        );
        fb.host_mut().set_portal_queues(prompts, queue.clone());

        let workspace = fb
            .chrome()
            .tree()
            .active_workspace()
            .map(|ws| ws.id)
            .expect("a fresh FrameBuilder has an active workspace");
        let (reply, _parked) = tokio::sync::oneshot::channel();
        queue.spawn(crate::portal::FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "dev.qos.tests"),
            title: "Insert image".into(),
            save: false,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
            reply,
        });
        assert!(
            fb.host_mut().pump_file_picker(),
            "the queued request should have mounted a dialog"
        );
        assert!(
            fb.host().file_picker().is_some(),
            "the dialog is what the predicate below is about"
        );
        let _ = fb.paint_settled();

        assert!(
            chrome_captures_input(&fb),
            "with the portal's file chooser up the chrome must hold input — \
             otherwise the clicks meant for it go to the client behind it and \
             the dialog cannot be used at all"
        );
    }

    /// a regression in the tree→widget resync or in
    /// `ShellOverlayHost::active_tile_rect` fails here.
    #[test]
    fn a_mirrored_foreign_group_is_laid_out_and_hit_testable() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();

        let mut placed = None;
        fb.with_chrome(|chrome| {
            {
                let mut tree = chrome.tree_mut();
                placed = insert_foreign_group(&mut tree, 4242, Some("org.example.client"));
            }
            chrome.bump();
        });
        let (_strip, group) = placed.expect("adoption lands in the bootstrap strip");

        // One settle pass runs the tree→row resync and the layout that
        // gives the freshly-spliced group its rect.
        let _ = fb.paint_settled();

        let rect = fb
            .host()
            .active_tile_rect(group)
            .expect("the mirrored group must exist in the live strip row");
        assert!(
            rect.width() > 1.0 && rect.height() > 1.0,
            "the client's tile must have a real extent; got {rect:?}"
        );

        let chrome = fb.chrome();
        let tree = chrome.tree();
        let centre = (rect.center().x, rect.center().y);
        let hit = foreign_tile_at(&tree, fb.host(), centre)
            .expect("a cursor inside the tile resolves to the client");
        assert_eq!(hit.surface_id, 4242);
        assert_eq!(hit.group, group);
        assert_eq!(hit.rect, rect);

        // Surface-local translation is the tile origin subtracted.
        assert_eq!(
            surface_local(centre, hit.rect),
            (centre.0 - rect.x0, centre.1 - rect.y0)
        );

        // Chrome keeps priority: a point on the top bar, and a point on
        // the tile's own tab bar (just above the tile rect), are not the
        // client's.
        assert!(
            foreign_tile_at(&tree, fb.host(), (800.0, 4.0)).is_none(),
            "the top bar must never resolve to a client surface"
        );
        assert!(
            foreign_tile_at(&tree, fb.host(), (centre.0, rect.y0 - 4.0)).is_none(),
            "the tab strip above the tile must never resolve to a client surface"
        );
    }

    /// The remote-input firewall's z-order check and local pointer
    /// routing must never disagree about what is on top, because the
    /// disagreement *is* the clickjacking hole: a point local input would
    /// deliver to one surface, the firewall would clear for another.
    ///
    /// They share [`foreign_tiles`], so this asserts the property that
    /// sharing is supposed to buy, on the real laid-out tree.
    #[test]
    fn the_firewall_view_and_local_hit_testing_resolve_the_same_surface() {
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

        let chrome = fb.chrome();
        let tree = chrome.tree();
        let tiles = foreign_tiles(&tree, fb.host());
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
        );

        // Probe each tile's inside and the pixels just past each of its
        // edges: a hit-test discrepancy shows up at a boundary long
        // before it shows up in the middle. Derived from the rects the
        // layout actually produced rather than from fixed screen
        // coordinates, because a strip that scrolls a group out of the
        // viewport would otherwise turn this into a sweep over empty
        // space that agrees on `None` everywhere and proves nothing.
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
        // Plus a coarse sweep of the whole output, which is mostly
        // chrome: every one of those has to resolve to nothing on both
        // sides, or the firewall would clear a click on the top bar.
        for gx in 0..80 {
            for gy in 0..45 {
                probes.push((gx as f64 * 20.0 + 10.0, gy as f64 * 20.0 + 10.0));
            }
        }

        // Wherever local input would land on a client, the firewall must
        // name that same client; wherever local input lands on chrome,
        // the firewall must name nothing.
        let mut inside = 0;
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
        assert!(
            inside >= tiles.len() * 3,
            "only {inside} probes landed inside a tile across {} tiles, so \
             agreement on a hit was never actually tested",
            tiles.len(),
        );
    }

    /// A tab is a place input can land only while the compositor still
    /// holds the client's surface. A tab left in the tree after its
    /// toplevel went away is a rectangle with nothing behind it, and
    /// publishing it would hand a guest a coordinate the local user
    /// cannot even click on.
    ///
    /// Drives the real publisher against the real laid-out tree, then
    /// asks the firewall the question a peer's event asks.
    #[test]
    fn a_tab_whose_surface_the_compositor_does_not_hold_is_not_published() {
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

    const ORPHAN_SURFACE: u64 = 900_003;

    /// The lock surface is drawn by the session, not by the tiled tree,
    /// so the tiles beneath it stay laid out and stay topmost as far as
    /// the tree knows. The publisher has to carry the lock across, or a
    /// guest keeps driving a window the host needs a password to reach.
    #[test]
    fn a_locked_session_is_carried_into_the_published_view() {
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

    /// A `State` with a live seat, one peer holding write access to
    /// `surface`, and that surface on screen with the keyboard.
    /// A session nobody has pointed at holds no pointer position, and
    /// holds one the instant a real motion is routed through.
    ///
    /// Both halves of the production path in one place: `route_input`
    /// is the only writer of this field, and `pointer_position` is what
    /// the frame loop publishes to the room from. They are asserted
    /// together because the two accessors disagree *by design* —
    /// `cursor()` answers the origin for a pointer that has never been
    /// anywhere, since the hit-testing callers need a point and have
    /// nowhere else to get one. The origin is also a perfectly good
    /// place to point, so that answer is safe to route input with and
    /// unsafe to broadcast: publishing it announced a cursor, in the
    /// corner of every peer's screen, for somebody who was not there.
    #[test]
    fn a_pointer_that_has_never_moved_has_no_position() {
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

        // The one thing that gives a session a pointer, through the
        // function every source of motion goes through.
        route_input(
            &mut state,
            &mut fb,
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
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

    /// A session that was pointed at once, a long time ago, is not
    /// pointing at anything now.
    ///
    /// The case the previous gate could not see. It refused a pointer
    /// that had never been anywhere, which is right and which a session
    /// stops being the instant anything presses a button on it — and
    /// pressing a button is how a session joins a room. From then on
    /// the position is real, the gate passes it, and the keepalive
    /// restates it to the room every second for as long as the session
    /// lives, so one press at start-up puts a cursor and a name pill on
    /// every peer's screen permanently.
    ///
    /// Asserted through `route_input` rather than by writing the fields,
    /// because the bug was never in the fields: both of them were
    /// exactly right, and it was the frame loop's *question* that was
    /// wrong. `live_pointer` is the question it now asks.
    #[test]
    fn a_pointer_nobody_has_touched_in_a_long_time_is_not_pointing_at_anything() {
        let mut state = state_sharing(9_000_102);
        let mut fb = FrameBuilder::new((1920, 1014));
        let mods = Modifiers::default();
        let idle = Duration::from_secs(10);

        // Exactly what pressing Join does: the pointer goes to the
        // button and the button is pressed. Nothing after this touches
        // the session again.
        route_input(
            &mut state,
            &mut fb,
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(1178.31, 582.0),
            },
            &mods,
        );
        let pressed = state
            .client_input
            .last_input()
            .expect("routing a motion records that somebody was here");

        assert_eq!(
            state.client_input.pointer_position(),
            Some((1178.31, 582.0)),
            "the position is real - it is where the press landed",
        );
        assert_eq!(
            state.client_input.live_pointer(pressed, idle),
            Some((1178.31, 582.0)),
            "and at the moment of the press this session is pointing there",
        );

        // The session is left alone. No key, no wheel, no motion.
        let later = pressed + idle + Duration::from_secs(1);
        assert_eq!(
            state.client_input.pointer_position(),
            Some((1178.31, 582.0)),
            "where the pointer was put does not stop being true",
        );
        assert_eq!(
            state.client_input.live_pointer(later, idle),
            None,
            "but nobody is pointing at that button any more, so the session has nothing to announce",
        );

        // Anything at all brings it back, on the next frame.
        route_input(
            &mut state,
            &mut fb,
            &WindowEvent::MouseWheel {
                device_id: unsafe { std::mem::zeroed() },
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: winit::event::TouchPhase::Moved,
            },
            &mods,
        );
        let revived = state
            .client_input
            .last_input()
            .expect("a wheel is input too");
        assert!(revived > pressed, "the stamp moved forward");
        assert_eq!(
            state.client_input.live_pointer(revived, idle),
            Some((1178.31, 582.0)),
            "somebody is here again, and their pointer is where they left it",
        );
    }

    fn state_sharing(surface: u64) -> crate::compositor::state::State {
        use crate::network::input_firewall::ShellView;
        use crate::network::streaming::StreamingShare;
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        let event_loop: EventLoop<'static, crate::compositor::state::State> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<crate::compositor::state::State> =
            Display::new().expect("wayland display");
        let mut state =
            crate::compositor::state::State::new(event_loop.handle(), display.handle());
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

    /// The panic key, end to end from the action the chord triggers: a
    /// guest is driving the seat, the host presses it, the guest stops,
    /// and the host is told so.
    #[test]
    fn stopping_remote_input_halts_the_guest_and_says_so_on_screen() {
        use crate::network::input_firewall::Denial;

        let mut fb = FrameBuilder::new((1600, 900));
        let mut state = state_sharing(KILL_SURFACE);

        guest_motion(&mut state).expect("the guest is driving the seat to begin with");

        let revoked = stop_remote_input(&mut state, &mut fb);
        assert_eq!(revoked, 1, "the one writable share had to lose write access");

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
            toast.body.contains('1'),
            "the confirmation has to say what it stopped; body was {:?}",
            toast.body,
        );
        assert!(
            toast.dismiss_after >= std::time::Duration::from_secs(60),
            "a stop that lasts until the host lifts it cannot be confirmed by a \
             toast that slides away in five seconds",
        );
    }

    /// Do-Not-Disturb is a request for quiet, not a request to be kept
    /// uninformed about the machine's own security state. A suppressed
    /// confirmation makes a working kill switch look exactly like a
    /// broken one.
    #[test]
    fn the_kill_switch_confirmation_is_shown_under_do_not_disturb() {
        let mut fb = FrameBuilder::new((1600, 900));
        let mut state = state_sharing(KILL_SURFACE);
        fb.with_chrome(|chrome| chrome.set_dnd(true));

        stop_remote_input(&mut state, &mut fb);

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

    /// The notification stack is capped, and a guest driving a client can
    /// make that client post toasts. Filling the stack must not be a way
    /// to keep the host from seeing that their panic key worked.
    #[test]
    fn a_flood_of_app_toasts_cannot_push_the_confirmation_off_the_screen() {
        let mut fb = FrameBuilder::new((1600, 900));
        let mut state = state_sharing(KILL_SURFACE);
        fb.with_chrome(|chrome| {
            for i in 0..crate::shell::notification::MAX_NOTIFICATIONS * 3 {
                chrome.push_notification(format!("App toast {i}"), "noise");
            }
        });

        stop_remote_input(&mut state, &mut fb);

        let chrome = fb.chrome();
        assert!(
            chrome
                .notifications()
                .iter()
                .any(|n| n.title == "Remote input stopped"),
            "a flood of app toasts buried the kill-switch confirmation",
        );
    }

    /// The scrubber reads evdev keycodes off the wire; local input reads
    /// winit logical keys. Both narrow to a [`ChordKey`], and a key that
    /// narrowed differently on the two sides would be a chord the shell
    /// grabs from its own user and hands to a guest.
    #[test]
    fn chord_key_agrees_across_both_directions() {
        let named = [
            NamedKey::Escape,
            NamedKey::Backspace,
            NamedKey::Tab,
            NamedKey::Enter,
            NamedKey::Control,
            NamedKey::Shift,
            NamedKey::Alt,
            NamedKey::Space,
            NamedKey::CapsLock,
            NamedKey::F1,
            NamedKey::F2,
            NamedKey::F3,
            NamedKey::F4,
            NamedKey::F5,
            NamedKey::F6,
            NamedKey::F7,
            NamedKey::F8,
            NamedKey::F9,
            NamedKey::F10,
            NamedKey::F11,
            NamedKey::F12,
            NamedKey::Home,
            NamedKey::ArrowUp,
            NamedKey::PageUp,
            NamedKey::ArrowLeft,
            NamedKey::ArrowRight,
            NamedKey::End,
            NamedKey::ArrowDown,
            NamedKey::PageDown,
            NamedKey::Insert,
            NamedKey::Delete,
            NamedKey::Super,
        ];
        let mut checked = 0;
        for n in named {
            let key = Key::Named(n);
            let code = evdev_from_logical(&key)
                .unwrap_or_else(|| panic!("{n:?} is in the forward keycode table"));
            assert_eq!(
                chord_key_for_evdev(code),
                chord_key_for_logical(&key),
                "{n:?} (evdev {code}) narrows differently from each side",
            );
            checked += 1;
        }
        // Every character the forward table maps, including the space
        // some layouts deliver as text rather than `NamedKey::Space`.
        const CHARS: &str = "abcdefghijklmnopqrstuvwxyz0123456789-=[];'`\\,./ ";
        for c in CHARS.chars() {
            let key = Key::Character(SmolStr::new(c.to_string()));
            let code = evdev_from_logical(&key)
                .unwrap_or_else(|| panic!("{c:?} is in the forward keycode table"));
            assert_eq!(
                chord_key_for_evdev(code),
                chord_key_for_logical(&key),
                "{c:?} (evdev {code}) narrows differently from each side",
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            named.len() + CHARS.chars().count(),
            "the round-trip sweep skipped keys",
        );
    }

    /// Function keys carry no chord on their own — the compositor hands
    /// `F2` to a focused client. It is the `Ctrl+Alt` combination that
    /// the kernel takes, which is why the scrubber names that pair
    /// itself rather than expecting the shell's table to cover it.
    #[test]
    fn function_keys_are_not_shell_chords_on_their_own() {
        assert!(!shell_owns_chord(
            ChordKey::Function(2),
            ChordMods::default(),
            false
        ));
        assert!(shell_owns_chord(
            ChordKey::Function(2),
            ChordMods {
                logo: true,
                ..Default::default()
            },
            false
        ));
    }

    #[test]
    fn client_input_registry_tracks_lifecycle_without_a_surface() {
        // The registry half that doesn't need a live `WlSurface`: pending
        // adoption / removal bookkeeping and the focus reset on destroy.
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
}
