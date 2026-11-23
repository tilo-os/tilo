//! Parts and their states.

/// A part a `ui` component declares. A theme reaches parts, never an app's
/// view tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Part {
    /// The ground of a window, which every app paints on.
    WindowGround,
    /// The body of floating chrome: a menu, popover, sheet, notification.
    FloatingPanel,
    /// The body of the top bar.
    BarBody,
    /// The face of a raised control: a field, dropdown, segmented strip,
    /// card. `tokens.fill` is its face colour.
    ControlFace,
    /// The container of a button. `tokens.fill`, `ink` and `edge` are the
    /// face colours its role and state resolved to.
    ButtonContainer,
    /// The ring that marks keyboard focus, around the part it belongs to.
    FocusRing,
    /// The track of a scroll bar.
    ScrollTrack,
    /// The thumb of a scroll bar.
    ScrollThumb,
}

impl Part {
    /// Every part, in wire order.
    pub const ALL: [Self; 8] = [
        Self::WindowGround,
        Self::FloatingPanel,
        Self::BarBody,
        Self::ControlFace,
        Self::ButtonContainer,
        Self::FocusRing,
        Self::ScrollTrack,
        Self::ScrollThumb,
    ];

    /// Its index in [`Self::ALL`], as the wire carries it.
    #[must_use]
    pub fn index(self) -> u8 {
        match self {
            Self::WindowGround => 0,
            Self::FloatingPanel => 1,
            Self::BarBody => 2,
            Self::ControlFace => 3,
            Self::ButtonContainer => 4,
            Self::FocusRing => 5,
            Self::ScrollTrack => 6,
            Self::ScrollThumb => 7,
        }
    }

    /// The part at `index` in [`Self::ALL`].
    #[must_use]
    pub fn from_index(index: u8) -> Option<Self> {
        Self::ALL.get(usize::from(index)).copied()
    }
}

/// The states a part is in at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct States(u16);

impl States {
    /// At rest: none of the others.
    pub const REST: Self = Self(0);
    pub const HOVERED: Self = Self(1);
    pub const PRESSED: Self = Self(1 << 1);
    pub const FOCUSED: Self = Self(1 << 2);
    pub const SELECTED: Self = Self(1 << 3);
    pub const DISABLED: Self = Self(1 << 4);
    pub const DRAGGING: Self = Self(1 << 5);
    /// Someone else changed what the part shows, a moment ago.
    pub const CHANGED_BY_OTHER: Self = Self(1 << 6);
    /// A toggle that is on.
    pub const ON: Self = Self(1 << 7);

    const KNOWN: u16 = (1 << 8) - 1;

    /// Whether every state of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with `other` set when `on`, cleared otherwise.
    #[must_use]
    pub const fn with(self, other: Self, on: bool) -> Self {
        if on {
            Self(self.0 | other.0)
        } else {
            Self(self.0 & !other.0)
        }
    }

    /// The bits, as the wire carries them.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// The states of `bits`; unknown bits are dropped.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & Self::KNOWN)
    }
}

impl std::ops::BitOr for States {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Where a part is: the states it is in now, and the ones it is leaving
/// (equal once a transition settled).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PartState {
    pub now: States,
    pub was: States,
}

impl PartState {
    /// Settled in `states`.
    #[must_use]
    pub const fn settled(states: States) -> Self {
        Self {
            now: states,
            was: states,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_round_trips_its_index() {
        for part in Part::ALL {
            assert_eq!(Part::from_index(part.index()), Some(part));
        }
        assert_eq!(Part::from_index(200), None);
    }

    #[test]
    fn states_set_and_clear() {
        let s = States::REST.with(States::HOVERED, true) | States::PRESSED;
        assert!(s.contains(States::HOVERED | States::PRESSED));
        assert!(!s.with(States::HOVERED, false).contains(States::HOVERED));
        assert_eq!(States::from_bits(0xffff).bits(), States::KNOWN);
    }
}
