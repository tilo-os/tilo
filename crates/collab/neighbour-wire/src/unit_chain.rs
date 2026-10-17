//! Whether an encoded unit of a shared surface continues the decoder's
//! reference chain, from its [`crate::NeighbourMsg::SurfaceFrame`] `seq`.
//!
//! Delta units reference the units before them. A unit lost between the
//! owner's encoder and a decoder (a saturated queue, a failed read, a bound
//! on held frames) leaves every delta after it referencing a picture the
//! decoder never had: fed them, it shows corrupt pictures or rejects them
//! until the next keyframe. Every viewer path keeps one [`UnitChain`] per
//! stream and feeds its decoder only what [`UnitChain::admit`] lets through.

/// What to do with one encoded unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Decode it: a keyframe, or the delta that continues the chain.
    Decode,
    /// Hold it back: a unit before it is missing, or the stream has not
    /// reached a keyframe yet. `broke` is whether this unit is the one that
    /// found the chain broken, the moment to ask the owner for a keyframe.
    AwaitingKeyframe { broke: bool },
    /// Drop it: a delta older than one already decoded, overtaken on the
    /// way or repeated. The chain it belonged to has moved on.
    Stale,
}

/// One stream's reference chain.
#[derive(Debug, Clone, Default)]
pub struct UnitChain {
    /// The `seq` the next delta must carry; `None` until a keyframe starts
    /// a chain.
    next: Option<u64>,
}

impl UnitChain {
    /// Admit unit `seq`, a keyframe or not. A keyframe always starts a
    /// chain, whatever its number: a share replaced by a new session numbers
    /// its units from zero again, and its first keyframe can overtake the
    /// stream start announcing it.
    pub fn admit(&mut self, seq: u64, keyframe: bool) -> Admission {
        if keyframe || self.next == Some(seq) {
            self.next = seq.checked_add(1);
            return Admission::Decode;
        }
        if self.next.is_some_and(|next| seq < next) {
            return Admission::Stale;
        }
        Admission::AwaitingKeyframe {
            broke: self.next.take().is_some(),
        }
    }

    /// The decoder rejected the unit last admitted, or the caller dropped
    /// units after admitting them: what the decoder holds is no base for
    /// the deltas that follow.
    pub fn lose(&mut self) {
        self.next = None;
    }

    /// Whether a delta would be decoded now, so the chain is intact.
    pub fn intact(&self) -> bool {
        self.next.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gap_holds_deltas_back_until_a_keyframe_and_old_deltas_are_stale() {
        let mut chain = UnitChain::default();
        assert_eq!(
            chain.admit(0, false),
            Admission::AwaitingKeyframe { broke: false },
            "a stream starts at a keyframe"
        );
        assert_eq!(chain.admit(1, true), Admission::Decode);
        assert_eq!(chain.admit(2, false), Admission::Decode);
        assert_eq!(chain.admit(2, false), Admission::Stale, "a repeat");
        assert_eq!(
            chain.admit(4, false),
            Admission::AwaitingKeyframe { broke: true },
            "unit 3 went missing"
        );
        assert_eq!(
            chain.admit(3, false),
            Admission::AwaitingKeyframe { broke: false },
            "arriving late does not mend the chain"
        );
        assert_eq!(
            chain.admit(5, false),
            Admission::AwaitingKeyframe { broke: false }
        );
        assert_eq!(
            chain.admit(0, true),
            Admission::Decode,
            "a restarted session"
        );
        assert_eq!(chain.admit(1, false), Admission::Decode);
        chain.lose();
        assert!(!chain.intact());
        assert_eq!(
            chain.admit(2, false),
            Admission::AwaitingKeyframe { broke: false }
        );
    }
}
