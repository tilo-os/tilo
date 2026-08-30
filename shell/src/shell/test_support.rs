//! Test helpers for synthesizing shell-level input in unit tests.
//!
//! Winit's `KeyEvent` carries a private `platform_specific` field that
//! can't be filled in from outside the winit crate, so a full
//! `WindowEvent::KeyboardInput` can't be built from test code on Linux.
//! [`KeyPress`] is a tiny constructible stand-in that
//! [`super::Shell::dispatch_key`] consumes directly; the real compositor
//! path extracts the same logical-key + modifier pair from a winit event
//! and feeds it through the same dispatcher, so production and test paths
//! exercise identical routing code.
//!
//! `Mods` is hand-rolled as a `u8` bitmask rather than pulled in via
//! `bitflags`, to keep the crate's dependency surface tight.

/// Logical modifier mask for [`KeyPress`], independent of winit's
/// `ModifiersState`. `CMD` is the Super/Meta "GUI" modifier (Cmd+K,
/// Cmd+Tab); on the Linux dev target Ctrl is an accepted stand-in, both
/// setting the same bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const CMD: Mods = Mods(1 << 0);
    pub const SHIFT: Mods = Mods(1 << 1);
    pub const ALT: Mods = Mods(1 << 2);

    /// Whether every bit in `other` is set in `self`.
    pub fn contains(self, other: Mods) -> bool {
        (self.0 & other.0) == other.0
    }

    #[must_use]
    pub const fn union(self, other: Mods) -> Mods {
        Mods(self.0 | other.0)
    }

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

/// A synthesized key press, routed into [`super::Shell::dispatch_key`].
/// The dispatcher lowercases characters, so `Cmd+K` and `Cmd+k` hit the
/// same branch — tests use uppercase purely for readability.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub logical: String,
    pub mods: Mods,
}

/// `logical`: canonical names (`"Escape"`, `"Tab"`, `"ArrowUp"`, …) or any
/// single-character string for a printable character.
pub fn key_press(logical: &str, mods: Mods) -> KeyPress {
    KeyPress {
        logical: logical.to_string(),
        mods,
    }
}
