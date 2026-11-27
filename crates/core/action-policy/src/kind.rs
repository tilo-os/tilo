//! The three kinds of action and sets of them.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What an action is able to do, derived by the framework from the action's
/// code and never declared by an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Shows or reads: views, `read_content`, view effects.
    Read,
    /// Changes the document and nothing outside it.
    Undoable,
    /// Reaches outside the document: sends, writes a device, calls a server.
    Outside,
}

impl ActionKind {
    pub const ALL: [Self; 3] = [Self::Read, Self::Undoable, Self::Outside];

    const fn bit(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Undoable => 2,
            Self::Outside => 4,
        }
    }

    /// The kind as people read it in a grant sentence.
    #[must_use]
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::Read => "reading",
            Self::Undoable => "undoable changes",
            Self::Outside => "outside effects",
        }
    }
}

impl ActionKind {
    /// What a person's own press of an action of this kind waits for, `destructive` when its
    /// declaration marks it so: a destructive action undo takes back confirms unless Shift is
    /// held, one undo cannot take back always confirms, and the rest acts on the press. What
    /// leaves the system always confirms, whatever its kind; its declaration says so apart from
    /// its kind. An agent's, an automation's or another app's call is decided by its grants and
    /// the action's own asks, never by this.
    #[must_use]
    pub const fn confirms(self, destructive: bool) -> Confirms {
        match (destructive, self) {
            (false, _) => Confirms::Never,
            (true, Self::Undoable) => Confirms::UnlessShift,
            (true, Self::Read | Self::Outside) => Confirms::Always,
        }
    }
}

/// What a person's own press of an action waits for before it runs: the one rule every window,
/// the shell and every language's apps decide a press by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Confirms {
    /// It acts on the press: copying, opening, playing, refreshing, making a new thing.
    Never,
    /// It confirms first, and Shift held on the press skips that: a destructive action undo
    /// takes back (a delete).
    UnlessShift,
    /// It confirms first, Shift held or not: it leaves the system, or is destructive and undo
    /// cannot take it back.
    Always,
}

impl Confirms {
    /// Whether a press, with Shift held when `shift`, waits for the person's yes.
    #[must_use]
    pub const fn on_press(self, shift: bool) -> bool {
        match self {
            Self::Never => false,
            Self::UnlessShift => !shift,
            Self::Always => true,
        }
    }

    /// Whether a plain press confirms: what an action's contract and its catalogue offer say.
    #[must_use]
    pub const fn by_default(self) -> bool {
        self.on_press(false)
    }
}

impl fmt::Display for ActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Undoable => "undoable",
            Self::Outside => "outside",
        })
    }
}

/// The kinds a grant covers. Serialised as the list of kinds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KindSet(u8);

impl KindSet {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1);
    pub const UNDOABLE: Self = Self(2);
    pub const OUTSIDE: Self = Self(4);
    pub const READ_UNDOABLE: Self = Self(3);
    pub const ALL: Self = Self(7);

    #[must_use]
    pub const fn of(kind: ActionKind) -> Self {
        Self(kind.bit())
    }

    #[must_use]
    pub const fn contains(self, kind: ActionKind) -> bool {
        self.0 & kind.bit() != 0
    }

    #[must_use]
    pub const fn with(self, kind: ActionKind) -> Self {
        Self(self.0 | kind.bit())
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The kinds in the set, in `Read`, `Undoable`, `Outside` order.
    pub fn iter(self) -> impl Iterator<Item = ActionKind> {
        ActionKind::ALL
            .into_iter()
            .filter(move |kind| self.contains(*kind))
    }
}

impl FromIterator<ActionKind> for KindSet {
    fn from_iter<I: IntoIterator<Item = ActionKind>>(iter: I) -> Self {
        iter.into_iter().fold(Self::NONE, Self::with)
    }
}

impl From<ActionKind> for KindSet {
    fn from(kind: ActionKind) -> Self {
        Self::of(kind)
    }
}

impl fmt::Debug for KindSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl Serialize for KindSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // A sequence of known length: compact formats need it up front.
        self.iter().collect::<Vec<_>>().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for KindSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<ActionKind>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An irreversible destructive press always confirms, Shift held or not; Shift skips only
    /// the confirmation of what undo takes back; nothing else confirms.
    #[test]
    fn shift_skips_only_what_undo_takes_back() {
        for shift in [false, true] {
            assert!(ActionKind::Outside.confirms(true).on_press(shift));
            assert_eq!(ActionKind::Undoable.confirms(true).on_press(shift), !shift);
            for kind in ActionKind::ALL {
                assert!(!kind.confirms(false).on_press(shift), "{kind}");
            }
        }
    }
}
