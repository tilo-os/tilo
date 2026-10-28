//! What an endpoint does with a document op it receives: the verdict on
//! whether its channel admits it as cleartext, and the hold for ops that
//! cannot open yet.
//!
//! Whether cleartext belongs on a channel is decided by the channel's
//! governance gate alone (`channel_crypto::admission`); an endpoint applies
//! the verdict and keeps no rule of its own. An op it cannot decide or open
//! yet waits in its route's [`InboundHold`] until the route's state changes
//! or history supplies it. Nothing waits on a timer, and nothing is dropped
//! unsaid.

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::{ArtifactId, Ed25519PublicKey, SignedOp};

/// The verdict on one op from its channel's gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpAdmission {
    /// No verdict yet: the op waits for one.
    Unstated,
    /// The channel admits cleartext for this op.
    CleartextAdmitted,
    /// The channel requires this op sealed.
    EncryptionRequired,
}

impl OpAdmission {
    /// Whether cleartext opens under this verdict, or `None` while there is
    /// none and the op waits.
    #[must_use]
    pub fn admits_cleartext(self) -> Option<bool> {
        match self {
            Self::Unstated => None,
            Self::CleartextAdmitted => Some(true),
            Self::EncryptionRequired => Some(false),
        }
    }
}

/// Which op an entry is: an author's op on an artifact, by its seq.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpKey {
    pub artifact: ArtifactId,
    pub author: Ed25519PublicKey,
    pub seq: u64,
}

impl OpKey {
    #[must_use]
    pub fn of(op: &SignedOp) -> Self {
        Self {
            artifact: op.artifact,
            author: op.author,
            seq: op.seq,
        }
    }
}

impl std::fmt::Display for OpKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} by {} at seq {}",
            self.artifact.0,
            hex::encode(self.author.as_bytes()),
            self.seq
        )
    }
}

/// A route's ops that could not open yet. Each waits until the route's
/// state changes ([`Self::take`], then hold again what still cannot open)
/// or history supplies it ([`Self::settle`]). Past `capacity` the oldest is
/// let go of as owed by history: its route pages history for it
/// ([`Self::owed_pages`]) rather than lose it.
#[derive(Debug)]
pub struct InboundHold<T> {
    capacity: usize,
    held: VecDeque<(OpKey, T)>,
    owed: HashSet<OpKey>,
}

impl<T> InboundHold<T> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            held: VecDeque::new(),
            owed: HashSet::new(),
        }
    }

    /// Hold `item` until the route's state changes. Holding an op already
    /// held or owed does nothing. The op let go of as owed by history to
    /// make room, if any.
    pub fn hold(&mut self, key: OpKey, item: T) -> Option<OpKey> {
        if self.owed.contains(&key) || self.held.iter().any(|(held, _)| *held == key) {
            return None;
        }
        let owed = if self.held.len() >= self.capacity {
            self.held.pop_front().map(|(oldest, _)| oldest)
        } else {
            None
        };
        if let Some(oldest) = owed {
            self.owed.insert(oldest);
        }
        self.held.push_back((key, item));
        owed
    }

    /// History supplied the op: it is neither held nor owed any longer.
    /// Whether it was either.
    pub fn settle(&mut self, key: &OpKey) -> bool {
        let held = self.held.len();
        self.held.retain(|(entry, _)| entry != key);
        self.owed.remove(key) | (self.held.len() != held)
    }

    /// Every held op, oldest first, to try again after the route's state
    /// changed. What still cannot open is held again.
    pub fn take(&mut self) -> Vec<(OpKey, T)> {
        self.held.drain(..).collect()
    }

    #[must_use]
    pub fn is_owed(&self, key: &OpKey) -> bool {
        self.owed.contains(key)
    }

    /// Where to page history for what is owed: each artifact, with the seq
    /// to page back from (one past its newest owed op).
    #[must_use]
    pub fn owed_pages(&self) -> Vec<(ArtifactId, u64)> {
        let mut pages: BTreeMap<uuid::Uuid, (ArtifactId, u64)> = BTreeMap::new();
        for key in &self.owed {
            let before = key.seq.saturating_add(1);
            pages
                .entry(key.artifact.0)
                .and_modify(|(_, seq)| *seq = (*seq).max(before))
                .or_insert((key.artifact, before));
        }
        pages.into_values().collect()
    }

    /// History has nothing older for `artifact`: what it never supplied,
    /// for the caller to log.
    pub fn give_up_owed(&mut self, artifact: ArtifactId) -> Vec<OpKey> {
        let given_up: Vec<OpKey> = self
            .owed
            .iter()
            .filter(|key| key.artifact == artifact)
            .copied()
            .collect();
        for key in &given_up {
            self.owed.remove(key);
        }
        given_up
    }

    /// The route is gone: forget everything held or owed for it.
    pub fn clear(&mut self) {
        self.held.clear();
        self.owed.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seq: u64) -> OpKey {
        OpKey {
            artifact: ArtifactId(uuid::Uuid::from_bytes([1; 16])),
            author: Ed25519PublicKey::from_bytes([2; 32]),
            seq,
        }
    }

    #[test]
    fn an_op_is_held_once_and_released_in_order() {
        let mut hold = InboundHold::new(4);
        hold.hold(key(1), "first");
        hold.hold(key(2), "second");
        assert_eq!(hold.hold(key(1), "again"), None);
        assert_eq!(hold.take(), [(key(1), "first"), (key(2), "second")]);
        assert!(hold.is_empty());
    }

    #[test]
    fn past_capacity_the_oldest_is_owed_by_history_until_it_supplies_it() {
        let mut hold = InboundHold::new(2);
        let owed: Vec<_> = (1..=4).filter_map(|seq| hold.hold(key(seq), seq)).collect();
        assert_eq!(owed, [key(1), key(2)]);
        assert_eq!(hold.len(), 2);
        assert!(hold.is_owed(&key(1)) && hold.is_owed(&key(2)));
        assert_eq!(hold.owed_pages(), [(key(1).artifact, 3)]);
        assert!(hold.settle(&key(1)));
        assert!(!hold.settle(&key(1)));
        assert_eq!(hold.give_up_owed(key(1).artifact), [key(2)]);
        assert!(hold.owed_pages().is_empty());
        assert_eq!(hold.take(), [(key(3), 3), (key(4), 4)]);
    }

    #[test]
    fn an_op_history_supplies_is_settled_once() {
        let mut hold = InboundHold::new(4);
        hold.hold(key(1), ());
        assert!(hold.settle(&key(1)));
        assert!(!hold.settle(&key(1)));
        assert!(hold.take().is_empty());
    }

    #[test]
    fn clearing_forgets_what_was_held_and_owed() {
        let mut hold = InboundHold::new(1);
        hold.hold(key(1), ());
        assert_eq!(hold.hold(key(2), ()), Some(key(1)));
        hold.clear();
        assert!(hold.is_empty() && hold.owed_pages().is_empty());
    }

    #[test]
    fn only_a_stated_verdict_decides_cleartext() {
        assert_eq!(OpAdmission::Unstated.admits_cleartext(), None);
        assert_eq!(
            OpAdmission::CleartextAdmitted.admits_cleartext(),
            Some(true)
        );
        assert_eq!(
            OpAdmission::EncryptionRequired.admits_cleartext(),
            Some(false)
        );
    }
}
