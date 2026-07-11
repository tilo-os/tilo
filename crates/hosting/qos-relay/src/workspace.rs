//! Per-workspace runtime state: the pinned CRDT (if any), the set
//! of live subscribers, and the fan-out routing rule.
//!
//! `WorkspaceRuntime` is owned by the daemon and shared with the
//! connection tasks via `Arc<Mutex<_>>`. Each connection task, on
//! subscribe, registers a `mpsc::UnboundedSender<Envelope>` here;
//! on receive-a-delta, the daemon iterates every subscriber ≠
//! author and calls `send`. Peers that are NOT connected but are
//! members of the workspace get their delta enqueued to disk.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use automerge::AutoCommit;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::PeerRole;
use crate::store::{LogEntry, WorkspaceStore};
use crate::transport::Envelope;

/// Membership set for a workspace on this relay. Populated by the
/// admin (`bind-workspace`) via the workspace-directory CRDT and
/// refreshed whenever the directory reports a change. Peers listed
/// here get store-and-forward; peers not listed get their subscribe
/// rejected with `NotAuthorised`.
#[derive(Clone, Debug, Default)]
pub struct Membership {
    /// Device public keys that are known members of this workspace.
    pub devices: Vec<[u8; 32]>,
}

impl Membership {
    /// Is `device` a member of this workspace?
    pub fn contains(&self, device: &[u8; 32]) -> bool {
        self.devices.contains(device)
    }

    /// Replace the membership with a new set.
    pub fn set(&mut self, devices: Vec<[u8; 32]>) {
        self.devices = devices;
    }
}

/// One live subscription — the outbound envelope channel a
/// connection task registers.
pub struct Subscriber {
    /// Peer device pubkey — routing key. Uniquely identifies the
    /// subscriber; if a peer opens a second connection its
    /// pubkey collides and the older subscription is replaced.
    pub device_public_key: [u8; 32],
    /// Role of the subscriber. A `PeerRelay` subscriber is treated
    /// as a federated relay — the fan-out rule skips it when the
    /// author is itself a `PeerRelay`, to prevent echo loops.
    pub role: PeerRole,
    /// Outbound envelope channel. `send_err` = disconnected; the
    /// runtime prunes on next fan-out.
    pub tx: mpsc::UnboundedSender<Envelope>,
}

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// On-disk state.
    pub store: WorkspaceStore,
    /// Pinned CRDT — only `Some` when the config marks this
    /// workspace `pin = true`. Reloaded from disk on start.
    pub doc: Option<AutoCommit>,
    /// Which of the user's devices are workspace members. Used to
    /// gate subscribes AND to decide who gets store-and-forward
    /// on delta receive.
    pub membership: Membership,
    /// Current live subscribers keyed by device pubkey.
    pub subscribers: HashMap<[u8; 32], Subscriber>,
    /// Device pubkeys of peer relays this workspace has federated
    /// with at least once since the daemon started. Ops arriving
    /// while such a relay is offline are queued via the same
    /// store-and-forward path as offline devices — reuses
    /// `store.enqueue_pending` verbatim; no new persistence code.
    pub known_peer_relays: HashSet<[u8; 32]>,
}

impl WorkspaceRuntime {
    /// Build a runtime for a bound workspace. Loads the snapshot
    /// from disk if pinned + present.
    pub fn open(
        state_root: &std::path::Path,
        workspace_id: u64,
        pinned: bool,
        membership: Membership,
    ) -> Result<Self, crate::store::StoreError> {
        let store = WorkspaceStore::open(state_root, workspace_id, pinned)?;
        let doc = if pinned {
            match store.load_snapshot()? {
                Some(bytes) => Some(
                    AutoCommit::load(&bytes).map_err(|e| {
                        crate::store::StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("automerge load: {e}"),
                        ))
                    })?,
                ),
                None => Some(AutoCommit::new()),
            }
        } else {
            None
        };
        Ok(Self {
            store,
            doc,
            membership,
            subscribers: HashMap::new(),
            known_peer_relays: HashSet::new(),
        })
    }

    /// Note that `device` is a peer relay we federate with for this
    /// workspace. Idempotent; called on every `PeerRelayHello` that
    /// mentions this workspace so subsequent offline arrivals are
    /// queued via the existing per-peer pending path.
    pub fn record_peer_relay(&mut self, device: [u8; 32]) {
        self.known_peer_relays.insert(device);
    }

    /// Register a subscription. Replaces any prior subscription
    /// from the same device.
    pub fn add_subscriber(&mut self, sub: Subscriber) {
        self.subscribers.insert(sub.device_public_key, sub);
    }

    /// Drop a subscription for `device`. Idempotent.
    pub fn remove_subscriber(&mut self, device: &[u8; 32]) {
        self.subscribers.remove(device);
    }

    /// Route a delta envelope to every subscriber that is NOT the
    /// author, AND enqueue for offline members. Returns how many
    /// live subscribers were fanned out to.
    ///
    /// `from_role` is the role of the sending peer. A `PeerRelay`
    /// origin is not re-forwarded to other peer relays — that filter
    /// is the ONE line federation adds to the fan-out; the rest of
    /// the routing (author exclusion, live subscriber loop, offline
    /// store-and-forward via `store.enqueue_pending`) is unchanged.
    pub fn fanout_delta(
        &mut self,
        author: &[u8; 32],
        from_role: PeerRole,
        envelope: Envelope,
        raw_delta_bytes: &[u8],
        seq: u64,
    ) -> Result<usize, crate::store::StoreError> {
        // 1. Live fan-out — prune closed subscribers as we go.
        let mut delivered = 0usize;
        let mut dead: Vec<[u8; 32]> = Vec::new();
        for (peer, sub) in self.subscribers.iter() {
            if peer == author {
                continue;
            }
            // Federation echo-suppression: a message originating on a
            // peer relay must not bounce onto other peer relays.
            if from_role == PeerRole::PeerRelay && sub.role == PeerRole::PeerRelay {
                continue;
            }
            if sub.tx.send(envelope.clone()).is_err() {
                dead.push(*peer);
            } else {
                delivered += 1;
            }
        }
        for d in dead {
            self.subscribers.remove(&d);
        }
        // 2. Store-and-forward for offline members. A member is any
        //    pubkey in `membership` that is not the author and not
        //    currently subscribed.
        for member in self.membership.devices.iter() {
            if member == author {
                continue;
            }
            if self.subscribers.contains_key(member) {
                continue;
            }
            let raw = crate::transport::encode(&envelope)
                .map_err(|e| crate::store::StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("encode: {e}"),
                )))?;
            self.store.enqueue_pending(member, seq, &raw)?;
        }
        // 3. Store-and-forward for offline peer relays — reuses the
        //    same `store.enqueue_pending` path keyed by device pubkey.
        //    Suppressed when the origin is itself a peer relay (echo
        //    prevention across a chain of federated relays).
        if from_role == PeerRole::Device {
            for pr in self.known_peer_relays.iter() {
                if pr == author {
                    continue;
                }
                if self.subscribers.contains_key(pr) {
                    continue;
                }
                let raw = crate::transport::encode(&envelope)
                    .map_err(|e| crate::store::StoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("encode: {e}"),
                    )))?;
                self.store.enqueue_pending(pr, seq, &raw)?;
            }
        }
        // 3. Audit log — best-effort; failure does not block routing.
        let _ = self.store.append_log(&LogEntry {
            workspace_id: self.store.workspace_id(),
            received_at_ns: seq,
            author_device_hex: hex::encode(author),
            authored_at_ns: seq,
            delta_len: raw_delta_bytes.len() as u64,
        });
        Ok(delivered)
    }

    /// Route a presence envelope (no persistence, drop-on-empty).
    /// Same echo-suppression rule as `fanout_delta`.
    pub fn fanout_presence(
        &mut self,
        author: &[u8; 32],
        from_role: PeerRole,
        envelope: Envelope,
    ) -> usize {
        let mut delivered = 0usize;
        let mut dead: Vec<[u8; 32]> = Vec::new();
        for (peer, sub) in self.subscribers.iter() {
            if peer == author {
                continue;
            }
            if from_role == PeerRole::PeerRelay && sub.role == PeerRole::PeerRelay {
                continue;
            }
            if sub.tx.send(envelope.clone()).is_err() {
                dead.push(*peer);
            } else {
                delivered += 1;
            }
        }
        for d in dead {
            self.subscribers.remove(&d);
        }
        delivered
    }
}

/// Shorthand: a workspace runtime held under an async mutex.
pub type SharedWorkspace = Arc<AsyncMutex<WorkspaceRuntime>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Envelope;

    fn env_delta(id: u64, author: [u8; 32]) -> Envelope {
        Envelope::Delta {
            workspace_id: id,
            author_device_public_key: author,
            authored_at_ns: 100,
            delta: vec![1, 2, 3],
            signature: [7u8; 64],
        }
    }

    #[test]
    fn fanout_excludes_author_and_enqueues_offline() {
        let tmp = tempfile::tempdir().unwrap();
        let mut membership = Membership::default();
        membership.set(vec![[1u8; 32], [2u8; 32], [3u8; 32]]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 42, false, membership).unwrap();
        // Peer 2 is live; peer 1 (author) is live too but should be
        // excluded; peer 3 is offline → gets store-and-forward.
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        rt.add_subscriber(Subscriber {
            device_public_key: [1u8; 32],
            role: PeerRole::Device,
            tx: tx1,
        });
        rt.add_subscriber(Subscriber {
            device_public_key: [2u8; 32],
            role: PeerRole::Device,
            tx: tx2,
        });
        let env = env_delta(42, [1u8; 32]);
        let delivered = rt
            .fanout_delta(&[1u8; 32], PeerRole::Device, env.clone(), b"1 2 3", 111)
            .unwrap();
        assert_eq!(delivered, 1);
        // Peer 2 saw the delta live.
        assert_eq!(rx2.try_recv().unwrap(), env);
        // Peer 3 has one pending delta on disk.
        let peer3 = [3u8; 32];
        assert_eq!(rt.store.count_pending(&peer3).unwrap(), 1);
    }

    #[test]
    fn fanout_prunes_dead_subscribers() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(
            tmp.path(),
            5,
            false,
            Membership::default(),
        )
        .unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx); // channel closed immediately
        rt.add_subscriber(Subscriber {
            device_public_key: [9u8; 32],
            role: PeerRole::Device,
            tx,
        });
        let env = env_delta(5, [1u8; 32]);
        let delivered = rt
            .fanout_delta(&[1u8; 32], PeerRole::Device, env, b"", 1)
            .unwrap();
        assert_eq!(delivered, 0);
        assert!(rt.subscribers.is_empty());
    }

    #[test]
    fn fanout_from_peer_relay_skips_other_peer_relays() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 8, false, Membership::default()).unwrap();
        // A local device + two peer-relay subscribers.
        let (tx_dev, mut rx_dev) = mpsc::unbounded_channel();
        let (tx_pr, mut rx_pr) = mpsc::unbounded_channel();
        rt.add_subscriber(Subscriber {
            device_public_key: [1u8; 32],
            role: PeerRole::Device,
            tx: tx_dev,
        });
        rt.add_subscriber(Subscriber {
            device_public_key: [2u8; 32],
            role: PeerRole::PeerRelay,
            tx: tx_pr,
        });
        // The originating peer relay (author, offline here).
        let author = [3u8; 32];
        let env = env_delta(8, author);
        let delivered = rt
            .fanout_delta(&author, PeerRole::PeerRelay, env.clone(), b"x", 1)
            .unwrap();
        // Only the local device saw it.
        assert_eq!(delivered, 1);
        assert_eq!(rx_dev.try_recv().unwrap(), env);
        assert!(rx_pr.try_recv().is_err(), "peer relay must not echo");
    }

    #[test]
    fn known_peer_relay_gets_pending_when_offline_and_origin_is_device() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 12, false, Membership::default()).unwrap();
        let pr_pk = [7u8; 32];
        rt.record_peer_relay(pr_pk);
        // A local device authors — pr is offline, should be queued.
        let author = [1u8; 32];
        let env = env_delta(12, author);
        rt.fanout_delta(&author, PeerRole::Device, env, b"x", 42).unwrap();
        assert_eq!(rt.store.count_pending(&pr_pk).unwrap(), 1);
    }

    #[test]
    fn known_peer_relay_pending_suppressed_when_origin_is_peer_relay() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 13, false, Membership::default()).unwrap();
        let pr_pk = [7u8; 32];
        rt.record_peer_relay(pr_pk);
        // Another peer relay is the origin — offline pr must NOT be
        // queued (that's the echo across a chain of relays).
        let author = [2u8; 32];
        let env = env_delta(13, author);
        rt.fanout_delta(&author, PeerRole::PeerRelay, env, b"x", 42).unwrap();
        assert_eq!(rt.store.count_pending(&pr_pk).unwrap(), 0);
    }
}
