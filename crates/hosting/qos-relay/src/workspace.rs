//! Per-workspace runtime state: the pinned CRDT (if any), the set
//! of live subscribers, and the fan-out routing rule.
//!
//! `WorkspaceRuntime` is owned by the daemon and shared with the
//! connection tasks via `Arc<Mutex<_>>`. Each connection task, on
//! subscribe, registers a bounded `mpsc::Sender<Envelope>` here;
//! on receive-a-delta, the daemon iterates every subscriber ≠
//! author and calls `try_send`. A saturated subscriber is disconnected
//! and its durable traffic is queued for replay on reconnect. Peers that
//! are NOT connected but are members of the workspace get their delta
//! enqueued to disk.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use automerge::{AutoCommit, ChangeHash};
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
    /// Bounded outbound envelope channel. Closed or full means the peer can
    /// no longer keep up and is pruned immediately.
    pub tx: mpsc::Sender<Envelope>,
    /// Signals the owning peer task when this subscriber cannot keep up. A
    /// reconnect replays its durable queue instead of growing RAM without
    /// bound or leaving the connection silently stale.
    pub disconnect: mpsc::Sender<()>,
}

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// On-disk state.
    pub store: WorkspaceStore,
    /// Pinned CRDT — only `Some` when the config marks this
    /// workspace `pin = true`. Reloaded from disk on start and
    /// materialized forward as raw Automerge deltas arrive (see
    /// [`WorkspaceRuntime::materialize`]), so the host holds
    /// authoritative current state that [`WorkspaceRuntime::changes_since`]
    /// serves as the anti-entropy source of truth — independent of the
    /// per-peer pending queues.
    pub doc: Option<AutoCommit>,
    /// Which of the user's devices are workspace members. Used to
    /// gate subscribes AND to decide who gets store-and-forward
    /// on delta receive.
    pub membership: Membership,
    /// Whether any connection already admitted by the relay's trust policy may
    /// join this workspace. Enabled automatically for matching-user home hosts;
    /// shared/multi-tenant hosts use an explicit membership list.
    pub admit_authenticated: bool,
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
        let mut membership = membership;
        for device in store.load_membership()? {
            if !membership.contains(&device) {
                membership.devices.push(device);
            }
        }
        let doc = if pinned {
            match store.load_snapshot()? {
                Some(bytes) => Some(AutoCommit::load(&bytes).map_err(|e| {
                    crate::store::StoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("automerge load: {e}"),
                    ))
                })?),
                None => Some(AutoCommit::new()),
            }
        } else {
            None
        };
        Ok(Self {
            store,
            doc,
            membership,
            admit_authenticated: false,
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

    /// Remember an authenticated device so deltas received while it is offline
    /// are queued across daemon restarts.
    pub fn admit_device(&mut self, device: [u8; 32]) -> Result<(), crate::store::StoreError> {
        if !self.membership.contains(&device) {
            self.membership.devices.push(device);
            self.store.save_membership(&self.membership.devices)?;
        }
        Ok(())
    }

    /// Drop a subscription for `device`. Idempotent.
    pub fn remove_subscriber(&mut self, device: &[u8; 32]) {
        self.subscribers.remove(device);
    }

    /// Whether `device` currently holds a subscription here.
    pub fn has_subscriber(&self, device: &[u8; 32]) -> bool {
        self.subscribers.contains_key(device)
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
        for (peer, sub) in &self.subscribers {
            if peer == author {
                continue;
            }
            // Federation echo-suppression: a message originating on a
            // peer relay must not bounce onto other peer relays.
            if from_role == PeerRole::PeerRelay && sub.role == PeerRole::PeerRelay {
                continue;
            }
            if sub.tx.try_send(envelope.clone()).is_err() {
                let _ = sub.disconnect.try_send(());
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
        for member in &self.membership.devices {
            if member == author {
                continue;
            }
            if self.subscribers.contains_key(member) {
                continue;
            }
            let raw = crate::transport::encode(&envelope).map_err(|e| {
                crate::store::StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("encode: {e}"),
                ))
            })?;
            self.store.enqueue_pending(member, seq, &raw)?;
        }
        // 3. Store-and-forward for offline peer relays — reuses the
        //    same `store.enqueue_pending` path keyed by device pubkey.
        //    Suppressed when the origin is itself a peer relay (echo
        //    prevention across a chain of federated relays).
        if from_role == PeerRole::Device {
            for pr in &self.known_peer_relays {
                if pr == author {
                    continue;
                }
                if self.subscribers.contains_key(pr) {
                    continue;
                }
                let raw = crate::transport::encode(&envelope).map_err(|e| {
                    crate::store::StoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("encode: {e}"),
                    ))
                })?;
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
        // Keep a transport-level bootstrap independent of the membership
        // queue. A device can be invited while every existing device is
        // offline, so it may have no per-device pending directory yet.
        if let Some(payload) = crate::coedit::CoeditPayload::decode(raw_delta_bytes) {
            let encoded = crate::transport::encode(&envelope).map_err(|error| {
                crate::store::StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("encode retained envelope: {error}"),
                ))
            })?;
            match payload {
                crate::coedit::CoeditPayload::Checkpoint { channel, .. } => {
                    self.store.retain_checkpoint(&channel, &encoded)?;
                }
                crate::coedit::CoeditPayload::RetainedDelta { channel, .. } => {
                    self.store.retain_delta(&channel, seq, &encoded)?;
                }
                crate::coedit::CoeditPayload::Unsigned(_)
                | crate::coedit::CoeditPayload::Signed(_) => {}
            }
        }
        // 4. Materialize into the pinned doc so the host holds
        //    authoritative current state (the source for anti-entropy
        //    resync), not just per-peer queues. Runs AFTER fan-out +
        //    store-and-forward so it can never alter those semantics;
        //    best-effort and no-op when the workspace is unpinned or the
        //    bytes are not a raw Automerge delta (a co-edit payload
        //    wrapper, a non-CRDT test blob, …).
        self.materialize(raw_delta_bytes);
        Ok(delivered)
    }

    /// Apply an incoming raw Automerge delta into the pinned doc and
    /// persist the updated snapshot, so the host materializes
    /// authoritative current state rather than relying solely on the
    /// per-peer pending queues. Returns `true` when the doc advanced.
    ///
    /// Best-effort and fully isolated from routing:
    /// - runs only when the workspace is pinned (`doc.is_some()`);
    /// - a delta that is not a raw Automerge incremental (e.g. a co-edit
    ///   `CoeditPayload` wrapper, or a non-CRDT test payload) is applied
    ///   to a throwaway clone first, so a decode failure leaves the live
    ///   doc + snapshot untouched;
    /// - a delta that adds nothing new writes no snapshot.
    fn materialize(&mut self, delta_bytes: &[u8]) -> bool {
        if self.doc.is_none() || delta_bytes.is_empty() {
            return false;
        }
        // Apply to a clone so a malformed / non-Automerge payload cannot
        // half-apply and corrupt the authoritative doc.
        let mut candidate = self.doc.as_ref().expect("doc.is_some checked").clone();
        match candidate.load_incremental(delta_bytes) {
            Ok(n) if n > 0 => {
                let saved = candidate.save();
                self.doc = Some(candidate);
                // Persist; save_snapshot no-ops on an unpinned store, and
                // a write failure must not break routing (already done).
                let _ = self.store.save_snapshot(&saved);
                true
            }
            _ => false,
        }
    }

    /// The host's current Automerge heads for this workspace, as wire
    /// 32-byte hashes. Empty when the workspace is not materialized here
    /// (unpinned) — a requester then receives the whole document.
    pub fn current_heads(&mut self) -> Vec<[u8; 32]> {
        match self.doc.as_mut() {
            Some(doc) => doc.get_heads().into_iter().map(|h| h.0).collect(),
            None => Vec::new(),
        }
    }

    /// Anti-entropy: compute the changes a peer is missing — every
    /// change in the host's materialized doc that is not a (transitive)
    /// dependency of `have_heads`. Returns a concatenated series of
    /// Automerge changes loadable via `load_incremental`. Empty when the
    /// workspace is not materialized here, or the peer is already
    /// converged.
    pub fn changes_since(&mut self, have_heads: &[[u8; 32]]) -> Vec<u8> {
        let Some(doc) = self.doc.as_mut() else {
            return Vec::new();
        };
        let heads: Vec<ChangeHash> = have_heads.iter().copied().map(ChangeHash).collect();
        doc.save_after(&heads)
    }

    /// Ingest a batch of Automerge changes obtained via anti-entropy
    /// from an upstream peer relay: materialize them into the pinned
    /// doc, and — if they advanced the doc — fan the batch out to local
    /// device subscribers as a `Delta` so devices on THIS relay converge
    /// even when echo-suppression prevented the change from arriving via
    /// live fan-out (the middle hop of a relay chain). Returns `true`
    /// when new changes were ingested.
    ///
    /// Fanned out with a zero sentinel author + `PeerRole::PeerRelay`
    /// origin, so it reuses the same routing (offline members get
    /// store-and-forward; other peer relays are not re-echoed) as any
    /// federated delta — no new delivery path.
    pub fn ingest_sync_changes(
        &mut self,
        workspace_id: u64,
        changes: &[u8],
        seq: u64,
    ) -> Result<bool, crate::store::StoreError> {
        if !self.materialize(changes) {
            // Nothing new (empty, already-held, or non-Automerge bytes).
            return Ok(false);
        }
        let env = Envelope::Delta {
            workspace_id,
            author_device_public_key: [0u8; 32],
            authored_at_ns: seq,
            delta: changes.to_vec(),
            signature: [0u8; 64],
        };
        // fan-out re-materializes, but `changes` are now already present
        // so that inner call is a no-op (no second snapshot write).
        self.fanout_delta(&[0u8; 32], PeerRole::PeerRelay, env, changes, seq)?;
        Ok(true)
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
        for (peer, sub) in &self.subscribers {
            if peer == author {
                continue;
            }
            if from_role == PeerRole::PeerRelay && sub.role == PeerRole::PeerRelay {
                continue;
            }
            if sub.tx.try_send(envelope.clone()).is_err() {
                let _ = sub.disconnect.try_send(());
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
        let (tx1, _rx1) = mpsc::channel(8);
        let (tx2, mut rx2) = mpsc::channel(8);
        let (disconnect1, _disconnect_rx1) = mpsc::channel(1);
        let (disconnect2, _disconnect_rx2) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: [1u8; 32],
            role: PeerRole::Device,
            tx: tx1,
            disconnect: disconnect1,
        });
        rt.add_subscriber(Subscriber {
            device_public_key: [2u8; 32],
            role: PeerRole::Device,
            tx: tx2,
            disconnect: disconnect2,
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
        let mut rt = WorkspaceRuntime::open(tmp.path(), 5, false, Membership::default()).unwrap();
        let (tx, rx) = mpsc::channel(8);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        drop(rx); // channel closed immediately
        rt.add_subscriber(Subscriber {
            device_public_key: [9u8; 32],
            role: PeerRole::Device,
            tx,
            disconnect,
        });
        let env = env_delta(5, [1u8; 32]);
        let delivered = rt
            .fanout_delta(&[1u8; 32], PeerRole::Device, env, b"", 1)
            .unwrap();
        assert_eq!(delivered, 0);
        assert!(rt.subscribers.is_empty());
    }

    #[test]
    fn fanout_disconnects_saturated_subscriber_and_queues_durable_delta() {
        let tmp = tempfile::tempdir().unwrap();
        let author = [1u8; 32];
        let slow_peer = [2u8; 32];
        let mut membership = Membership::default();
        membership.set(vec![author, slow_peer]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 6, false, membership).unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let (disconnect, mut disconnect_rx) = mpsc::channel(1);
        tx.try_send(env_delta(6, [9u8; 32])).unwrap();
        rt.add_subscriber(Subscriber {
            device_public_key: slow_peer,
            role: PeerRole::Device,
            tx,
            disconnect,
        });

        let delivered = rt
            .fanout_delta(
                &author,
                PeerRole::Device,
                env_delta(6, author),
                b"durable",
                2,
            )
            .unwrap();

        assert_eq!(delivered, 0);
        assert!(!rt.subscribers.contains_key(&slow_peer));
        disconnect_rx
            .try_recv()
            .expect("slow peer task must be told to disconnect");
        assert_eq!(rt.store.count_pending(&slow_peer).unwrap(), 1);
    }

    #[test]
    fn fanout_from_peer_relay_skips_other_peer_relays() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 8, false, Membership::default()).unwrap();
        // A local device + two peer-relay subscribers.
        let (tx_dev, mut rx_dev) = mpsc::channel(8);
        let (tx_pr, mut rx_pr) = mpsc::channel(8);
        let (disconnect_dev, _disconnect_rx_dev) = mpsc::channel(1);
        let (disconnect_pr, _disconnect_rx_pr) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: [1u8; 32],
            role: PeerRole::Device,
            tx: tx_dev,
            disconnect: disconnect_dev,
        });
        rt.add_subscriber(Subscriber {
            device_public_key: [2u8; 32],
            role: PeerRole::PeerRelay,
            tx: tx_pr,
            disconnect: disconnect_pr,
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
        rt.fanout_delta(&author, PeerRole::Device, env, b"x", 42)
            .unwrap();
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
        rt.fanout_delta(&author, PeerRole::PeerRelay, env, b"x", 42)
            .unwrap();
        assert_eq!(rt.store.count_pending(&pr_pk).unwrap(), 0);
    }

    /// A pinned host materializes an incoming raw Automerge delta into
    /// its doc and can then serve it back via `changes_since` — the
    /// source of truth for anti-entropy. A fresh peer converges from
    /// that alone.
    #[test]
    fn materialize_pins_delta_and_serves_it_via_changes_since() {
        use automerge::transaction::Transactable;
        use automerge::{AutoCommit, ROOT};

        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 55, true, Membership::default()).unwrap();

        let mut doc_a = AutoCommit::new();
        doc_a.put(ROOT, "title", "hello").unwrap();
        let d1 = doc_a.save_incremental();

        let env = env_delta(55, [1u8; 32]);
        rt.fanout_delta(&[1u8; 32], PeerRole::Device, env, &d1, 1)
            .unwrap();

        // Host materialized the change — its heads match the author's.
        let host_heads: Vec<[u8; 32]> = doc_a.get_heads().into_iter().map(|h| h.0).collect();
        assert_eq!(rt.current_heads(), host_heads);
        // And it persisted a snapshot (durable across restart).
        assert!(rt.store.load_snapshot().unwrap().is_some());

        // A fresh peer with nothing converges purely from the host's
        // anti-entropy response.
        let delta_since = rt.changes_since(&[]);
        assert!(!delta_since.is_empty());
        let mut doc_peer = AutoCommit::new();
        doc_peer.load_incremental(&delta_since).unwrap();
        assert_eq!(doc_peer.get_heads(), doc_a.get_heads());
    }

    /// A non-Automerge payload (the byte-blob deltas some peers publish,
    /// or a co-edit wrapper) must not corrupt or advance the pinned doc,
    /// and must not write a snapshot — materialization is best-effort and
    /// isolated from routing.
    #[test]
    fn materialize_skips_non_automerge_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 56, true, Membership::default()).unwrap();
        let env = env_delta(56, [5u8; 32]);
        // fanout still succeeds (routing is unaffected)...
        rt.fanout_delta(&[5u8; 32], PeerRole::Device, env, b"not-automerge", 1)
            .unwrap();
        // ...but the doc did not advance and no snapshot was written.
        assert!(rt.current_heads().is_empty());
        assert!(rt.store.load_snapshot().unwrap().is_none());
        assert!(rt.changes_since(&[]).is_empty());
    }

    /// The previously-unrecoverable disconnect race, at the runtime
    /// level: a delta fanned out to a *live* subscriber whose channel
    /// then dies before the write lands. Because the subscriber was
    /// live, the delta is counted delivered and is NEVER written to its
    /// pending queue — so the store-and-forward tier has nothing to
    /// recover from. Anti-entropy against the host's materialized doc
    /// reconciles the peer to convergence anyway.
    ///
    /// This exercises the real `WorkspaceRuntime` (`fanout_delta`,
    /// `changes_since`, the production mpsc subscriber channel) — no
    /// mock of the code under test.
    #[test]
    fn anti_entropy_recovers_delta_lost_in_half_closed_channel_race() {
        use automerge::transaction::Transactable;
        use automerge::{AutoCommit, ROOT};

        let tmp = tempfile::tempdir().unwrap();
        let a = [1u8; 32];
        let b = [2u8; 32];
        let mut membership = Membership::default();
        membership.set(vec![a, b]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 77, true, membership).unwrap();

        // B is a LIVE subscriber — exactly what `run_peer` registers.
        let (tx_b, rx_b) = mpsc::channel(8);
        let (disconnect_b, _disconnect_rx_b) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: b,
            role: PeerRole::Device,
            tx: tx_b,
            disconnect: disconnect_b,
        });

        // A authors a genuine Automerge edit and publishes it.
        let mut doc_a = AutoCommit::new();
        doc_a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
        doc_a.put(ROOT, "title", "shared").unwrap();
        let d1 = doc_a.save_incremental();
        let env = Envelope::Delta {
            workspace_id: 77,
            author_device_public_key: a,
            authored_at_ns: 1,
            delta: d1.clone(),
            signature: [0u8; 64],
        };
        let delivered = rt
            .fanout_delta(&a, PeerRole::Device, env, &d1, 111)
            .unwrap();
        assert_eq!(delivered, 1, "delta fanned out to the live subscriber B");

        // The gap: a delivered delta is NOT queued, so the pending tier
        // holds nothing to recover B from.
        assert_eq!(
            rt.store.count_pending(&b).unwrap(),
            0,
            "a delivered delta is never written to the pending queue"
        );

        // The genuine disconnect race: B's wire dies before the outbound
        // channel drained, so B never applied d1. Dropping the receiver
        // models the dead wire — the fanned-out delta is simply lost.
        drop(rx_b);

        // Pre-anti-entropy: B (fresh, empty) is divergent and has no
        // queue entry — the previously-unrecoverable state.
        let mut doc_b = AutoCommit::new();
        doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
        assert_ne!(doc_b.get_heads(), doc_a.get_heads(), "B starts divergent");

        // ---- Anti-entropy against the host's materialized doc. ----
        let have: Vec<[u8; 32]> = doc_b.get_heads().into_iter().map(|h| h.0).collect();
        let delta_since = rt.changes_since(&have);
        assert!(
            !delta_since.is_empty(),
            "host serves the lost change from its materialized doc"
        );
        doc_b.load_incremental(&delta_since).unwrap();

        assert_eq!(
            doc_b.get_heads(),
            doc_a.get_heads(),
            "anti-entropy reconciles B to convergence despite the lost fan-out and empty queue"
        );
    }
}
