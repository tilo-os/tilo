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

/// Linux `BTN_LEFT`. The Wayland pointer protocol carries raw evdev button codes, not an enum, so the winit `MouseButton` has to be mapped back.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// Scroll distance, in surface-local units, that one notch of a discrete wheel maps to — matches mainstream compositors so clients scaling `wl_pointer.axis` behave consistently.
const WHEEL_STEP: f64 = 15.0;

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
    /// Surface the pointer is currently inside (and therefore focused on at the seat), or `None` when the pointer is over chrome.
    pointer_focus: Option<u64>,
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
    /// evdev codes whose *press* was forwarded to a client. A release
    /// forwards iff its press did — a client never sees a release for a key it never saw pressed (a shell chord).
    forwarded_keys: HashSet<u32>,
    /// Accumulator for the Alt+Shift wheel group-jump gesture, so a touchpad's small pixel deltas fire one discrete jump per [`JUMP_PX_PER_DETENT`]. See [`WheelJumpAccum`].
    jump_wheel: WheelJumpAccum,
}

/// Resolve where an Alt+Shift wheel jump of `steps` windows lands. A
/// "window" is a tab: the walk flattens the active strip's groups into tabs
/// in strip order, clamped at the ends; pure, so tests can pin navigation against a bare [`WorkspaceTree`].
pub(crate) fn step_active_window(
    tree: &WorkspaceTree,
    steps: i32,
) -> Option<(StripId, GroupId, u64)> {
    let ws = tree.active_workspace()?;
    // Focused strip, else the first — the same fallback `place_tab` applies when a recorded pointer has gone stale.
    let strip = tree
        .active_strip
        .get(&ws.id)
        .copied()
        .and_then(|id| ws.strips.iter().find(|s| s.id == id))
        .or_else(|| ws.strips.first())?;
    let windows: Vec<(GroupId, u64)> = strip
        .window_groups
        .iter()
        .flat_map(|g| g.tabs.iter().map(|t| (g.id, t.id)))
        .collect();
    if windows.is_empty() {
        return None;
    }
    // Current position: the focused group's active tab (same first-group fallback used everywhere else).
    let cur_group = tree
        .active_group_for(strip.id)
        .and_then(|g| strip.window_groups.iter().find(|wg| wg.id == g))
        .or_else(|| strip.window_groups.first())?;
    let cur = windows
        .iter()
        .position(|&(g, t)| g == cur_group.id && t == cur_group.active_tab)
        .unwrap_or(0);
    // `windows.len()` is a handful of on-screen tabs, never near `i64::MAX` — the fallbacks below are unreachable in practice, just panic-free.
    let last = i64::try_from(windows.len())
        .unwrap_or(i64::MAX)
        .saturating_sub(1);
    let cur_i64 = i64::try_from(cur).unwrap_or(i64::MAX);
    let idx = usize::try_from(cur_i64.saturating_add(i64::from(steps)).clamp(0, last)).unwrap_or(0);
    if idx == cur {
        return None;
    }
    let (group, tab) = *windows.get(idx)?;
    Some((strip.id, group, tab))
}

impl ClientInput {
    /// Record a freshly-created foreign toplevel — called from `XdgShellHandler::new_toplevel`; the chrome-side tab is spliced in by [`sync_foreign_tabs`] next tick.
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

    /// Drop any queued adoption/removal/rename bookkeeping — the rendered
    /// tab tree is mirrored by an idempotent diff of the protocol tree
    /// (`foreign::reconcile_foreign_tabs`) that doesn't consume these queues, so the run loop drains them each tick to keep them bounded.
    pub fn clear_pending(&mut self) {
        self.pending_adoptions.clear();
        self.pending_removals.clear();
        self.pending_renames.clear();
    }

    /// Record a destroyed foreign toplevel — drops the surface handle immediately and queues the chrome-side tab removal.
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

    /// Keep a registered surface routable while removing every assumption
    /// that it belongs to a tiled window. Used only for the compositor-
    /// authorised on-screen keyboard after its stable app-id arrives.
    pub fn promote_to_overlay(&mut self, surface_id: u64) {
        self.pending_adoptions
            .retain(|pending| pending.surface_id != surface_id);
        self.pending_renames.retain(|(id, _)| *id != surface_id);
        self.pending_removals.retain(|id| *id != surface_id);
        self.placements.remove(&surface_id);
    }

    /// The surface the seat's keyboard is currently focused on, if any.
    pub fn keyboard_focus(&self) -> Option<u64> {
        self.keyboard_focus
    }

    /// The surface the seat's pointer is currently focused on, if any.
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

    /// Where the pointer is, or `None` if it has never been anywhere — the honest form of [`Self::cursor`], for the one caller that must tell the two apart.
    pub fn pointer_position(&self) -> Option<(f64, f64)> {
        self.cursor
    }
}

// Pure helpers — no seat, no widgets. Unit-testable in isolation.

/// Resolve the foreign surface hosted by the *focused* tab (active tab of
/// the active group/strip/workspace). `None` when shell-owned or empty — the keyboard-focus rule: input follows the active tab.
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

/// A foreign tile under the cursor: its surface id, the strip + group that own it, and its screen-space rect.
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
/// area, excluding the top bar, tab strip, sidebar wheel and inter-group gutters. Overlays painting *over* a tile are excluded via [`chrome_captures_input`].
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

/// Every foreign tile currently on screen, in front-to-back order — the
/// order [`foreign_tile_at`] resolves ties in. Exposed because remote
/// input must answer the same "what's on top here?" question from a
/// different thread; a second traversal risks disagreeing with where the click really lands (see [`crate::network::input_firewall`]).
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

/// Surface-local coordinates for a global `point` inside `rect`. Smithay does this itself when handed the focus origin, but it's exposed and asserted on separately since it's load-bearing.
pub fn surface_local(point: (f64, f64), rect: Rect) -> (f64, f64) {
    (point.0 - rect.x0, point.1 - rect.y0)
}

/// Bare modifier keys — never treated as compositor chords, since a client's xkb state has to see them or its own `Shift`/`Ctrl` handling desynchronises from the seat's.
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

/// Fold a bare modifier key press/release into `modifiers`, returning
/// `true` when the mask changed — winit reports modifiers via a separate
/// `ModifiersChanged` event, so a `KeyboardInput`-only backend would leave
/// the mask stale, making [`is_shell_chord`] miss `Ctrl+K`. No-op for backends that do emit `ModifiersChanged`.
pub fn apply_modifier_key(modifiers: &mut Modifiers, key: &KeyEvent) -> bool {
    use winit::keyboard::ModifiersState;
    let bit = match &key.logical_key {
        Key::Named(NamedKey::Shift) => ModifiersState::SHIFT,
        Key::Named(NamedKey::Control) => ModifiersState::CONTROL,
        Key::Named(NamedKey::Alt | NamedKey::AltGraph) => ModifiersState::ALT,
        Key::Named(NamedKey::Super | NamedKey::Meta) => ModifiersState::SUPER,
        _ => return false,
    };
    let mut mask = modifiers.state();
    if key.state == ElementState::Pressed {
        mask |= bit;
    } else {
        mask.remove(bit);
    }
    if mask == modifiers.state() {
        return false;
    }
    *modifiers = Modifiers::from(mask);
    true
}

/// A key reduced to the shape chord classification cares about.
///
/// Consulted from two directions: local input as a winit [`Key`], remote
/// input as a bare evdev keycode. Both narrow to this enum and ask [`shell_owns_chord`], so a shell chord and a guest's refusal can't drift apart.
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
    /// A bare modifier. Never a chord on its own — a client's xkb state has to see modifier transitions or it desynchronises from the seat.
    Modifier,
    /// Anything the chord table has no opinion about.
    Other,
}

/// The modifier mask a chord is evaluated against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this struct IS the flags grouping for the four modifier bits, each independently settable; fields are read directly from shell/src/network/input_firewall.rs, so their shape can't change in isolation here"
)]
pub struct ChordMods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

impl ChordMods {
    /// The mask carried by an xkbcommon-style bitfield — the encoding the peer-sync wire protocol uses for [`crate::network::streaming::InputEvent::Key`].
    pub fn from_xkb_bits(bits: u32) -> Self {
        Self {
            shift: bits & 0x1 != 0,
            ctrl: bits & 0x4 != 0,
            alt: bits & 0x8 != 0,
            logo: bits & 0x40 != 0,
        }
    }

    /// Union of two masks. Used where a mask is assembled from more than one source and any bit set in either has to count.
    #[must_use]
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

/// The shell's chord table, expressed once: every `Super` chord, `Ctrl+K`/
/// `Alt+Space`, `Ctrl+Tab`, `Ctrl+Arrow`, and `Escape` *only while an overlay is open* — an unconditional Escape grab would break every modal editor running as a client.
pub fn shell_owns_chord(key: ChordKey, mods: ChordMods, overlay_open: bool) -> bool {
    if key == ChordKey::Modifier {
        return false;
    }
    if mods.logo {
        return true;
    }
    match key {
        ChordKey::Space => mods.alt,
        ChordKey::Tab | ChordKey::ArrowUp | ChordKey::ArrowDown => mods.ctrl,
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
                // A layout that delivers the space bar as text rather than `NamedKey::Space` must still reach the `Alt+Space` arm.
                (Some(' '), None) => ChordKey::Space,
                (Some(c), None) => ChordKey::Char(c.to_ascii_lowercase()),
                _ => ChordKey::Other,
            }
        }
        _ => ChordKey::Other,
    }
}

/// Narrow an evdev keycode to its [`ChordKey`] — the inverse of
/// [`evdev_from_logical`]; `chord_key_agrees_across_both_directions` pins the two together so a key added to one is not missed by the other.
pub fn chord_key_for_evdev(code: u32) -> ChordKey {
    match code {
        1 => ChordKey::Escape,
        15 => ChordKey::Tab,
        // left/right Ctrl, left/right Shift, Alt/AltGr, CapsLock, NumLock, left/right Super
        29 | 97 | 42 | 54 | 56 | 100 | 58 | 69 | 125 | 126 => ChordKey::Modifier,
        57 => ChordKey::Space,
        59..=68 => ChordKey::Function(u8::try_from(code.saturating_sub(58)).unwrap_or(0)),
        87 => ChordKey::Function(11),
        88 => ChordKey::Function(12),
        103 => ChordKey::ArrowUp,
        108 => ChordKey::ArrowDown,
        111 => ChordKey::Delete,
        _ => match char_from_evdev(code) {
            Some(c) => ChordKey::Char(c),
            None => ChordKey::Other,
        },
    }
}

/// Is this key event a compositor-level binding that must be handled by the shell *before* a focused client ever sees it? Thin winit adapter over [`shell_owns_chord`].
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
/// Wayland/X11, exactly what `wl_keyboard.key` carries). Falls back to a
/// logical-key table for events synthesised without a physical key — the DRM backend's libinput bridge and the test-control socket both build `KeyEvent`s that way.
pub fn evdev_keycode(key: &KeyEvent) -> Option<u32> {
    // Only an identified physical key carries a trustworthy scancode: winit maps `PhysicalKey::Unidentified` to `KEY_UNKNOWN` (240), which would otherwise make every synthesised keystroke a dead key.
    if let PhysicalKey::Code(code) = key.physical_key
        && let Some(scancode) = code.to_scancode()
        && scancode != 0
    {
        return Some(scancode);
    }
    evdev_from_logical(&key.logical_key)
}

/// Logical-key → evdev keycode fallback. Values are from `linux/input-event-codes.h`.
pub fn evdev_from_logical(key: &Key) -> Option<u32> {
    match key {
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
    }
}

/// The unshifted key a character sits on, and whether Shift reaches it. A
/// keycode names a *position*, not a character: 39 is `;` or `:` purely by whether Shift is held (US positions).
pub(crate) fn unshifted(c: char) -> (char, bool) {
    let pair = match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        ':' => ';',
        '"' => '\'',
        '~' => '`',
        '|' => '\\',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => return (c.to_ascii_lowercase(), c.is_ascii_uppercase()),
    };
    (pair, true)
}

/// Single-character → evdev keycode (US layout positions) — answers about the *position*, so a shifted character resolves to the key its unshifted twin sits on (see [`unshifted`]).
fn evdev_from_char(c: char) -> Option<u32> {
    let (lower, _) = unshifted(c);
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

/// evdev keycode → the single character [`evdev_from_char`] would have mapped to it, in lowercase. `None` for codes that carry no character.
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

/// Translate a winit scroll delta into the `(value, v120)` pair `wl_pointer.axis`/`axis_value120` carry — winit's "up/left" positive vs Wayland's "down/right" means both components are negated.
pub fn axis_values(delta: MouseScrollDelta) -> ((f64, f64), Option<(i32, i32)>) {
    match delta {
        MouseScrollDelta::LineDelta(h, v) => (
            (-f64::from(h) * WHEEL_STEP, -f64::from(v) * WHEEL_STEP),
            Some((
                v120_component(-f64::from(h) * 120.0),
                v120_component(-f64::from(v) * 120.0),
            )),
        ),
        MouseScrollDelta::PixelDelta(p) => ((-p.x, -p.y), None),
    }
}

/// Round a `wl_pointer.axis_value120` component to `i32`, clamped to that
/// type's range first — a real wheel notch never gets near either bound, this just keeps the cast itself infallible.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to [i32::MIN, i32::MAX] and .round()-ed first, so the cast to i32 never \
              truncates a fraction or wraps out of range"
)]
fn v120_component(value: f64) -> i32 {
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Whether the shell chrome is currently claiming all input: an overlay,
/// the command palette, the peer-sync card, the workspace-name editor, or a permission prompt — in any of those a client must not receive keys or pointer focus.
pub fn chrome_captures_input(fb: &FrameBuilder) -> bool {
    // First-login chrome is a compositor-owned security boundary. It hides the
    // entire desktop and must mute both the local Wayland seat and the remote
    // input firewall, just like the session lock does.
    if fb.host().onboarding_active() {
        return true;
    }
    if fb.host().palette_visible() {
        return true;
    }
    // The portal's file chooser, for the same reason as the peer-sync card
    // below: modal, drawn over a scrim, but "drawn above" and "receives the click" are separate — without this it was unreachable with mouse or keyboard.
    if fb.host().file_picker().is_some() {
        return true;
    }
    let peer_sync_visible = fb.host().peer_sync_card_visible();
    let chrome = fb.chrome();
    chrome.overlays.overview
        || chrome.overlays.quick_settings
        || chrome.overlays.ws_settings
        || chrome.overlays.collaborators
        || chrome.overlays.room_activity
        // The peer-sync card, on the same two fields the overlay host
        // paints it from — its handler runs before every other overlay,
        // but this predicate decides whether an event reaches it. Without
        // it, a card raised over a focused client sent its Escape to the client, undismissable, with a live invite token exposed.
        || peer_sync_visible
        || chrome.editing_workspace_name.is_some()
        || !chrome.pending_grants.is_empty()
}

// Seat delivery — the actual `wl_keyboard`/`wl_pointer` traffic.

/// Mint a fresh input serial. Wayland clients ignore events whose serial goes backwards, so every delivered event gets its own.
fn mint_serial(state: &mut State) -> Serial {
    state.serial_counter = state.serial_counter.wrapping_add(1);
    Serial::from(state.serial_counter)
}

/// Milliseconds since compositor start — the timestamp base every input event on this seat shares.
fn event_time(state: &State) -> u32 {
    // `as_millis()` is `u128`; keep the low 32 bits exactly as `as u32` would (wrapping after ~49.7 days of uptime), just without the lint-flagged cast. `millis` is always in range, so `try_from` never actually falls back.
    let millis = state.start_time.elapsed().as_millis() % (1u128 << 32);
    u32::try_from(millis).unwrap_or(0)
}

impl State {
    /// Point the seat's keyboard at `surface_id` (or nothing when `None`), sending `leave`/`enter` and the keymap; idempotent, so the run loop can call it unconditionally every tick.
    pub fn set_client_keyboard_focus(&mut self, surface_id: Option<u64>) {
        if self.client_input.keyboard_focus == surface_id {
            return;
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
        keyboard.set_focus(self, target.map(|(_, s)| s), serial);
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
        forward && has_focus && intercepted.is_none()
    }

    /// Move the seat pointer to `global` and focus it on `target` (`(surface
    /// id, tile origin)`) — smithay derives `enter`/`leave`/`motion` from the focus transition; passing `None` is how the pointer *leaves* a client.
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
        self.client_input.pointer_focus = focus.as_ref().and_then(|_| target.map(|(id, _)| id));
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

    /// Forward a pointer button press/release to the surface the pointer is focused on — no-op when the pointer is over chrome.
    pub fn forward_client_pointer_button(&mut self, button: u32, pressed: bool) -> bool {
        // A chrome drag's grab needs the release even with no client under the pointer — that is how it learns the drag was cancelled.
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

    /// Forward one touch point to the focused client over `wl_touch`.
    /// `target` is `(surface id, tile origin)` for a `Down` landing inside a client tile; touch focus is set on `down` only (per protocol).
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

/// Which client (if any) a touch point should be delivered to — only a `Down` establishes touch focus; the rest of the sequence belongs to whatever the down landed on.
pub fn touch_target(
    state: &State,
    fb: &FrameBuilder,
    touch: &BackendTouchEvent,
) -> Option<(u64, (f64, f64))> {
    let BackendTouchEvent::Down { x, y, .. } = touch else {
        return None;
    };
    if let Some((surface_id, rect)) = fb.osk_surface_hit((*x, *y))
        && state.osk_surface == Some(surface_id)
        && state.client_input.surfaces.contains_key(&surface_id)
    {
        return Some((surface_id, (rect.x0, rect.y0)));
    }
    if let Some((surface_id, rect)) = fb.floating_surface_hit((*x, *y))
        && state.client_input.surfaces.contains_key(&surface_id)
    {
        return Some((surface_id, (rect.x0, rect.y0)));
    }
    let hit = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        foreign_tile_at(&tree, fb.host(), (*x, *y))?
    };
    if !state.client_input.surfaces.contains_key(&hit.surface_id) {
        return None;
    }
    Some((hit.surface_id, (hit.rect.x0, hit.rect.y0)))
}

// Run-loop entry points.

/// Mirror foreign toplevels adopted at the protocol layer into the shell
/// chrome's workspace tree, and reap the tabs of destroyed ones; `true` when changed. Each becomes its own window group in the active strip.
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

/// Splice a foreign toplevel into the active workspace as its own window group, active in its strip so keyboard focus follows it. Returns `(strip, group)`, or `None` when the workspace has no strip.
fn insert_foreign_group(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<&str>,
) -> Option<(StripId, GroupId)> {
    let active_ws = tree.active;
    let preferred_strip = tree.active_strip.get(&active_ws).copied();
    let ws = tree.workspaces.iter_mut().find(|w| w.id == active_ws)?;
    // Index first, then borrow once — `or_else` over two `&mut` borrows of the same vec doesn't type-check.
    let idx = preferred_strip
        .and_then(|sid| ws.strips.iter().position(|s| s.id == sid))
        .unwrap_or(0);
    let strip = ws.strips.get_mut(idx)?;
    let resolved = app_id.map_or_else(
        || format!("anon-{surface_id}"),
        std::string::ToString::to_string,
    );
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

/// Drop the tab bound to `surface_id`, and the group that hosted it when that leaves it empty — also advances the strip's active-group pointer off a group that just disappeared.
fn remove_foreign_tab(tree: &mut WorkspaceTree, surface_id: u64) -> bool {
    let mut vacated: Option<(StripId, GroupId)> = None;
    for ws in &mut tree.workspaces {
        for strip in &mut ws.strips {
            let mut hit_group: Option<GroupId> = None;
            for group in &mut strip.window_groups {
                let Some(pos) = group.tabs.iter().position(|t| {
                    matches!(&t.tile, TileContent::Foreign { surface_id: s, .. } if *s == surface_id)
                }) else {
                    continue;
                };
                let removed = group.tabs.remove(pos);
                if group.active_tab == removed.id {
                    group.active_tab = group.tabs.last().map_or(0, |t| t.id);
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

/// Rewrite the app-id + title of a mirrored foreign tab after a late `xdg_toplevel.set_app_id`.
fn rename_foreign_tab(tree: &mut WorkspaceTree, surface_id: u64, new_app_id: &str) -> bool {
    for ws in &mut tree.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    if let TileContent::Foreign {
                        surface_id: s,
                        app_id,
                    } = &mut tab.tile
                        && *s == surface_id
                    {
                        *app_id = new_app_id.to_string();
                        tab.title = foreign_tab_title(new_app_id);
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Tab-bar-friendly label for a wayland app-id (`org.example.MyApp` → `MyApp`), capped so a pathological id can't blow out the tab strip.
fn foreign_tab_title(app_id: &str) -> String {
    let trimmed = app_id.split('.').next_back().unwrap_or(app_id);
    if trimmed.len() <= 32 {
        return trimmed.to_string();
    }
    // A client's app-id is untrusted Unicode; walk back from the byte cap to
    // the nearest character boundary so a multi-byte character straddling it can't panic a slice.
    let mut end = 32;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    trimmed.get(..end).unwrap_or(trimmed).to_string()
}

/// Reconcile the seat's keyboard focus with the shell's focused tab. Called every tick: chrome capture drops client focus outright; otherwise focus follows the active tab.
pub fn sync_keyboard_focus(state: &mut State, fb: &FrameBuilder) {
    let captured = chrome_captures_input(fb);
    let candidate = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        active_foreign_surface(&tree).filter(|id| state.client_input.surfaces.contains_key(id))
    };
    state.set_client_keyboard_focus(gate_keyboard_focus(candidate, captured));
}

fn gate_keyboard_focus(candidate: Option<u64>, chrome_captures: bool) -> Option<u64> {
    if chrome_captures { None } else { candidate }
}

/// Tell the remote-input firewall what is on screen. Called every tick,
/// immediately after [`sync_keyboard_focus`], so a remote event is judged against what the local user is looking at.
///
/// The tile list comes from [`foreign_tiles`], the same enumeration
/// [`foreign_tile_at`] resolves local clicks with. `session_locked` must be
/// passed in since the lock surface isn't part of the tiled tree — without it a guest's click would pass through to a password-protected window.
pub fn publish_remote_input_view(state: &mut State, fb: &FrameBuilder, session_locked: bool) {
    let tiles: Vec<(u64, Rect)> = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        let mut tiles = foreign_tiles(&tree, fb.host())
            .into_iter()
            // A tab whose surface the compositor no longer holds is not a place input can land, for the local user or a remote one.
            .filter(|hit| state.client_input.surfaces.contains_key(&hit.surface_id))
            .map(|hit| (hit.surface_id, hit.rect))
            .collect::<Vec<_>>();
        drop(tree);
        drop(chrome);
        tiles.extend(
            fb.floating_surface_rects()
                .into_iter()
                .filter(|(surface_id, _)| state.client_input.surfaces.contains_key(surface_id)),
        );
        tiles
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
/// Action half of the `Super+Esc` kill switch, split from the backend loop
/// (a `-> !` function) so it can be driven from a test. Returns how many
/// shares lost write access. The confirmation toast bypasses Do-Not-Disturb ([`Notification::bypassing_dnd`]) and is persistent.
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
    state
        .remote_input_firewall
        .note_local_input(std::time::Instant::now());
    let captured = chrome_captures_input(fb);
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            route_cursor_moved(state, fb, captured, *position)
        }
        WindowEvent::CursorLeft { .. } => route_cursor_left(state),
        WindowEvent::MouseInput {
            state: element,
            button,
            ..
        } => route_mouse_input(state, fb, captured, *element, *button),
        WindowEvent::MouseWheel { delta, .. } => route_mouse_wheel(state, fb, *delta, *modifiers),
        WindowEvent::KeyboardInput { .. } => {
            route_keyboard_input(state, fb, captured, event, *modifiers)
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
    fb.host_mut()
        .note_pointer(vello::kurbo::Point::new(position.x, position.y));
    let hit = current_pointer_target(state, fb, captured);
    deliver_pointer(state, hit);
    // During a chrome drag the file manager keeps drawing the ghost under the cursor, so it sees every move even over a client.
    hit.is_some() && state.selection.chrome_drag.is_none()
}

/// `WindowEvent::CursorLeft` half of [`route_input`]: the pointer left the
/// output entirely, so any client focus and implicit grab go with it.
fn route_cursor_left(state: &mut State) -> bool {
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
            deliver_pointer(state, hit);
            state.client_input.grab = Some(match hit {
                Some(h) => PointerOwner::Client(h.surface_id),
                None => PointerOwner::Chrome,
            });
            // Click-to-focus: raise the clicked window group so the keyboard follows the pointer into the client.
            if let Some(h) = hit {
                // Tapping the keyboard must keep the editable field's
                // keyboard focus. Pointer focus moves to the OSK, but
                // raising/focusing it would deactivate the very
                // text-input session the tap is meant to feed.
                if state.osk_surface != Some(h.surface_id)
                    && !state.is_floating_toplevel(h.surface_id)
                {
                    raise_group(fb, h.strip, h.group);
                    sync_keyboard_focus(state, fb);
                }
            }
        }
        state.client_input.buttons_down = state.client_input.buttons_down.saturating_add(1);
    } else {
        state.client_input.buttons_down = state.client_input.buttons_down.saturating_sub(1);
    }
    // A chrome drag's release is the drop: the server-side grab smithay holds only ends on a button event, whether or not a client is under the pointer.
    let chrome_drag = state.selection.chrome_drag.is_some();
    let to_client = (matches!(state.client_input.grab, Some(PointerOwner::Client(_)))
        && state.client_input.pointer_focus.is_some())
        || (chrome_drag && !pressed);
    let consumed = if to_client {
        match evdev_button(button) {
            Some(code) => state.forward_client_pointer_button(code, pressed),
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
        state.forward_client_pointer_axis(delta)
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
    let Some(code) = evdev_keycode(key) else {
        return false;
    };
    // Anything the shell has up that Escape would dismiss — the summoned workspace wheel counts, since Escape must reach the shell, not a focused client.
    let overlay_open = fb.host().palette_visible() || fb.host().wheel_summoned();
    let shell_owns = captured || is_shell_chord(event, &modifiers, overlay_open);
    let forward = if pressed {
        let forward = !shell_owns && state.client_input.keyboard_focus.is_some();
        if forward {
            state.client_input.forwarded_keys.insert(code);
        }
        forward
    } else {
        // A release mirrors its press exactly — see `ClientInput::forwarded_keys`.
        state.client_input.forwarded_keys.remove(&code)
    };
    // Runs for shell-owned keys too: the seat's xkb state has to track every key, or its modifier view drifts (see `deliver_client_key`).
    state.deliver_client_key(code, pressed, forward)
}

/// Promote a file drag from the hosted file manager into a Wayland drag.
/// Called every tick: once the chrome's in-process drag gesture has the
/// pointer over a foreign tile, the compositor claims it, starts smithay's
/// server-side drag offering the file, and delivers the enter. From then on `current_pointer_target` hit-tests normally so the client under the pointer receives `wl_data_device` enter/motion/drop; the release is the drop — Files does not act on the release itself.
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

/// Where the pointer should be focused right now: the foreign tile under the cursor, or the implicit grab's owner while a button is held.
fn current_pointer_target(state: &State, fb: &FrameBuilder, captured: bool) -> Option<TileHit> {
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

/// Make `group` the active group of `strip` in the chrome's tree so the shell's focus rules agree with what the user just clicked.
fn raise_group(fb: &mut FrameBuilder, strip: StripId, group: GroupId) {
    let mut changed = false;
    fb.with_chrome(|chrome| {
        {
            let mut tree = chrome.tree_mut();
            changed = tree.active_group_for(strip) != Some(group);
            if changed {
                tree.set_active_group(strip, group);
            }
        }
        if changed {
            chrome.bump();
        }
    });
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic pointer-coordinate fixtures use exactly representable values"
)]
mod tests {
    use super::*;
    use crate::shell::state::{AppKind, Strip, Workspace, WorkspaceId};
    use std::time::Duration;
    use winit::event::{DeviceId, KeyEvent};
    use winit::keyboard::{KeyLocation, ModifiersState, PhysicalKey, SmolStr};

    /// Every printable ASCII character resolves to a key on the board — asserted over the whole range since a missing shifted-punctuation entry is exactly the gap a hand-picked list would miss.
    #[test]
    fn every_printable_ascii_character_names_a_key() {
        let missing: Vec<char> = (0x20u8..0x7f)
            .map(char::from)
            .filter(|c| evdev_from_char(*c).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "these characters reach a client as nothing at all: {missing:?}",
        );
    }

    /// A shifted character names the key its unshifted twin sits on.
    #[test]
    fn a_shifted_character_shares_its_key_with_the_unshifted_one() {
        for (shifted, plain) in [
            (':', ';'),
            ('"', '\''),
            ('?', '/'),
            ('_', '-'),
            ('+', '='),
            ('{', '['),
            ('}', ']'),
            ('<', ','),
            ('>', '.'),
            ('~', '`'),
            ('|', '\\'),
            ('!', '1'),
            (')', '0'),
        ] {
            assert_eq!(
                evdev_from_char(shifted),
                evdev_from_char(plain),
                "{shifted:?} and {plain:?} are one key",
            );
            assert_eq!(
                unshifted(shifted),
                (plain, true),
                "{shifted:?} is {plain:?} with Shift held",
            );
        }
        assert_eq!(unshifted('a'), ('a', false), "a lowercase letter is bare");
        assert_eq!(unshifted('A'), ('a', true), "a capital is Shift held");
    }

    fn key_event(logical: Key, pressed: bool) -> WindowEvent {
        // Mirrors the synthesis helpers already used by the test-control executor: winit's `KeyEvent` has a private platform field, so the struct is zero-initialised.
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

    /// A workspace tree with one strip carrying two groups: a shell-owned stub and a foreign client on `surface_id`.
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
        // Active group defaults to the first (shell-owned) group — no client should hold the keyboard.
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
        // ONE group holding several windows as tabs — one step right of the first is the second.
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
        assert_eq!(
            step_active_window(&tree, 1),
            Some((strip_id, group_id, ids[2]))
        );
        tree.workspaces[0].strips[0].window_groups[0].active_tab = ids[2];
        assert_eq!(step_active_window(&tree, 1), None, "right edge clamps");
        assert_eq!(step_active_window(&tree, 5), None, "overshoot clamps too");
        // And back left, two at once, lands on Notes; the left edge clamps.
        assert_eq!(
            step_active_window(&tree, -2),
            Some((strip_id, group_id, ids[0]))
        );
        tree.workspaces[0].strips[0].window_groups[0].active_tab = ids[0];
        assert_eq!(step_active_window(&tree, -1), None, "left edge clamps");
    }

    #[test]
    fn step_active_window_continues_into_the_next_group() {
        let (mut tree, strip, foreign_group) = tree_with_foreign(1);
        // No explicit active pointer yet: the walk starts at the first group's tab; one step right crosses into the foreign group.
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
        // Winit: positive y = wheel up. Niri: WheelScrollDown → focus-column-right — so wheel-down must be a positive detent.
        assert_eq!(
            wheel_jump_detents(&MouseScrollDelta::LineDelta(0.0, -1.0)),
            1.0
        );
        assert_eq!(
            wheel_jump_detents(&MouseScrollDelta::LineDelta(0.0, 1.0)),
            -1.0
        );
        // Touchpad pixels are fractional: one detent per JUMP_PX_PER_DETENT.
        assert_eq!(
            wheel_jump_detents(&MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                0.0, -25.0
            ))),
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
        // SLOW deliberate scrolling still gets there: fractional detents separated by long pauses must keep accumulating.
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
        // A point outside the rect still translates linearly — the caller is responsible for hit-testing first.
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
        // Ctrl chords the shell does NOT own stay with the client (Ctrl+C in a terminal must not be eaten).
        assert!(!is_shell_chord(
            &key_event(Key::Character(SmolStr::new("c")), true),
            &ctrl,
            false
        ));
        // A bare modifier is always forwarded so the client's xkb state tracks the seat's, even while Super is held.
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
        // Values from linux/input-event-codes.h — the codes `wl_keyboard.key` carries on the wire.
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
        // `KeyA` is evdev 30; the logical key deliberately disagrees so a fallback to the table would produce a different value.
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

        // Draining the last foreign group leaves the shell-owned group active, so no client holds the keyboard.
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

    /// The peer-sync card must take the keyboard away from a focused client, or
    /// it cannot be dismissed — a key only reaches its handler when `chrome_captures_input` says the chrome has taken input.
    #[test]
    fn the_peer_sync_card_takes_input_from_a_focused_client() {
        for field in ["invite_token", "connect_input"] {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            assert!(
                !chrome_captures_input(&fb),
                "nothing is up yet, so a client should still own the keyboard"
            );

            let snapshot = match field {
                "invite_token" => crate::shell::overlay_host::PeerSyncSnapshot {
                    invite_token: Some("a-signed-blob".into()),
                    ..Default::default()
                },
                _ => crate::shell::overlay_host::PeerSyncSnapshot {
                    connect_input: Some(String::new()),
                    ..Default::default()
                },
            };
            fb.host_mut().set_peer_sync_snapshot_for_tests(snapshot);
            let _ = fb.paint_settled();

            assert!(
                chrome_captures_input(&fb),
                "with peer_sync_{field} set the card is painted, so the chrome \
                 must hold the keyboard — otherwise Escape goes to the client \
                 and the card can never be dismissed"
            );
        }
    }

    /// The portal's file chooser is modal, so it must hold input while it is
    /// up — the same contract as the peer-sync card above. Asserted on
    /// `chrome_captures_input` with the dialog mounted the way the compositor
    /// mounts one (through the portal's queue, drained by `pump_file_picker`), so this can't pass against a dialog production would never raise.
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
        let prompts =
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoAllow);
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

    #[test]
    fn onboarding_mutes_wayland_focus_and_the_remote_input_view() {
        const HIDDEN_SURFACE: u64 = 424_242;
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        fb.host_mut()
            .enable_onboarding(std::path::PathBuf::from("/tmp/qos-oobe-input-test"));
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

    /// The geometry contract the pointer router depends on: a foreign
    /// toplevel mirrored into the chrome's tree becomes a live window group
    /// whose *tile* rect is laid out, hit-testable, and translates a global
    /// cursor into surface-local coordinates. Runs against the real `FrameBuilder`, so a regression in the tree→widget resync or `ShellOverlayHost::active_tile_rect` fails here.
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

        // One settle pass runs the tree→row resync and the layout that gives the freshly-spliced group its rect.
        let _ = fb.paint_settled();

        let rect = fb
            .host()
            .active_tile_rect(group)
            .expect("the mirrored group must exist in the live strip row");
        assert!(
            rect.width() > 1.0 && rect.height() > 1.0,
            "the client's tile must have a real extent; got {rect:?}"
        );

        let centre = (rect.center().x, rect.center().y);
        let hit = {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            foreign_tile_at(&tree, fb.host(), centre)
                .expect("a cursor inside the tile resolves to the client")
        };
        assert_eq!(hit.surface_id, 4242);
        assert_eq!(hit.group, group);
        assert_eq!(hit.rect, rect);

        // Surface-local translation is the tile origin subtracted.
        assert_eq!(
            surface_local(centre, hit.rect),
            (centre.0 - rect.x0, centre.1 - rect.y0)
        );

        // Chrome keeps priority: a point on the top bar, and a point on the tile's own tab bar, are not the client's.
        let chrome = fb.chrome();
        let tree = chrome.tree();
        assert!(
            foreign_tile_at(&tree, fb.host(), (800.0, 4.0)).is_none(),
            "the top bar must never resolve to a client surface"
        );
        assert!(
            foreign_tile_at(&tree, fb.host(), (centre.0, rect.y0 - 4.0)).is_none(),
            "the tab strip above the tile must never resolve to a client surface"
        );
    }

    /// The remote-input firewall's z-order check and local pointer routing must never disagree about what is on top — the disagreement *is* the clickjacking hole; they share [`foreign_tiles`].
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "tree is read for every probe in the loop below; the block already ends \
                  right after the loop, so there's no earlier point to drop the guard"
    )]
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

    /// A tab is a place input can land only while the compositor still holds
    /// the client's surface — one left in the tree after its toplevel went
    /// away is a rectangle nothing is behind, and publishing it would hand a
    /// guest a coordinate the local user can't even click on. Drives the real publisher against the real laid-out tree, then asks the firewall the question a peer's event asks.
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

    /// The lock surface is drawn by the session, not the tiled tree, so tiles
    /// beneath it stay laid out and topmost as far as the tree knows — the publisher must carry the lock across, or a guest keeps driving a password-protected window.
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

    /// A session nobody has pointed at holds no pointer position, and holds
    /// one the instant a real motion is routed through. The two accessors
    /// disagree by design: `cursor()` answers the origin (hit-testing needs a
    /// point), `pointer_position` answers `None` — publishing the origin would announce a cursor for somebody who isn't there.
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

        // The one thing that gives a session a pointer, through the function every source of motion goes through.
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

    /// Routing a motion tells the chrome where the pointer went, whether or
    /// not the chrome is the one that gets to read the event — the chrome
    /// hit-tests a press against the last position it was given, since
    /// `MouseInput` carries none of its own. Motion over a client window is withheld from the chrome's own dispatch, so without this its pointer idea would freeze at its last chrome-side position.
    #[test]
    fn routing_a_motion_keeps_the_chrome_pointer_current() {
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
                device_id: unsafe { std::mem::zeroed() },
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
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(880.0, 300.0),
            },
            &mods,
        );
        assert_eq!(
            fb.host().pointer_position(),
            Some(vello::kurbo::Point::new(880.0, 300.0)),
        );
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

    /// The panic key, end to end from the action the chord triggers: a guest is driving the seat, the host presses it, the guest stops, and the host is told so.
    #[test]
    fn stopping_remote_input_halts_the_guest_and_says_so_on_screen() {
        use crate::network::input_firewall::Denial;

        let mut fb = FrameBuilder::new((1600, 900));
        let mut state = state_sharing(KILL_SURFACE);

        guest_motion(&mut state).expect("the guest is driving the seat to begin with");

        let revoked = stop_remote_input(&mut state, &mut fb);
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

    /// Do-Not-Disturb is a request for quiet, not to be kept uninformed about the machine's own security state — a suppressed confirmation makes a working kill switch look broken.
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

    /// The notification stack is capped, and a guest driving a client can make that client post toasts — filling the stack must not hide that the panic key worked.
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

    /// The scrubber reads evdev keycodes off the wire; local input reads winit logical keys — both narrow to a [`ChordKey`], so the two can't drift apart.
    #[test]
    fn chord_key_agrees_across_both_directions() {
        // Every character the forward table maps, including the space some layouts deliver as text rather than `NamedKey::Space`.
        const CHARS: &str = "abcdefghijklmnopqrstuvwxyz0123456789-=[];'`\\,./ ";
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

    /// Function keys carry no chord on their own — the compositor hands `F2` to a focused client; it's the `Ctrl+Alt` combination the kernel takes.
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
}
