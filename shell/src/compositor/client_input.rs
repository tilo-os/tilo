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
    /// Last known cursor position in global (output) coordinates.
    cursor: (f64, f64),
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
    pub fn cursor(&self) -> (f64, f64) {
        self.cursor
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
    let ws = tree.active_workspace()?;
    let p = KurboPoint::new(point.0, point.1);
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
            if rect.contains(p) {
                return Some(TileHit {
                    surface_id: *surface_id,
                    strip: strip.id,
                    group: group.id,
                    rect,
                });
            }
        }
    }
    None
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
/// the command palette is open, the workspace-name editor has a buffer in
/// flight, or a permission prompt is pending. In any of those states a
/// client must neither receive keys nor hold pointer focus.
pub fn chrome_captures_input(fb: &FrameBuilder) -> bool {
    if fb.host().palette_visible() {
        return true;
    }
    let chrome = fb.chrome();
    chrome.overlays.overview
        || chrome.overlays.quick_settings
        || chrome.overlays.ws_settings
        || chrome.overlays.collaborators
        || chrome.overlays.room_activity
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
        if self.client_input.pointer_focus.is_none() {
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
    let captured = chrome_captures_input(fb);
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            state.client_input.cursor = (position.x, position.y);
            let hit = current_pointer_target(state, fb, captured);
            deliver_pointer(state, hit);
            hit.is_some()
        }
        WindowEvent::CursorLeft { .. } => {
            let global = state.client_input.cursor;
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
            let to_client = matches!(state.client_input.grab, Some(PointerOwner::Client(_)))
                && state.client_input.pointer_focus.is_some();
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
            consumed
        }
        WindowEvent::MouseWheel { delta, .. } => {
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
            let overlay_open = fb.host().palette_visible();
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

/// Where the pointer should be focused right now: the foreign tile under
/// the cursor, or the implicit grab's owner while a button is held.
fn current_pointer_target(state: &State, fb: &FrameBuilder, captured: bool) -> Option<TileHit> {
    match state.client_input.grab {
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
            foreign_tile_at(&tree, fb.host(), state.client_input.cursor)
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
            foreign_tile_at(&tree, fb.host(), state.client_input.cursor)
                .filter(|h| state.client_input.surfaces.contains_key(&h.surface_id))
        }
    }
}

/// Push the current cursor position at `hit` (or clear client focus when
/// `None`), producing the enter / motion / leave sequence.
fn deliver_pointer(state: &mut State, hit: Option<TileHit>) {
    let global = state.client_input.cursor;
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
