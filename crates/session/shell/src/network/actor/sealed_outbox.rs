//! This device's sealed writes to a room whose records are sealed channels,
//! held until a host of the room keeps them.
//!
//! The room's governor cannot open a sealed write and never admits it, so
//! the owner's outgoing custody, which the governor's admission retires,
//! does not carry it. A host retains it for the record's channel members
//! instead, and says when it did: the write goes to the host as a commit
//! ([`RelayCoeditSession::commit`]), the host answers once it is durable,
//! and only that answer lets the write go here. Every session of the room
//! that starts sends what is still held, so a write sent into a session that
//! ended before its host kept it, one that died unnoticed included, reaches
//! the room's next host. A write's commit id is the hash of its bytes: a
//! host keeps a write it is sent twice once, and says so both times.
//!
//! [`RelayCoeditSession::commit`]: tilo_relay_session::RelayCoeditSession::commit

use std::collections::VecDeque;

use neighbour_wire::Retention;
use tilo_relay_session::CoeditPayload;

/// Most writes one room holds for its hosts.
const MAX_HELD: usize = 4096;
/// Most bytes of writes one room holds for its hosts.
const MAX_HELD_BYTES: usize = 32 * 1024 * 1024;

struct Held {
    op: [u8; 16],
    channel: [u8; 32],
    retention: Retention,
    bytes: Vec<u8>,
}

impl Held {
    fn payload(&self) -> CoeditPayload {
        let (channel, body) = (self.channel, self.bytes.clone());
        match self.retention {
            Retention::Checkpoint => CoeditPayload::Checkpoint { channel, body },
            Retention::Delta => CoeditPayload::RetainedDelta { channel, body },
        }
    }
}

/// One room's sealed writes no host has said it keeps yet, oldest first.
#[derive(Default)]
pub(super) struct SealedOutbox {
    held: VecDeque<Held>,
    bytes: usize,
}

impl SealedOutbox {
    /// Hold `bytes`, a sealed write retained on `channel` as `retention`,
    /// until a host keeps it, and answer its commit: what to send a session
    /// up now. A write already held is not held twice.
    pub(super) fn hold(
        &mut self,
        channel: [u8; 32],
        retention: Retention,
        bytes: Vec<u8>,
    ) -> ([u8; 16], CoeditPayload) {
        let op = commit_id(&bytes);
        if let Some(held) = self.held.iter().find(|held| held.op == op) {
            return (op, held.payload());
        }
        let held = Held {
            op,
            channel,
            retention,
            bytes,
        };
        let payload = held.payload();
        self.bytes = self.bytes.saturating_add(held.bytes.len());
        self.held.push_back(held);
        while self.held.len() > MAX_HELD || self.bytes > MAX_HELD_BYTES {
            let Some(dropped) = self.held.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(dropped.bytes.len());
            tracing::warn!(
                "sealed writes no host has kept overflowed what a room holds; the oldest is let go"
            );
        }
        (op, payload)
    }

    /// A host keeps commit `op`: it is let go. Whether it was held.
    pub(super) fn kept(&mut self, op: [u8; 16]) -> bool {
        let Some(index) = self.held.iter().position(|held| held.op == op) else {
            return false;
        };
        if let Some(held) = self.held.remove(index) {
            self.bytes = self.bytes.saturating_sub(held.bytes.len());
        }
        true
    }

    /// Every write still held, as the commit a session that starts sends,
    /// oldest first.
    pub(super) fn commits(&self) -> impl Iterator<Item = ([u8; 16], CoeditPayload)> + '_ {
        self.held.iter().map(|held| (held.op, held.payload()))
    }

    /// Whether every write was kept by a host.
    pub(super) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

/// The commit id of a sealed write: its bytes' hash, the same on every send.
fn commit_id(bytes: &[u8]) -> [u8; 16] {
    let mut op = [0; 16];
    op.copy_from_slice(&blake3::hash(bytes).as_bytes()[..16]);
    op
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(outbox: &SealedOutbox) -> Vec<[u8; 16]> {
        outbox.commits().map(|(op, _)| op).collect()
    }

    #[test]
    fn a_sealed_write_is_held_until_a_host_keeps_it() {
        let mut outbox = SealedOutbox::default();
        let (op, payload) = outbox.hold([1; 32], Retention::Delta, b"edit".to_vec());
        assert_eq!(
            payload,
            CoeditPayload::RetainedDelta {
                channel: [1; 32],
                body: b"edit".to_vec()
            }
        );
        assert_eq!(sent(&outbox), [op], "sent, but not kept yet");
        assert!(
            !outbox.kept([9; 16]),
            "another write's answer lets nothing go"
        );
        assert_eq!(sent(&outbox), [op]);
        assert!(outbox.kept(op));
        assert!(outbox.is_empty(), "kept by a host, it is let go");
        assert!(!outbox.kept(op), "a second answer finds nothing");
    }

    #[test]
    fn a_session_that_starts_sends_every_write_still_held_in_order() {
        let mut outbox = SealedOutbox::default();
        let (first, _) = outbox.hold([1; 32], Retention::Delta, b"first".to_vec());
        let (second, _) = outbox.hold([1; 32], Retention::Checkpoint, b"second".to_vec());
        let (third, _) = outbox.hold([2; 32], Retention::Delta, b"third".to_vec());
        // The session they went out on ends; the host kept only the second.
        assert!(outbox.kept(second));
        // The next session sends what is still held, as the same commits.
        let resent: Vec<_> = outbox.commits().collect();
        assert_eq!(
            resent,
            [
                (
                    first,
                    CoeditPayload::RetainedDelta {
                        channel: [1; 32],
                        body: b"first".to_vec()
                    }
                ),
                (
                    third,
                    CoeditPayload::RetainedDelta {
                        channel: [2; 32],
                        body: b"third".to_vec()
                    }
                ),
            ]
        );
        // And again on the session after, until a host keeps them.
        assert_eq!(sent(&outbox), [first, third]);
        assert!(outbox.kept(first) && outbox.kept(third));
        assert!(outbox.is_empty());
    }

    #[test]
    fn a_write_held_twice_is_one_commit() {
        let mut outbox = SealedOutbox::default();
        let (op, _) = outbox.hold([1; 32], Retention::Delta, b"edit".to_vec());
        let (again, _) = outbox.hold([1; 32], Retention::Delta, b"edit".to_vec());
        assert_eq!(op, again, "the same bytes are the same commit");
        assert_eq!(sent(&outbox), [op]);
        assert!(outbox.kept(op));
        assert!(outbox.is_empty());
    }
}
