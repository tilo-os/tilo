//! Test helpers for synthesizing shell-level input in unit tests.
//!
//! Winit's `KeyEvent` carries a private `platform_specific` field that cannot
//! be filled in from outside the winit crate, so constructing a full
//! `WindowEvent::KeyboardInput` purely from test code is impossible on
//! Linux. Instead we expose a shell-local [`KeyPress`] struct — a tiny,
//! constructible surface that [`super::Shell::dispatch_key`] consumes
//! directly. The real compositor path takes a winit `WindowEvent`, extracts
//! the same logical-key + modifier pair, builds a [`KeyPress`], and feeds it
//! through the same `dispatch_key` — so production and test paths exercise
//! identical routing code.
//!
//! Added in Plan 3 Task 28 alongside the global-hotkey wiring. Spec:
//! `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` §10 (global
//! hotkeys: Cmd+K, Cmd+Tab, Escape).
//!
//! `Mods` is hand-rolled as an `u8` bitmask rather than pulled in via
//! `bitflags` so the shell crate keeps a tight dependency surface (see
//! `Cargo.toml` — no `bitflags` today).

/// Logical modifier mask for [`KeyPress`]. Independent of winit's
/// `ModifiersState` so tests never need to drag in a winit `Modifiers`
/// value; the real `WindowEvent` dispatcher inside `Shell` projects the
/// winit modifier bits onto this mask.
///
/// `CMD` is the Super/Meta key — the "GUI" modifier the spec uses for
/// Cmd+K / Cmd+Tab. On the Linux dev target Ctrl is accepted as an
/// ergonomic stand-in; both set the `CMD` bit so `Mods::CMD` is one flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mods(u8);

impl Mods {
    /// No modifier pressed.
    pub const NONE: Mods = Mods(0);
    /// Super / Meta / "Cmd" key. Also accepted via Ctrl on Linux for
    /// ergonomic parity with hardware that lacks a dedicated Super.
    pub const CMD: Mods = Mods(1 << 0);
    /// Shift modifier.
    pub const SHIFT: Mods = Mods(1 << 1);
    /// Alt / Option modifier.
    pub const ALT: Mods = Mods(1 << 2);

    /// Whether every bit in `other` is set in `self`.
    pub fn contains(self, other: Mods) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Bitwise OR — build a compound mask like `Mods::CMD | Mods::SHIFT`.
    pub const fn union(self, other: Mods) -> Mods {
        Mods(self.0 | other.0)
    }

    /// Raw u8 for serialization / equality checks in the dispatcher.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, rhs: Mods) -> Mods {
        self.union(rhs)
    }
}

/// A synthesized key press event. Constructed via [`key_press`] and routed
/// into [`super::Shell::dispatch_key`].
///
/// `logical` is either a named key label (`"Escape"`, `"Tab"`, `"ArrowUp"`,
/// …) or a single printable character (`"K"`, `"k"`, `"/"`). The dispatcher
/// normalizes characters to lowercase before matching so both `Cmd+K` and
/// `Cmd+k` hit the same branch — the test harness uses the uppercase form
/// purely for readability.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyPress {
    /// Logical key label (see struct doc for the allowed values).
    pub logical: String,
    /// Modifier mask held at the time of the press.
    pub mods: Mods,
}

/// Build a [`KeyPress`] for a given logical key label and modifier mask.
/// Canonical names: `"Escape"`, `"Tab"`, `"Enter"`, `"ArrowUp"`,
/// `"ArrowDown"`, `"ArrowLeft"`, `"ArrowRight"`, `"Backspace"`, `"Space"`.
/// Any single-character string is treated as a printable character.
pub fn key_press(logical: &str, mods: Mods) -> KeyPress {
    KeyPress {
        logical: logical.to_string(),
        mods,
    }
}
