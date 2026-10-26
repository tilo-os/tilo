//! When an encoded unit may reach a decoder: the reference-chain admission
//! both video profiles share.
//!
//! Delta units reference the units before them. A unit lost between the
//! encoder and a decoder (a saturated queue, a failed read, a bound on held
//! frames) leaves every delta after it referencing a picture the decoder
//! never had: fed them, it shows corrupt pictures or rejects them until the
//! next keyframe. Every viewer path keeps one [`UnitChain`] per stream and
//! feeds its decoder only what [`UnitChain::admit`] lets through.
//!
//! [`Sequence`] is the transport-side companion: it turns the sequence
//! numbers a profile already carries into "continuous", "a gap of n frames"
//! or "stale", without knowing which codec or queue produced them.

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
    /// The sequence the next delta must carry; `None` until a keyframe starts
    /// a chain.
    next: Option<u64>,
}

impl UnitChain {
    /// Admit unit `sequence`, a keyframe or not. A keyframe always starts a
    /// chain, whatever its number: a share replaced by a new session numbers
    /// its units from zero again, and its first keyframe can overtake the
    /// stream start announcing it.
    pub fn admit(&mut self, sequence: u64, keyframe: bool) -> Admission {
        if keyframe || self.next == Some(sequence) {
            self.next = sequence.checked_add(1);
            return Admission::Decode;
        }
        if self.next.is_some_and(|next| sequence < next) {
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

/// What observing one sequence number showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceEvent {
    /// The first unit of a stream: nothing before it was observed.
    First,
    /// The next unit after the one last observed.
    Continuous,
    /// This many units after the one last observed never arrived.
    Gap { dropped: u64 },
}

/// Why a sequence number cannot continue the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SequenceError {
    /// The finite sequence space was exhausted; a stream that wraps to zero
    /// is a new stream, not a continuation.
    #[error("encoded stream sequence space exhausted")]
    Exhausted,
    /// A unit older than one already observed. The receiver drops it and
    /// forgets its history rather than displaying stale video.
    #[error("stale or out-of-order sequence {received} after {previous}")]
    Stale { previous: u64, received: u64 },
}

/// The sequence history of one received stream.
///
/// The first observed unit starts the stream. A later unit either continues
/// it, names the gap before it, or is stale. Exhaustion and staleness forget
/// the history so a newly established stream can start cleanly; a gap keeps
/// it, because the stream continues.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sequence {
    last: Option<u64>,
}

impl Sequence {
    /// Observe `received`, the next unit's sequence number.
    pub fn observe(&mut self, received: u64) -> Result<SequenceEvent, SequenceError> {
        let Some(previous) = self.last else {
            self.last = Some(received);
            return Ok(SequenceEvent::First);
        };
        let Some(expected) = previous.checked_add(1) else {
            self.last = None;
            return Err(SequenceError::Exhausted);
        };
        if received < expected {
            self.last = None;
            return Err(SequenceError::Stale { previous, received });
        }
        self.last = Some(received);
        if received > expected {
            return Ok(SequenceEvent::Gap {
                dropped: received.saturating_sub(expected),
            });
        }
        Ok(SequenceEvent::Continuous)
    }

    /// Forget the history: the next observed unit starts a new stream.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// The last sequence observed, before any gap or stale refusal.
    pub fn last(&self) -> Option<u64> {
        self.last
    }
}
