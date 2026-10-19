//! Shared shell navigation, chord ownership, geometry and peer wire input encoding.
use crate::shell::frame::FrameBuilder;
use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::{GroupId, StripId, TileContent, WorkspaceTree};
use vello::kurbo::{Point as KurboPoint, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
const WHEEL_STEP: f64 = ui::tokens::Layout::BASE.wheel_step as f64;
/// Resolve the foreign surface hosted by the *focused* tab (active tab of
/// the active group/strip/workspace). `None` when shell-owned or empty — the keyboard-focus rule: input follows the active tab.
pub fn active_foreign_surface(tree: &WorkspaceTree) -> Option<u64> {
    let ws = tree.desktop.active_workspace()?;
    let strip = match tree.desktop.active_strip.get(&ws.id) {
        Some(sid) => ws
            .strips
            .iter()
            .find(|s| s.id == *sid)
            .or_else(|| ws.strips.first())?,
        None => ws.strips.first()?,
    };
    let group = match tree.desktop.active_group_for(strip.id) {
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
        // A window showing its card takes no keys: its app cannot.
        TileContent::Foreign { surface_id, .. }
            if !crate::shell::foreign::shows_window_card(*surface_id) =>
        {
            Some(*surface_id)
        }
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
    if host.dictation_pointer_at(point) {
        return None;
    }
    let p = KurboPoint::new(point.0, point.1);
    find_foreign_tile(tree, host, |hit| {
        hit.rect.contains(p)
            && !host.group_resize_hit(hit.group, point)
            // The card over a not-responding or unshowable window is the
            // shell's: the pointer goes to it, not to the app.
            && !crate::shell::foreign::shows_window_card(hit.surface_id)
    })
}

/// The live laid-out rect for one foreign surface in the active workspace.
/// Used by per-tick pointer-constraint sync without building the full tile list.
pub fn foreign_tile_for_surface(
    tree: &WorkspaceTree,
    host: &ShellOverlayHost,
    surface_id: u64,
) -> Option<TileHit> {
    find_foreign_tile(tree, host, |hit| hit.surface_id == surface_id)
}

/// Every foreign tile currently on screen, in front-to-back order — the
/// order [`foreign_tile_at`] resolves ties in. Exposed because remote
/// input must answer the same "what's on top here?" question from a
/// different thread; a second traversal risks disagreeing with where the click really lands (see [`crate::network::input_firewall`]).
pub fn foreign_tiles(tree: &WorkspaceTree, host: &ShellOverlayHost) -> Vec<TileHit> {
    let mut out = Vec::new();
    for_each_foreign_tile(tree, host, |hit| {
        out.push(hit);
        true
    });
    out
}

/// Walk foreign tiles in the same front-to-back order as [`foreign_tiles`].
/// `visit` returns `false` to stop early.
fn for_each_foreign_tile(
    tree: &WorkspaceTree,
    host: &ShellOverlayHost,
    mut visit: impl FnMut(TileHit) -> bool,
) {
    let Some(ws) = tree.desktop.active_workspace() else {
        return;
    };
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
            let hit = TileHit {
                surface_id: *surface_id,
                strip: strip.id,
                group: group.id,
                rect,
            };
            if !visit(hit) {
                return;
            }
        }
    }
}

fn find_foreign_tile(
    tree: &WorkspaceTree,
    host: &ShellOverlayHost,
    mut pred: impl FnMut(&TileHit) -> bool,
) -> Option<TileHit> {
    let mut found = None;
    for_each_foreign_tile(tree, host, |hit| {
        if pred(&hit) {
            found = Some(hit);
            false
        } else {
            true
        }
    });
    found
}

/// Surface-local coordinates for a global `point` inside `rect`. Smithay does this itself when handed the focus origin, but it's exposed and asserted on separately since it's load-bearing.
pub fn surface_local(point: (f64, f64), rect: Rect) -> (f64, f64) {
    (point.0 - rect.x0, point.1 - rect.y0)
}

/// Bare modifier keys — never treated as compositor chords, since a client's xkb state has to see them or its own `Shift`/`Ctrl` handling desynchronises from the seat's.
pub fn is_modifier_key(key: &Key) -> bool {
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
    PageUp,
    PageDown,
    PrintScreen,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Escape,
    Delete,
    Enter,
    /// `F1`..`F12`, numbered from 1.
    Function(u8),
    /// Num Lock. A lock key rather than a held modifier, so unlike Shift it
    /// can carry a chord (Shift+Alt+Num Lock toggles mouse keys); on its own
    /// it always reaches the focused client.
    NumLock,
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
    reason = "this struct IS the flags grouping for the four modifier bits, each independently settable; fields are read directly from crates/session/shell/src/network/input_firewall.rs, so their shape can't change in isolation here"
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

/// True immutable shell chords only. Configurable navigation/search bindings
/// are resolved from `ShortcutConfig` by the host before client delivery; do
/// not add their defaults here or rebinding would leave the old app chord
/// blocked forever.
///
/// Pure over its inputs and never touches settings storage: it runs for
/// nearly every local and remote keystroke.
pub fn shell_owns_chord(key: ChordKey, mods: ChordMods, overlay_open: bool) -> bool {
    if key == ChordKey::Modifier {
        return false;
    }
    // The fixed chords (Alt+Tab, Alt+F4, Super+Page Up/Down, F1, ...) are
    // withheld before a focused client sees them. They live in one table
    // shared with the shortcut guide, the portal and the keyboard map.
    if crate::shell::fixed_chords::fixed_action(key, mods).is_some() {
        return true;
    }
    // The optional Vim navigation layer (Super+Ctrl+H/J/K/L) needs no
    // separate check: every Super+Ctrl chord reaches the `mods.logo` arm
    // below, so ownership does not depend on whether the layer is enabled.
    // These are defaults for editable actions.  They intentionally fall
    // through when the effective configuration no longer assigns them.
    if matches!(
        key,
        ChordKey::Tab
            | ChordKey::Space
            | ChordKey::ArrowLeft
            | ChordKey::ArrowRight
            | ChordKey::ArrowUp
            | ChordKey::ArrowDown
            | ChordKey::Char(
                'k' | 'l' | 'q' | '/' | '-' | '=' | '1'..='9' | 'e' | 'v' | 'a' | 'f' | '.'
            )
    ) && ((mods.logo && !mods.ctrl && !mods.alt)
        || (matches!(key, ChordKey::Tab) && (mods.ctrl || mods.alt)))
    {
        return false;
    }
    if mods.logo {
        return true;
    }
    match key {
        // Print Screen and the F1 guide are fixed chords in their bare form;
        // under Ctrl/Alt/Shift they stay shell-owned too, so a modifier held
        // by accident cannot hand them to a focused client. Other function
        // keys remain application-owned.
        ChordKey::PrintScreen | ChordKey::Function(1) => true,
        ChordKey::Escape => overlay_open,
        // Alt+Space has no desktop action; reserving it here silently ate
        // application input before native widgets and Wayland/X11 clients.
        ChordKey::Space
        | ChordKey::Tab
        | ChordKey::PageUp
        | ChordKey::PageDown
        | ChordKey::ArrowLeft
        | ChordKey::ArrowRight
        | ChordKey::ArrowUp
        | ChordKey::ArrowDown
        | ChordKey::Char(_)
        | ChordKey::Delete
        | ChordKey::Enter
        | ChordKey::Function(_)
        | ChordKey::NumLock
        | ChordKey::Other
        | ChordKey::Modifier => false,
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
        Key::Named(NamedKey::PageUp) => ChordKey::PageUp,
        Key::Named(NamedKey::PageDown) => ChordKey::PageDown,
        Key::Named(NamedKey::PrintScreen) => ChordKey::PrintScreen,
        Key::Named(NamedKey::ArrowLeft) => ChordKey::ArrowLeft,
        Key::Named(NamedKey::ArrowRight) => ChordKey::ArrowRight,
        Key::Named(NamedKey::ArrowUp) => ChordKey::ArrowUp,
        Key::Named(NamedKey::ArrowDown) => ChordKey::ArrowDown,
        Key::Named(NamedKey::Escape) => ChordKey::Escape,
        Key::Named(NamedKey::Delete) => ChordKey::Delete,
        Key::Named(NamedKey::Enter) => ChordKey::Enter,
        Key::Named(NamedKey::NumLock) => ChordKey::NumLock,
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
        104 => ChordKey::PageUp,
        109 => ChordKey::PageDown,
        99 => ChordKey::PrintScreen,
        // left/right Ctrl, left/right Shift, Alt/AltGr, CapsLock, left/right Super
        29 | 97 | 42 | 54 | 56 | 100 | 58 | 125 | 126 => ChordKey::Modifier,
        69 => ChordKey::NumLock,
        57 => ChordKey::Space,
        59..=68 => ChordKey::Function(u8::try_from(code.saturating_sub(58)).unwrap_or(0)),
        87 => ChordKey::Function(11),
        88 => ChordKey::Function(12),
        105 => ChordKey::ArrowLeft,
        106 => ChordKey::ArrowRight,
        103 => ChordKey::ArrowUp,
        108 => ChordKey::ArrowDown,
        111 => ChordKey::Delete,
        28 => ChordKey::Enter,
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
/// Prefers the physical key position mapped to the stable peer wire code. Falls back to a
/// logical-key table for events synthesised without a physical key — the DRM backend's libinput bridge and the test-control socket both build `KeyEvent`s that way.
pub fn evdev_keycode(key: &KeyEvent) -> Option<u32> {
    // Only an identified physical key carries a trustworthy scancode: winit maps `PhysicalKey::Unidentified` to `KEY_UNKNOWN` (240), which would otherwise make every synthesised keystroke a dead key.
    if let Some(scancode) = ui::evdev::from_physical(key.physical_key)
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
            NamedKey::NumLock => Some(69),
            NamedKey::F11 => Some(87),
            NamedKey::F12 => Some(88),
            NamedKey::PrintScreen => Some(99),
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
pub fn unshifted(c: char) -> (char, bool) {
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
        // winit has already normalised both Linux side-button pairs. Emit the
        // conventional BTN_SIDE/BTN_EXTRA pair used by ordinary mice and
        // accepted by Wayland browsers/Electron clients.
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
    // Keep or revert a display tool's change: Enter and Escape answer it
    // whichever window had focus.
    if fb.host().display_confirmation_visible() {
        return true;
    }
    // The emoji picker types into its own search field; the field it will
    // insert into gets the keyboard back when it closes.
    if fb.host().emoji_picker_open() {
        return true;
    }
    // The portal's file chooser, for the same reason as the peer-sync card
    // below: modal, drawn over a scrim, but "drawn above" and "receives the click" are separate — without this it was unreachable with mouse or keyboard.
    if fb.host().file_picker().is_some() {
        return true;
    }
    // Permission and other portal sheets are the same kind of modal. Without
    // this, a key or click lands in the window behind the sheet.
    if fb.host().portal_dialog_open() {
        return true;
    }
    // Alt is held after Alt+Tab. The focused client gives up the keyboard
    // for as long as the switcher is open, so Tab, the arrows and Escape
    // reach it, and the client never sees Alt pressed and released alone
    // (which toolkits read as "show the menu bar").
    if fb.host().window_switcher_active() {
        return true;
    }
    // The strip switcher opened by Super+S, or naming a strip: arrows,
    // Enter, typed letters and Escape are its.
    if fb.host().strip_switcher_takes_keys() {
        return true;
    }
    let peer_sync_visible = fb.host().peer_sync_card_visible();
    let inner_surface_captures = fb.host().inner_surface_captures_input();
    let chrome = fb.chrome();
    chrome.overlays.overview
        || chrome.overlays.quick_settings
        || chrome.overlays.ws_settings
        || chrome.overlays.collaborators
        || chrome.overlays.room_activity
        || chrome.overlays.projection
        || chrome.overlays.shortcut_help
        // The peer-sync card, on the same two fields the overlay host
        // paints it from — its handler runs before every other overlay,
        // but this predicate decides whether an event reaches it. Without
        // it, a card raised over a focused client sent its Escape to the client, undismissable, with a live invite token exposed.
        || peer_sync_visible
        || inner_surface_captures
        || chrome.editing_workspace_name.is_some()
        || !chrome.pending_grants.is_empty()
}

/// Whether the shell takes the pointer: whenever it takes all input, and
/// also while the pointer is on the bar a maximized window shows over its
/// client, or on a notification banner over a window. That bar takes only the pointer — the client keeps the keyboard,
/// so typing never stops because the pointer rests near the window's top.
pub fn chrome_captures_pointer(fb: &FrameBuilder) -> bool {
    chrome_captures_input(fb)
        || fb.host().maximized_chrome_captures_pointer()
        || fb.host().strip_switcher_captures_pointer()
        || fb.host().top_bar_captures_pointer()
        || fb.host().notification_banner_captures_pointer()
}

// Seat delivery — the actual `wl_keyboard`/`wl_pointer` traffic.

/// Splice a foreign window's group into the active strip and focus it, for
/// tests that need a client tile without driving the Wayland protocol.
#[cfg(test)]
pub(crate) fn insert_foreign_group(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<&str>,
) -> Option<(StripId, GroupId)> {
    use crate::shell::state::{Tab, WindowGroup, WindowWidth, next_id};
    let active_ws = tree.desktop.active;
    let preferred_strip = tree.desktop.active_strip.get(&active_ws).copied();
    let ws = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|w| w.id == active_ws)?;
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
        focus_history: Vec::new(),
    };
    let group_id = group.id;
    let strip_id = strip.id;
    strip.window_groups.push(group);
    tree.desktop.set_active_group(strip_id, group_id);
    Some((strip_id, group_id))
}

#[cfg(test)]
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

pub fn retain_live_overlay_focus(
    current: Option<u64>,
    floating: Option<u64>,
    transient: Option<u64>,
    fullscreen: Option<u64>,
) -> Option<u64> {
    let current = current?;
    (floating == Some(current) || transient == Some(current) || fullscreen == Some(current))
        .then_some(current)
}

pub fn gate_keyboard_focus(candidate: Option<u64>, chrome_captures: bool) -> Option<u64> {
    if chrome_captures { None } else { candidate }
}

pub fn track_forwarded_key<S: std::hash::BuildHasher>(
    forwarded: &mut std::collections::HashSet<u32, S>,
    code: u32,
    pressed: bool,
    press_can_forward: bool,
) -> bool {
    if pressed {
        if press_can_forward {
            forwarded.insert(code);
        }
        press_can_forward
    } else {
        forwarded.remove(&code)
    }
}

/// How long Super must be held, alone, before the Super surface starts to
/// grow in as a peek. A tap (press and release) is well under this; a
/// person reaching for a chord usually presses its second key sooner, and
/// one who holds Super this long without one is holding it to look.
pub const SUPER_HOLD: std::time::Duration = std::time::Duration::from_millis(300);

/// How long Super must be held, alone, before the peek grows from its quiet
/// first stage (the widgets' glance forms and the time, nothing over the
/// windows) into the whole surface (the search field, the scrim, every
/// widget open). A person who held Super a moment too long while looking
/// for a chord's second key (Super+Shift+Left, Super+]) sees at most the
/// quiet stage, and the chord takes that away without a fade.
pub const SUPER_FULL: std::time::Duration = std::time::Duration::from_millis(600);

/// How far the pointer moves (physical px) while Super is held before the
/// peek counts as being used and grows into the whole surface at once.
/// More than a hand resting on a touchpad drifts.
pub const SUPER_PEEK_MOTION_PX: f64 = ui::tokens::Metrics::BASE.icon_label_gap as f64;

/// What Super alone asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuperGesture {
    /// Pressed and released with nothing in between, before
    /// [`SUPER_HOLD`]: open the Super surface and keep it.
    Tap,
    /// Held alone for [`SUPER_HOLD`]: the peek's quiet first stage, the
    /// widgets' glance forms and the time.
    Peek,
    /// Held alone for [`SUPER_FULL`], or the pointer moved while peeking:
    /// the peek grows into the whole surface.
    PeekFull,
    /// The held Super came up: close the peek unless it was taken up.
    PeekReleased,
    /// A pointer press while peeking: the person is using the surface, so
    /// it stays when Super comes up.
    PeekTaken,
    /// Another key while peeking: the hold was a chord after all; close the
    /// peek at once, without a fade, and let the chord run.
    PeekCancelled,
}

/// Super alone, tapped or held: the one surface with search, widgets, the
/// time, status and people's lines.
///
/// Fed every input event ahead of chord routing, so a Super chord the
/// shell handles (Super+L, Super+Left) disarms it exactly like one a
/// client receives, and a Super-click or Super-scroll does too. Time comes
/// from the caller: [`Self::observe`] and [`Self::poll`] take the instant
/// they reason about, and [`Self::deadline`] tells the loop when a held
/// key turns into a peek or grows, so nothing polls.
#[derive(Debug, Default)]
pub struct SuperTap {
    /// When the lone Super went down, while nothing else has happened.
    armed: Option<std::time::Instant>,
    /// The hold became a peek.
    peeking: bool,
    /// The peek grew into the whole surface.
    full: bool,
    /// Where the pointer first was during the peek's quiet stage.
    peek_origin: Option<(f64, f64)>,
}

impl SuperTap {
    /// What `event` at `now` completes, if anything.
    pub fn observe(
        &mut self,
        event: &WindowEvent,
        now: std::time::Instant,
    ) -> Option<SuperGesture> {
        match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                let is_super = matches!(
                    key.logical_key,
                    Key::Named(NamedKey::Super | NamedKey::Meta)
                );
                match (is_super, key.state) {
                    (true, ElementState::Pressed) => {
                        // Autorepeat of the held key keeps the tap alive;
                        // a fresh press starts one.
                        if !key.repeat {
                            self.armed = Some(now);
                            self.end_peek();
                        }
                        None
                    }
                    (true, ElementState::Released) => {
                        let armed = self.armed.take();
                        if self.end_peek() {
                            return Some(SuperGesture::PeekReleased);
                        }
                        armed
                            .is_some_and(|at| now.saturating_duration_since(at) < SUPER_HOLD)
                            .then_some(SuperGesture::Tap)
                    }
                    (false, ElementState::Pressed) => {
                        self.armed = None;
                        self.end_peek().then_some(SuperGesture::PeekCancelled)
                    }
                    (false, ElementState::Released) => None,
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
            | WindowEvent::Touch(_) => {
                self.armed = None;
                self.end_peek().then_some(SuperGesture::PeekTaken)
            }
            WindowEvent::MouseWheel { .. } => {
                self.armed = None;
                None
            }
            WindowEvent::CursorMoved { position, .. } if self.peeking && !self.full => {
                let p = (position.x, position.y);
                let origin = *self.peek_origin.get_or_insert(p);
                let moved = (p.0 - origin.0).hypot(p.1 - origin.1);
                (moved >= SUPER_PEEK_MOTION_PX).then(|| self.grow())
            }
            _ => None,
        }
    }

    /// Forget the peek; whether there was one.
    fn end_peek(&mut self) -> bool {
        self.full = false;
        self.peek_origin = None;
        std::mem::take(&mut self.peeking)
    }

    fn grow(&mut self) -> SuperGesture {
        self.full = true;
        self.peek_origin = None;
        SuperGesture::PeekFull
    }

    /// The peek, once Super has been held alone for [`SUPER_HOLD`] at `now`,
    /// and its growth into the whole surface at [`SUPER_FULL`].
    pub fn poll(&mut self, now: std::time::Instant) -> Option<SuperGesture> {
        let at = self.armed?;
        let held = now.saturating_duration_since(at);
        if !self.peeking {
            if held < SUPER_HOLD {
                return None;
            }
            self.peeking = true;
            return Some(SuperGesture::Peek);
        }
        (!self.full && held >= SUPER_FULL).then(|| self.grow())
    }

    /// When a held Super next turns into a peek, or the peek grows.
    pub fn deadline(&self) -> Option<std::time::Instant> {
        let at = self.armed?;
        if !self.peeking {
            at.checked_add(SUPER_HOLD)
        } else if !self.full {
            at.checked_add(SUPER_FULL)
        } else {
            None
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test timelines add milliseconds to one monotonic instant"
)]
mod super_tap_tests {
    use super::*;
    use std::time::{Duration, Instant};
    use winit::keyboard::KeyLocation;

    fn key(logical: Key, state: ElementState) -> WindowEvent {
        let mut ev = ui::synthetic_input::key_event(logical, None, state);
        ev.location = KeyLocation::Left;
        ui::synthetic_input::keyboard_input(ev)
    }

    fn super_key(state: ElementState) -> WindowEvent {
        key(Key::Named(NamedKey::Super), state)
    }

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn a_lone_super_tap_fires_on_release() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        assert_eq!(tap.observe(&super_key(ElementState::Pressed), t0), None);
        assert_eq!(tap.poll(ms(t0, 90)), None);
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 110)),
            Some(SuperGesture::Tap)
        );
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 120)),
            None,
            "one tap, one surface"
        );
    }

    #[test]
    fn holding_super_alone_peeks_and_release_ends_the_peek() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.observe(&super_key(ElementState::Pressed), t0);
        assert_eq!(
            tap.deadline(),
            Some(t0 + SUPER_HOLD),
            "the loop wakes for the hold"
        );
        assert_eq!(
            tap.poll(
                (t0 + SUPER_HOLD)
                    .checked_sub(Duration::from_millis(1))
                    .unwrap()
            ),
            None
        );
        assert_eq!(tap.poll(t0 + SUPER_HOLD), Some(SuperGesture::Peek));
        assert_eq!(
            tap.deadline(),
            Some(t0 + SUPER_FULL),
            "the loop wakes again for the growth, not frame by frame"
        );
        assert_eq!(tap.poll(t0 + SUPER_HOLD), None, "one peek per hold");
        assert_eq!(tap.poll(t0 + SUPER_FULL), Some(SuperGesture::PeekFull));
        assert_eq!(tap.poll(ms(t0, 900)), None, "it grows once");
        assert_eq!(tap.deadline(), None);
        // Autorepeat while held changes nothing.
        let mut repeat = ui::synthetic_input::key_event(
            Key::Named(NamedKey::Super),
            None,
            ElementState::Pressed,
        );
        repeat.repeat = true;
        assert_eq!(
            tap.observe(&ui::synthetic_input::keyboard_input(repeat), ms(t0, 950)),
            None
        );
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 1200)),
            Some(SuperGesture::PeekReleased)
        );
    }

    /// Holding Super a moment too long before a chord's second key: the
    /// quiet stage shows, the chord cancels it, and the whole surface never
    /// comes.
    #[test]
    fn a_chord_after_a_long_hold_cancels_the_quiet_stage_before_it_grows() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.observe(&super_key(ElementState::Pressed), t0);
        assert_eq!(tap.poll(ms(t0, 420)), Some(SuperGesture::Peek));
        assert_eq!(
            tap.observe(
                &key(Key::Named(NamedKey::ArrowLeft), ElementState::Pressed),
                ms(t0, 480)
            ),
            Some(SuperGesture::PeekCancelled)
        );
        assert_eq!(tap.poll(t0 + SUPER_FULL), None, "no growth after a chord");
        assert_eq!(tap.deadline(), None);
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 700)),
            None
        );
    }

    #[test]
    fn moving_the_pointer_while_peeking_grows_the_surface_at_once() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        let moved = |x: f64| ui::synthetic_input::cursor_moved(x, 100.0);
        tap.observe(&super_key(ElementState::Pressed), t0);
        assert_eq!(
            tap.observe(&moved(10.0), ms(t0, 100)),
            None,
            "before the peek, motion is nothing"
        );
        tap.poll(t0 + SUPER_HOLD);
        assert_eq!(tap.observe(&moved(10.0), ms(t0, 320)), None);
        assert_eq!(
            tap.observe(&moved(13.0), ms(t0, 330)),
            None,
            "a resting hand's drift"
        );
        assert_eq!(
            tap.observe(&moved(30.0), ms(t0, 340)),
            Some(SuperGesture::PeekFull)
        );
        assert_eq!(tap.observe(&moved(60.0), ms(t0, 350)), None, "once");
        assert_eq!(tap.poll(t0 + SUPER_FULL), None);
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 900)),
            Some(SuperGesture::PeekReleased)
        );
    }

    #[test]
    fn a_long_press_never_counts_as_a_tap_even_unpolled() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.observe(&super_key(ElementState::Pressed), t0);
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 2000)),
            None
        );
    }

    #[test]
    fn a_click_while_peeking_takes_the_surface_and_a_chord_cancels_it() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.observe(&super_key(ElementState::Pressed), t0);
        tap.poll(t0 + SUPER_HOLD);
        assert_eq!(
            tap.observe(&ui::synthetic_input::mouse_press(), ms(t0, 500)),
            Some(SuperGesture::PeekTaken)
        );
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 700)),
            None
        );

        tap.observe(&super_key(ElementState::Pressed), ms(t0, 1000));
        tap.poll(ms(t0, 1000) + SUPER_HOLD);
        assert_eq!(
            tap.observe(
                &key(Key::Character("l".into()), ElementState::Pressed),
                ms(t0, 1500)
            ),
            Some(SuperGesture::PeekCancelled)
        );
        assert_eq!(
            tap.observe(&super_key(ElementState::Released), ms(t0, 1600)),
            None
        );
    }

    #[test]
    fn any_key_in_between_makes_it_a_chord_not_a_tap() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.observe(&super_key(ElementState::Pressed), t0);
        tap.observe(&key(Key::Character("l".into()), ElementState::Pressed), t0);
        tap.observe(&key(Key::Character("l".into()), ElementState::Released), t0);
        assert_eq!(tap.observe(&super_key(ElementState::Released), t0), None);
        assert_eq!(tap.poll(ms(t0, 1000)), None, "a chord never peeks");

        tap.observe(&key(Key::Named(NamedKey::Shift), ElementState::Pressed), t0);
        tap.observe(&super_key(ElementState::Pressed), t0);
        tap.observe(&key(Key::Named(NamedKey::Tab), ElementState::Pressed), t0);
        assert_eq!(tap.observe(&super_key(ElementState::Released), t0), None);
    }

    #[test]
    fn a_super_click_or_scroll_is_not_a_tap() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.observe(&super_key(ElementState::Pressed), t0);
        tap.observe(&ui::synthetic_input::mouse_press(), t0);
        assert_eq!(tap.observe(&super_key(ElementState::Released), t0), None);

        tap.observe(&super_key(ElementState::Pressed), t0);
        tap.observe(&ui::synthetic_input::mouse_wheel(0.0, 1.0), t0);
        assert_eq!(tap.observe(&super_key(ElementState::Released), t0), None);
    }

    #[test]
    fn a_modifier_released_alone_is_not_super() {
        let t0 = Instant::now();
        let mut tap = SuperTap::default();
        tap.observe(&key(Key::Named(NamedKey::Alt), ElementState::Pressed), t0);
        assert_eq!(
            tap.observe(&key(Key::Named(NamedKey::Alt), ElementState::Released), t0),
            None
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;
    use crate::shell::state::{
        AppKind, Strip, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceId, next_id,
    };
    use std::time::{Duration, Instant};
    use ui::widget::{WheelJumpAccum, wheel_jump_detents};
    use winit::keyboard::{KeyLocation, ModifiersState, PhysicalKey, SmolStr};
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
        ui::synthetic_input::keyboard_input(ev)
    }
    fn mods(state: ModifiersState) -> Modifiers {
        Modifiers::from(state)
    }
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
            focus_history: Vec::new(),
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
            focus_history: Vec::new(),
        };
        let foreign_group = foreign.id;
        let strip_id = StripId::from(next_id());
        let mut strip = Strip::new(strip_id, "main");
        strip.window_groups = vec![stub, foreign];
        let ws_id = WorkspaceId::from(next_id());
        let mut ws = Workspace::new(ws_id, "Home");
        ws.strips = vec![strip];
        let mut tree = WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                ..Default::default()
            },
            ..Default::default()
        };
        tree.desktop.active_strip.insert(ws_id, strip_id);
        (tree, strip_id, foreign_group)
    }
    #[test]
    fn sole_foreign_window_right_edge_belongs_to_compositor_resize() {
        use ui::Widget;
        let mut fb = FrameBuilder::with_tree(WorkspaceTree::bootstrap_default(), (1280, 800));
        let mut placed = None;
        fb.with_chrome(|chrome| {
            placed =
                insert_foreign_group(&mut chrome.tree_mut(), 991_001, Some("org.example.resize"));
            chrome.bump();
        });
        let (strip, group) = placed.expect("foreign group");
        let _ = fb.paint_settled();
        let rect = fb
            .host_mut()
            .group_at_for_tests(strip, 0)
            .expect("group")
            .get_rect();
        let point = (rect.x1 - 2.0, rect.center().y);
        assert!(
            fb.host_mut()
                .group_at_for_tests(strip, 0)
                .expect("group")
                .resize_grab_hit(KurboPoint::new(point.0, point.1)),
            "fixture grabs painted grip"
        );
        let chrome = fb.chrome();
        let tree = chrome.tree();
        assert_eq!(tree.desktop.active_group_for(strip), Some(group));
        assert_eq!(
            tree.desktop.active_workspace().expect("workspace").strips[0]
                .window_groups
                .len(),
            1
        );
        assert!(
            foreign_tile_at(&tree, fb.host(), point).is_none(),
            "the sole window's painted resize grip must reach chrome, not its client"
        );
        let content = fb.host().active_tile_rect(group).expect("content");
        assert_eq!(
            foreign_tile_at(&tree, fb.host(), (content.center().x, content.center().y))
                .expect("client interior")
                .surface_id,
            991_001
        );
    }

    #[test]
    fn scrolled_foreign_window_grip_uses_screen_coordinates() {
        use ui::Widget;
        let mut fb = FrameBuilder::with_tree(WorkspaceTree::bootstrap_default(), (1472, 800));
        let mut placed = None;
        fb.with_chrome(|chrome| {
            placed =
                insert_foreign_group(&mut chrome.tree_mut(), 991_002, Some("org.example.resize"));
            insert_foreign_group(
                &mut chrome.tree_mut(),
                991_003,
                Some("org.example.neighbor"),
            );
            chrome.bump();
        });
        let (strip, group) = placed.expect("first group");
        let _ = fb.paint_settled();
        let initial = fb
            .host_mut()
            .group_at_for_tests(strip, 0)
            .expect("group")
            .get_rect();
        let viewport = fb.host().group_row_rect_for_tests(strip).expect("viewport");
        assert!(fb.host_mut().update_active_strip_pan(
            initial.x0 - viewport.x0 + 80.0,
            true,
            std::time::Instant::now()
        ));
        let _ = fb.paint_settled();
        let rect = fb
            .host_mut()
            .group_at_for_tests(strip, 0)
            .expect("group")
            .get_rect();
        let viewport = fb.host().group_row_rect_for_tests(strip).expect("viewport");
        assert!(
            rect.x0 < viewport.x0 && rect.x1 > viewport.x0 + 10.0,
            "scrolled group {rect:?} in {viewport:?}"
        );
        let point = (rect.x1 - 2.0, rect.center().y);
        let chrome = fb.chrome();
        let tree = chrome.tree();
        assert!(fb.host().group_resize_hit(group, point));
        assert!(foreign_tile_at(&tree, fb.host(), point).is_none());
        let inside = (rect.x1 - 20.0, rect.center().y);
        assert_eq!(
            foreign_tile_at(&tree, fb.host(), inside)
                .expect("client")
                .surface_id,
            991_002
        );
    }

    #[test]
    fn maximized_foreign_window_retains_client_edge_input() {
        let mut fb = FrameBuilder::with_tree(WorkspaceTree::bootstrap_default(), (1280, 800));
        let mut placed = None;
        fb.with_chrome(|chrome| {
            placed = insert_foreign_group(
                &mut chrome.tree_mut(),
                991_004,
                Some("org.example.maximized"),
            );
            let mut tree = chrome.tree_mut();
            tree.desktop.workspaces[0].strips[0].window_groups[0].maximized = true;
            drop(tree);
            chrome.bump();
        });
        let (_, group) = placed.expect("group");
        let _ = fb.paint_settled();
        let rect = fb.host().active_tile_rect(group).expect("client rect");
        let point = (rect.x1 - 2.0, rect.center().y);
        let chrome = fb.chrome();
        let tree = chrome.tree();
        assert!(!fb.host().group_resize_hit(group, point));
        assert_eq!(
            foreign_tile_at(&tree, fb.host(), point)
                .expect("client edge")
                .surface_id,
            991_004
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn keys_buttons_and_scroll_map_to_stable_wire_codes() {
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

        fn evdev_keycode_prefers_the_physical_scancode() {
            let mut ev = ui::synthetic_input::key_event(
                winit::keyboard::Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
                None,
                winit::event::ElementState::Pressed,
            );
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

        fn non_us_physical_keys_keep_the_peer_wire_code() {
            let WindowEvent::KeyboardInput { mut event, .. } =
                key_event(Key::Character(SmolStr::new("a")), true)
            else {
                unreachable!()
            };
            event.physical_key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ);
            assert_eq!(evdev_keycode(&event), Some(16));
            event.physical_key = PhysicalKey::Code(winit::keyboard::KeyCode::IntlBackslash);
            event.logical_key = Key::Character(SmolStr::new("<"));
            assert_eq!(evdev_keycode(&event), Some(86));
        }

        fn mouse_buttons_map_to_evdev_codes() {
            assert_eq!(evdev_button(MouseButton::Left), Some(0x110));
            assert_eq!(evdev_button(MouseButton::Right), Some(0x111));
            assert_eq!(evdev_button(MouseButton::Middle), Some(0x112));
            assert_eq!(evdev_button(MouseButton::Forward), Some(0x114));
            assert_eq!(evdev_button(MouseButton::Back), Some(0x113));
            assert_eq!(evdev_button(MouseButton::Other(9)), None);
        }

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
                NamedKey::NumLock,
                NamedKey::F11,
                NamedKey::F12,
                NamedKey::PrintScreen,
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

        every_printable_ascii_character_names_a_key();
        a_shifted_character_shares_its_key_with_the_unshifted_one();
        evdev_keycodes_match_the_kernel_table();
        evdev_keycode_prefers_the_physical_scancode();
        non_us_physical_keys_keep_the_peer_wire_code();
        mouse_buttons_map_to_evdev_codes();
        scroll_deltas_invert_for_the_wayland_axis_convention();
        chord_key_agrees_across_both_directions();
    }
    #[test]
    fn active_foreign_surface_and_overlay_focus_follow_the_live_tree() {
        fn active_foreign_surface_follows_the_active_group() {
            let (mut tree, strip, foreign_group) = tree_with_foreign(4242);
            // Active group defaults to the first (shell-owned) group — no client should hold the keyboard.
            assert_eq!(
                active_foreign_surface(&tree),
                None,
                "a shell-owned active tab must not hand the keyboard to a client"
            );
            // Switching the active group onto the foreign one moves focus.
            tree.desktop.set_active_group(strip, foreign_group);
            assert_eq!(active_foreign_surface(&tree), Some(4242));
        }

        fn active_foreign_surface_ignores_inactive_workspaces() {
            let (mut tree, strip, foreign_group) = tree_with_foreign(7);
            tree.desktop.set_active_group(strip, foreign_group);
            assert_eq!(active_foreign_surface(&tree), Some(7));
            // Point the tree at a workspace that doesn't exist: nothing to focus.
            tree.desktop.active = crate::shell::state::WorkspaceId::from(u64::MAX);
            assert_eq!(
                active_foreign_surface(&tree),
                None,
                "focus must not leak across workspaces"
            );
        }

        fn mapped_overlay_focus_survives_reconciliation_for_every_overlay_kind() {
            for (name, id, floating, transient, fullscreen) in [
                ("floating", 11, Some(11), None, None),
                ("transient", 22, None, Some(22), None),
                ("fullscreen", 33, None, None, Some(33)),
            ] {
                assert_eq!(
                    retain_live_overlay_focus(Some(id), floating, transient, fullscreen),
                    Some(id),
                    "a mapped {name} surface must retain keyboard focus across a tick"
                );
            }
        }

        fn dismissed_overlay_focus_is_released_to_the_tree_fallback() {
            assert_eq!(
                retain_live_overlay_focus(Some(41), None, None, None),
                None,
                "a surface that disappeared from every overlay layer must not retain stale focus"
            );
            assert_eq!(
                retain_live_overlay_focus(Some(41), Some(99), Some(99), Some(99)),
                None,
                "only the currently focused overlay id may be preserved"
            );
        }

        fn surface_local_subtracts_the_tile_origin() {
            let rect = Rect::new(320.0, 96.0, 1220.0, 800.0);
            assert_eq!(surface_local((320.0, 96.0), rect), (0.0, 0.0));
            assert_eq!(surface_local((420.0, 196.0), rect), (100.0, 100.0));
            // A point outside the rect still translates linearly — the caller is responsible for hit-testing first.
            assert_eq!(surface_local((300.0, 96.0), rect), (-20.0, 0.0));
        }

        active_foreign_surface_follows_the_active_group();
        active_foreign_surface_ignores_inactive_workspaces();
        mapped_overlay_focus_survives_reconciliation_for_every_overlay_kind();
        dismissed_overlay_focus_is_released_to_the_tree_fallback();
        surface_local_subtracts_the_tile_origin();
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn window_stepping_and_wheel_jumps_walk_the_strip() {
        fn spatial_window_step_clamps_at_the_strip_ends() {
            let (mut tree, strip, foreign_group) = tree_with_foreign(4242);
            assert_eq!(step_active_window(&tree, -1), None);
            tree.desktop.set_active_group(strip, foreign_group);
            assert_eq!(step_active_window(&tree, 1), None);
        }

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
                focus_history: Vec::new(),
            };
            let group_id = group.id;
            let strip_id = StripId::from(next_id());
            let mut strip = Strip::new(strip_id, "main");
            strip.window_groups = vec![group];
            let ws_id = WorkspaceId::from(next_id());
            let mut ws = Workspace::new(ws_id, "Home");
            ws.strips = vec![strip];
            let mut tree = WorkspaceTree {
                desktop: crate::shell::state::DesktopTree {
                    workspaces: vec![ws],
                    active: ws_id,
                    ..Default::default()
                },
                ..Default::default()
            };
            tree.desktop.active_strip.insert(ws_id, strip_id);

            // Notes is active → one step right is Files.
            assert_eq!(
                step_active_window(&tree, 1),
                Some((strip_id, group_id, ids[1])),
                "one step right of the first tab must be the second"
            );
            // Apply it, step again → Terminal; then the right edge clamps.
            tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab = ids[1];
            assert_eq!(
                step_active_window(&tree, 1),
                Some((strip_id, group_id, ids[2]))
            );
            tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab = ids[2];
            assert_eq!(step_active_window(&tree, 1), None, "right edge clamps");
            assert_eq!(step_active_window(&tree, 5), None, "overshoot clamps too");
            // And back left, two at once, lands on Notes; the left edge clamps.
            assert_eq!(
                step_active_window(&tree, -2),
                Some((strip_id, group_id, ids[0]))
            );
            tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab = ids[0];
            assert_eq!(step_active_window(&tree, -1), None, "left edge clamps");
        }

        fn step_active_window_continues_into_the_next_group() {
            let (mut tree, strip, foreign_group) = tree_with_foreign(1);
            // No explicit active pointer yet: the walk starts at the first group's tab; one step right crosses into the foreign group.
            let (s, g, _t) = step_active_window(&tree, 1).expect("step into the next group");
            assert_eq!((s, g), (strip, foreign_group));
            tree.desktop.set_active_group(strip, foreign_group);
            // At the strip's last window the jump clamps.
            assert_eq!(step_active_window(&tree, 1), None);
            // Stepping back crosses into the first group again.
            let (s2, first, _t) = step_active_window(&tree, -1).expect("step back left");
            assert_eq!(s2, strip);
            assert_ne!(first, foreign_group);
        }

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

        spatial_window_step_clamps_at_the_strip_ends();
        step_active_window_walks_tabs_within_a_group();
        step_active_window_continues_into_the_next_group();
        wheel_jump_detents_map_wheel_down_to_a_rightward_jump();
        wheel_jump_accum_fires_whole_detents_and_survives_slow_scrolling();
    }
    #[test]
    fn vim_navigation_chords_are_shell_owned_without_reading_settings() {
        let super_ctrl = ChordMods {
            logo: true,
            ctrl: true,
            ..Default::default()
        };
        for key in ['h', 'j', 'k', 'l'] {
            assert!(
                shell_owns_chord(ChordKey::Char(key), super_ctrl, false),
                "Super+Ctrl+{key} must be withheld from a focused client"
            );
        }
        let ctrl = ChordMods {
            ctrl: true,
            ..Default::default()
        };
        assert!(
            !shell_owns_chord(ChordKey::Char('h'), ctrl, false),
            "Ctrl+H stays an application text chord"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn shell_chords_are_owned_before_clients_and_plain_keys_are_forwarded() {
        fn only_immutable_super_chords_are_static_client_firewall_rules() {
            let m = mods(ModifiersState::SUPER);
            for key in [
                Key::Character(SmolStr::new("w")),
                Key::Named(NamedKey::Enter),
            ] {
                let ev = key_event(key.clone(), true);
                assert!(
                    is_shell_chord(&ev, &m, false),
                    "Super+{key:?} must be handled by the shell"
                );
            }
            for key in [
                Key::Character(SmolStr::new("k")),
                Key::Character(SmolStr::new("l")),
                Key::Named(NamedKey::Space),
                Key::Named(NamedKey::Tab),
                Key::Named(NamedKey::ArrowUp),
                Key::Named(NamedKey::ArrowDown),
            ] {
                assert!(
                    !is_shell_chord(&key_event(key.clone(), true), &m, false),
                    "Super+{key:?} is configurable and must be resolved by the effective table"
                );
            }
        }

        fn configurable_ctrl_defaults_are_not_static_but_alt_tab_is_the_ring_alias() {
            let ctrl = mods(ModifiersState::CONTROL);
            let alt = mods(ModifiersState::ALT);
            assert!(!is_shell_chord(
                &key_event(Key::Character(SmolStr::new("k")), true),
                &ctrl,
                false
            ));
            assert!(!is_shell_chord(
                &key_event(Key::Named(NamedKey::Tab), true),
                &ctrl,
                false
            ));
            assert!(
                is_shell_chord(&key_event(Key::Named(NamedKey::Tab), true), &alt, false),
                "Alt+Tab is the immutable conventional window-ring alias"
            );
            assert!(!is_shell_chord(
                &key_event(Key::Named(NamedKey::ArrowDown), true),
                &ctrl,
                false
            ));
            assert!(
                !is_shell_chord(&key_event(Key::Named(NamedKey::Space), true), &alt, false),
                "Alt+Space has no desktop action and must remain application input"
            );
            assert!(is_shell_chord(
                &key_event(Key::Named(NamedKey::PrintScreen), true),
                &mods(ModifiersState::empty()),
                false
            ));
        }

        fn conventional_close_and_workspace_aliases_are_owned_before_clients() {
            let alt = mods(ModifiersState::ALT);
            let super_only = mods(ModifiersState::SUPER);
            assert!(is_shell_chord(
                &key_event(Key::Named(NamedKey::F4), true),
                &alt,
                false
            ));
            for key in [NamedKey::PageUp, NamedKey::PageDown] {
                assert!(is_shell_chord(
                    &key_event(Key::Named(key), true),
                    &super_only,
                    false
                ));
            }
            assert!(!is_shell_chord(
                &key_event(Key::Named(NamedKey::F4), true),
                &mods(ModifiersState::empty()),
                false
            ));
            assert!(!is_shell_chord(
                &key_event(Key::Named(NamedKey::PageDown), true),
                &mods(ModifiersState::CONTROL),
                false
            ));
        }

        fn plain_keys_and_bare_modifiers_are_forwarded() {
            let none = mods(ModifiersState::empty());
            let ctrl = mods(ModifiersState::CONTROL);
            // Unmodified typing goes to the client.
            assert!(!is_shell_chord(
                &key_event(Key::Character(SmolStr::new("a")), true),
                &none,
                false
            ));
            assert!(!is_shell_chord(
                &key_event(Key::Named(NamedKey::Tab), true),
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

        fn only_f1_is_a_shell_chord_on_its_own() {
            assert!(shell_owns_chord(
                ChordKey::Function(1),
                ChordMods::default(),
                false
            ));
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

        fn conventional_alt_tab_ring_is_owned_before_a_focused_client() {
            assert!(shell_owns_chord(
                ChordKey::Tab,
                ChordMods {
                    alt: true,
                    ..Default::default()
                },
                false
            ));
            assert!(shell_owns_chord(
                ChordKey::Tab,
                ChordMods {
                    shift: true,
                    alt: true,
                    ..Default::default()
                },
                false
            ));
        }

        fn a_shell_owned_press_never_produces_an_unmatched_client_release() {
            let mut forwarded = std::collections::HashSet::new();
            assert!(!track_forwarded_key(&mut forwarded, 34, true, false));
            assert!(!track_forwarded_key(&mut forwarded, 34, false, true));

            assert!(track_forwarded_key(&mut forwarded, 35, true, true));
            assert!(track_forwarded_key(&mut forwarded, 35, false, false));
            assert!(forwarded.is_empty());
        }

        only_immutable_super_chords_are_static_client_firewall_rules();
        configurable_ctrl_defaults_are_not_static_but_alt_tab_is_the_ring_alias();
        conventional_close_and_workspace_aliases_are_owned_before_clients();
        plain_keys_and_bare_modifiers_are_forwarded();
        escape_only_belongs_to_the_shell_while_an_overlay_is_open();
        only_f1_is_a_shell_chord_on_its_own();
        conventional_alt_tab_ring_is_owned_before_a_focused_client();
        a_shell_owned_press_never_produces_an_unmatched_client_release();
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn a_foreign_group_is_laid_out_and_hit_testable() {
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

        a_mirrored_foreign_group_is_laid_out_and_hit_testable();
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn desktop_modals_take_input_before_a_focused_client() {
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

        fn desktop_modals_take_input_before_a_focused_client_can_consume_it() {
            for overlay in ["shortcut_help", "projection"] {
                let mut fb = FrameBuilder::new((1600, 900));
                fb.with_chrome(|chrome| match overlay {
                    "shortcut_help" => chrome.overlays.shortcut_help = true,
                    "projection" => chrome.overlays.projection = true,
                    _ => unreachable!(),
                });
                assert!(
                    chrome_captures_input(&fb),
                    "{overlay} is painted as modal and must revoke client focus"
                );
            }

            let mut capture = FrameBuilder::new((1600, 900));
            assert!(
                capture
                    .host_mut()
                    .open_capture_studio(ui::widget::CaptureMode::Screenshot)
            );
            assert!(
                chrome_captures_input(&capture),
                "Print Screen's chooser must receive its follow-up keys and clicks"
            );

            let mut markup = FrameBuilder::new((1600, 900));
            markup
                .host_mut()
                .open_screenshot_markup(std::sync::Arc::from([0_u8; 4]), 1, 1, None);
            assert!(
                chrome_captures_input(&markup),
                "screenshot editing must not leak input into the captured app"
            );
        }

        fn maximized_restore_band_takes_pointer_before_the_client() {
            let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
            let first = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
            first.maximized = true;
            let group = first.id;
            let mut fb = FrameBuilder::with_tree(tree, (1280, 800));
            fb.with_chrome(|chrome| chrome.set_maximized_group(Some(group)));
            let _ = fb.paint_settled();

            // At rest the bar hides: the top of the window is the client's.
            fb.host_mut()
                .note_pointer(vello::kurbo::Point::new(1210.0, 70.0));
            assert!(!chrome_captures_pointer(&fb));

            // Reached for, its controls take the pointer, never the keyboard.
            let strip = fb.chrome().tree().desktop.workspaces[0].strips[0].id;
            fb.host_mut()
                .group_at_for_tests(strip, 0)
                .expect("maximized group")
                .set_maximized_chrome_shown_for_tests(true);
            let _ = fb.paint_settled();
            let restore = fb
                .host()
                .active_window_bounds()
                .and_then(|bounds| bounds.restore)
                .expect("Restore shows");
            fb.host_mut().note_pointer(restore.center());
            assert!(
                chrome_captures_pointer(&fb),
                "the visible Restore/Close pills must receive their own click"
            );
            assert!(
                !chrome_captures_input(&fb),
                "the bar never takes the client's keyboard"
            );

            fb.host_mut()
                .note_pointer(vello::kurbo::Point::new(640.0, 400.0));
            assert!(
                !chrome_captures_pointer(&fb),
                "down in the app the maximized client must remain interactive"
            );
        }

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
                .desktop
                .active_workspace()
                .map(|ws| ws.id)
                .expect("a fresh FrameBuilder has an active workspace");
            let (reply, _parked) = tokio::sync::oneshot::channel();
            queue.spawn(crate::portal::FilePickerRequest {
                caller: crate::portal::PortalCaller::new(workspace, "so.tilo.tests"),
                parent_window: None,
                title: "Insert image".into(),
                save: false,
                directory: false,
                writable_directory: false,
                multiple: false,
                accept_label: None,
                current_name: None,
                filters: Vec::new(),
                scope: None,
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

        the_peer_sync_card_takes_input_from_a_focused_client();
        desktop_modals_take_input_before_a_focused_client_can_consume_it();
        maximized_restore_band_takes_pointer_before_the_client();
        the_file_chooser_takes_input_from_a_focused_client();
    }
}
pub fn step_active_window(tree: &WorkspaceTree, steps: i32) -> Option<(StripId, GroupId, u64)> {
    let ws = tree.desktop.active_workspace()?;
    // Focused strip, else the first — the same fallback `place_tab` applies when a recorded pointer has gone stale.
    let strip = tree
        .desktop
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
        .desktop
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
use crate::compositor::lock_controller::LockKey;
/// Is `event` the remote-input kill switch, `Super+Esc`, on key *press*?
/// The panic key for a shared session — stops whatever a guest is doing.
/// Checked ahead of the client router and the shell's own Escape handling
/// so an open overlay or a keyboard-grabbing client can't swallow it.
pub fn is_remote_input_kill_switch(event: &WindowEvent, modifiers: Modifiers) -> bool {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    event.state == ElementState::Pressed
        && modifiers.state().super_key()
        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
}

/// Is `event` a press of the fixed mouse-keys chord, Shift+Alt+Num Lock?
/// Held-key repeats are ignored so holding the chord flips it once.
pub fn is_mouse_keys_toggle(event: &WindowEvent, modifiers: Modifiers) -> bool {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return false;
    };
    key.state == ElementState::Pressed
        && !key.repeat
        && crate::shell::fixed_chords::fixed_action(
            chord_key_for_logical(&key.logical_key),
            ChordMods::from(&modifiers),
        ) == Some(crate::shell::fixed_chords::FixedChordAction::ToggleMouseKeys)
}

/// Is `event` a press of the configured session-lock binding?
///
/// Called for every input event the compositor takes off the backend,
/// pointer motion included, so it rejects non-keyboard events first and
/// reads the host's cached `shortcuts` rather than the settings file.
pub fn is_lock_chord(
    event: &WindowEvent,
    modifiers: Modifiers,
    shortcuts: &settings_store::hardware::ShortcutConfig,
) -> bool {
    shortcuts
        .bindings
        .get(&settings_store::hardware::ShortcutAction::Lock)
        .is_some_and(|chord| lock_chord_matches(event, modifiers, chord))
}

/// Session-lock bindings run before client routing, so their matching must
/// accept every key the editable shortcut UI can persist.  In particular,
/// named keys such as F12 are already withheld from the focused client by
/// `configured_shortcut_matches`.
pub fn lock_chord_matches(
    event: &WindowEvent,
    modifiers: Modifiers,
    chord: &settings_store::hardware::KeyChord,
) -> bool {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    if event.state != ElementState::Pressed {
        return false;
    }
    crate::shell::overlay_host::shortcut_chord_matches(&event.logical_key, modifiers, chord)
}

/// Map a live winit key event to the lock screen's normalised [`LockKey`]
/// while locked. Non-press/unhandled keys become [`LockKey::Ignored`] —
/// still swallowed (never routed to apps behind the overlay), but inert on
/// the password buffer. The test-control path produces the same `LockKey`s so both drive one routing.
pub fn winit_event_to_lock_key(event: &WindowEvent) -> LockKey {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return LockKey::Ignored;
    };
    if event.state != ElementState::Pressed {
        return LockKey::Ignored;
    }
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => LockKey::Submit,
        Key::Named(NamedKey::Escape) => LockKey::Cancel,
        Key::Named(NamedKey::Backspace) => LockKey::Backspace,
        Key::Named(NamedKey::Space) => LockKey::Text(" ".to_string()),
        Key::Character(s) => LockKey::Text(s.to_string()),
        _ => {
            // Some layouts deliver typed text on otherwise-Named keys.
            match &event.text {
                Some(t) if !t.is_empty() && !t.chars().all(char::is_control) => {
                    LockKey::Text(t.to_string())
                }
                _ => LockKey::Ignored,
            }
        }
    }
}
#[cfg(test)]
mod lock_chord_tests {
    use super::*;
    use crate::shell::input_policy as client_input;
    use crate::shell::test_control::keyboard_input_event;

    use winit::keyboard::{ModifiersState, SmolStr};
    fn mods_with_super(on: bool) -> Modifiers {
        let mut mask = ModifiersState::empty();
        if on {
            mask |= ModifiersState::SUPER;
        }
        Modifiers::from(mask)
    }

    /// The lock chord against the default shortcut table.
    fn locks(event: &WindowEvent, modifiers: Modifiers) -> bool {
        is_lock_chord(
            event,
            modifiers,
            &settings_store::hardware::ShortcutConfig::default(),
        )
    }

    #[test]
    fn lock_chord_follows_the_passed_shortcut_snapshot() {
        use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
        let mut rebound = ShortcutConfig::default();
        rebound.bindings.insert(
            ShortcutAction::Lock,
            KeyChord::parse("Ctrl+Alt+L").expect("a valid chord"),
        );
        let l = keyboard_input_event(&Key::Character(SmolStr::new("l")), true);
        let ctrl_alt = Modifiers::from(ModifiersState::CONTROL | ModifiersState::ALT);
        assert!(is_lock_chord(&l, ctrl_alt, &rebound));
        assert!(
            !is_lock_chord(&l, ctrl_alt, &ShortcutConfig::default()),
            "a custom lock chord only locks while the snapshot assigns it"
        );
        assert!(
            !is_lock_chord(&l, mods_with_super(true), &rebound),
            "rebinding Lock screen replaces Super+L"
        );

        let mut unbound = ShortcutConfig::default();
        unbound.bindings.remove(&ShortcutAction::Lock);
        assert!(!is_lock_chord(&l, mods_with_super(true), &unbound));
        assert!(!is_lock_chord(&l, ctrl_alt, &unbound));
    }

    #[test]
    fn lock_chord_is_only_a_super_l_or_configured_press() {
        /// Super+L on key press is the lock chord.
        fn super_l_press_is_lock_chord() {
            let ev = keyboard_input_event(&Key::Character(SmolStr::new("l")), true);
            assert!(locks(&ev, mods_with_super(true)));
            // A layout/caps state may report an upper-case logical key without an
            // extra Shift modifier; key comparison remains case-insensitive.
            let ev_upper = keyboard_input_event(&Key::Character(SmolStr::new("L")), true);
            assert!(locks(&ev_upper, mods_with_super(true)));
        }

        fn other_super_chords_and_extra_modifiers_do_not_lock() {
            let search = keyboard_input_event(&Key::Character(SmolStr::new("k")), true);
            assert!(
                !locks(&search, mods_with_super(true)),
                "Super+K must reach Search instead of locking the session"
            );

            let lock = keyboard_input_event(&Key::Character(SmolStr::new("l")), true);
            let shifted = Modifiers::from(ModifiersState::SUPER | ModifiersState::SHIFT);
            assert!(
                !locks(&lock, shifted),
                "an extra modifier must not broaden Super+L into a lock chord"
            );
        }

        /// Without Super held, plain `l` is not the lock chord (it types).
        fn l_without_super_is_not_lock_chord() {
            let ev = keyboard_input_event(&Key::Character(SmolStr::new("l")), true);
            assert!(!locks(&ev, mods_with_super(false)));
        }

        /// The lock chord only fires on press, not release — else key-up would re-trigger and toggle twice.
        fn super_l_release_is_not_lock_chord() {
            let ev = keyboard_input_event(&Key::Character(SmolStr::new("l")), false);
            assert!(!locks(&ev, mods_with_super(true)));
        }

        fn configured_named_lock_keys_use_the_editor_matcher_without_claiming_neighbours() {
            let ctrl = Modifiers::from(ModifiersState::CONTROL);
            let f12 = settings_store::hardware::KeyChord::parse("Ctrl+F12")
                .expect("the shortcut editor accepts named function keys");
            let f12_press = keyboard_input_event(&Key::Named(NamedKey::F12), true);
            assert!(lock_chord_matches(&f12_press, ctrl, &f12));

            let f11_press = keyboard_input_event(&Key::Named(NamedKey::F11), true);
            assert!(
                !lock_chord_matches(&f11_press, ctrl, &f12),
                "a nearby client-owned function key must not be claimed"
            );
            let f12_release = keyboard_input_event(&Key::Named(NamedKey::F12), false);
            assert!(
                !lock_chord_matches(&f12_release, ctrl, &f12),
                "only the captured press may lock; its release stays non-actioning"
            );

            let navigation = settings_store::hardware::KeyChord::parse("Ctrl+Left")
                .expect("the shortcut editor accepts named navigation keys");
            let left_press = keyboard_input_event(&Key::Named(NamedKey::ArrowLeft), true);
            assert!(
                lock_chord_matches(&left_press, ctrl, &navigation),
                "a named navigation binding must use the same matcher as the editor"
            );
        }

        /// A non-keyboard event (cursor move) is never the lock chord even with Super held.
        fn cursor_move_is_not_lock_chord() {
            let ev = ui::synthetic_input::cursor_moved(10.0, 10.0);
            assert!(!locks(&ev, mods_with_super(true)));
        }

        super_l_press_is_lock_chord();
        other_super_chords_and_extra_modifiers_do_not_lock();
        l_without_super_is_not_lock_chord();
        super_l_release_is_not_lock_chord();
        configured_named_lock_keys_use_the_editor_matcher_without_claiming_neighbours();
        cursor_move_is_not_lock_chord();
    }

    #[test]
    fn remote_input_kill_switch_is_only_a_super_escape_press_owned_by_the_shell() {
        /// `Super+Esc` on press is the remote-input kill switch.
        fn super_escape_press_is_the_remote_input_kill_switch() {
            let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
            assert!(is_remote_input_kill_switch(&ev, mods_with_super(true)));
        }

        /// Escape on its own closes an overlay and reaches modal clients; must not stop a shared session by accident.
        fn escape_without_super_is_not_the_kill_switch() {
            let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
            assert!(!is_remote_input_kill_switch(&ev, mods_with_super(false)));
        }

        /// Press only — a re-firing release would engage the latch twice and post a second toast for one keystroke.
        fn super_escape_release_is_not_the_kill_switch() {
            let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), false);
            assert!(!is_remote_input_kill_switch(&ev, mods_with_super(true)));
        }

        /// The panic key is not a key a client can be handed — `Super` is the
        /// compositor modifier the shell's chord table already claims, so no client can steal it.
        fn super_escape_is_a_shell_chord_and_never_reaches_a_client() {
            let ev = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
            assert!(client_input::is_shell_chord(
                &ev,
                &mods_with_super(true),
                false
            ));
            assert!(
                client_input::is_shell_chord(&ev, &mods_with_super(true), true),
                "an open overlay must not swallow the panic key either"
            );
        }

        super_escape_press_is_the_remote_input_kill_switch();
        escape_without_super_is_not_the_kill_switch();
        super_escape_release_is_not_the_kill_switch();
        super_escape_is_a_shell_chord_and_never_reaches_a_client();
    }

    #[test]
    fn lock_keys_map_presses_to_submit_cancel_text_and_backspace() {
        /// Enter and Escape on press are the explicit unlock action.
        fn enter_and_escape_press_map_to_submit_and_cancel() {
            let enter = keyboard_input_event(&Key::Named(NamedKey::Enter), true);
            let escape = keyboard_input_event(&Key::Named(NamedKey::Escape), true);
            assert_eq!(winit_event_to_lock_key(&enter), LockKey::Submit);
            assert_eq!(winit_event_to_lock_key(&escape), LockKey::Cancel);
        }

        /// A plain character keypress feeds the password buffer as text — it doesn't itself unlock.
        fn char_key_maps_to_text() {
            let ev = keyboard_input_event(&Key::Character(SmolStr::new("a")), true);
            assert_eq!(winit_event_to_lock_key(&ev), LockKey::Text("a".to_string()));
        }

        /// Backspace maps to a buffer edit; a key *release* is ignored (only presses type).
        fn backspace_maps_and_release_is_ignored() {
            let bs = keyboard_input_event(&Key::Named(NamedKey::Backspace), true);
            assert_eq!(winit_event_to_lock_key(&bs), LockKey::Backspace);
            let release = keyboard_input_event(&Key::Named(NamedKey::Enter), false);
            assert_eq!(winit_event_to_lock_key(&release), LockKey::Ignored);
        }

        enter_and_escape_press_map_to_submit_and_cancel();
        char_key_maps_to_text();
        backspace_maps_and_release_is_ignored();
    }
}
