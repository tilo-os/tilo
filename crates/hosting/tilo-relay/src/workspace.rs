//! Per-workspace runtime state: the pinned CRDT (if any), the set
//! of live subscribers, and the fan-out routing rule.
//!
//! `WorkspaceRuntime` is owned by the daemon and shared with the
//! connection tasks via `Arc<Mutex<_>>`. Each connection task, on
//! subscribe, registers a bounded `mpsc::Sender<OutboundFrame>` here;
//! on receive-a-delta, the daemon encodes the envelope once and hands the
//! same shared bytes to every subscriber ≠ author with `try_send`. A
//! saturated subscriber is disconnected and its durable traffic is replayed
//! on reconnect. Offline members get non-retained deltas enqueued to disk;
//! retained document traffic (checkpoints and their tails) is stored once
//! per channel and replayed to every subscriber, never copied per member.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tilo_automerge::ApplyIncoming;

use automerge::{AutoCommit, ChangeHash};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::PeerRole;
use crate::store::{LogEntry, MembershipStamp, WorkspaceStore};
use crate::transport::Envelope;

/// One framed, encoded envelope as written to the wire. Shared by every
/// subscriber it is fanned out to, so a delta costs one encode and one
/// allocation however many members receive it.
pub type OutboundFrame = Arc<[u8]>;

/// Encode `envelope` once into a shareable wire frame.
///
/// # Errors
///
/// Returns an error if postcard cannot serialize the envelope.
pub fn encode_frame(envelope: &Envelope) -> Result<OutboundFrame, postcard::Error> {
    crate::frame::encode(envelope).map(Arc::from)
}

use crate::store::AUTOMERGE_MAGIC;

/// Membership set for a workspace on this relay. Populated by the
/// admin (`bind-workspace`) via the workspace-directory CRDT and
/// refreshed whenever the directory reports a change. Peers listed
/// here get store-and-forward; peers not listed get their subscribe
/// rejected with `NotAuthorised`.
#[derive(Clone, Debug, Default)]
pub struct Membership {
    /// Device public keys in admission order (the durable file's order).
    devices: Vec<[u8; 32]>,
    /// The same keys, for constant-time checks in rooms with thousands of
    /// member devices.
    index: HashSet<[u8; 32]>,
}

impl Membership {
    /// Is `device` a member of this workspace?
    pub fn contains(&self, device: &[u8; 32]) -> bool {
        self.index.contains(device)
    }

    /// Replace the membership with a new set.
    pub fn set(&mut self, devices: Vec<[u8; 32]>) {
        self.index = devices.iter().copied().collect();
        self.devices = devices;
    }

    /// Add `device`; returns whether it was new.
    pub fn insert(&mut self, device: [u8; 32]) -> bool {
        if !self.index.insert(device) {
            return false;
        }
        self.devices.push(device);
        true
    }

    /// Member device keys in admission order.
    pub fn devices(&self) -> &[[u8; 32]] {
        &self.devices
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
    /// Bounded outbound frame channel. Closed or full means the peer can
    /// no longer keep up and is pruned immediately.
    pub tx: mpsc::Sender<OutboundFrame>,
    /// Signals the owning peer task when this subscriber cannot keep up. A
    /// reconnect replays its durable queue instead of growing RAM without
    /// bound or leaving the connection silently stale.
    pub disconnect: mpsc::Sender<()>,
}

/// What one [`WorkspaceRuntime::fanout_delta`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fanout {
    /// Live subscribers the frame was handed to.
    pub delivered: usize,
    /// Per-device pending entries written for offline members and relays.
    pub queued: usize,
}

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// On-disk state.
    pub store: WorkspaceStore,
    /// Pinned CRDT — only `Some` when the config marks this
    /// workspace `pin = true`. Reloaded from disk on start and
    /// materialized forward as raw Automerge deltas arrive (see
    /// `WorkspaceRuntime::materialize`), so the host holds
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
    /// Cross-user roots admitted by the signed authority for this exact room.
    pub admitted_users: HashSet<[u8; 32]>,
    /// Current live subscribers keyed by device pubkey.
    pub subscribers: HashMap<[u8; 32], Subscriber>,
    /// Device pubkeys of peer relays this workspace has federated
    /// with at least once since the daemon started. Ops arriving
    /// while such a relay is offline are queued via the same
    /// store-and-forward path as offline devices — reuses
    /// `store.enqueue_pending` verbatim; no new persistence code.
    pub known_peer_relays: HashSet<[u8; 32]>,
    /// Ephemeral eligible workers plus durable exact-route job bindings.
    pub compute: crate::compute_transport::ComputeTransportState,
    /// The membership file as last loaded, so the per-second access audit
    /// of every subscriber re-parses it only after it actually changed.
    membership_stamp: Option<MembershipStamp>,
    /// Last store sequence handed out; see [`Self::next_seq`].
    last_seq: u64,
    /// What is retained per document channel; see [`RetainedTail`].
    tails: HashMap<[u8; 32], RetainedTail>,
    /// When a retained tail asks for a checkpoint and when it disconnects.
    pub tail_bounds: TailBounds,
}

/// Bounds on one channel's retained tail.
///
/// A tail is trimmed only from its front, and only while each entry is
/// named by a newer checkpoint's [`Coverage::Entries`] — what its publisher
/// read from the bytes it saved. Dropping any other entry would hand a
/// joiner changes whose dependencies never arrive. Past the soft bound the
/// relay asks a connected member for a fresh checkpoint; past the hard bound
/// it disconnects the connected member that retained the most bytes, whose
/// reconnect publishes a checkpoint.
///
/// [`Coverage::Entries`]: crate::coedit::Coverage::Entries
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TailBounds {
    /// Entries after which a checkpoint is requested.
    pub soft_entries: usize,
    /// Bytes after which a checkpoint is requested.
    pub soft_bytes: u64,
    /// Entries after which the heaviest connected sender is disconnected.
    pub hard_entries: usize,
    /// Bytes after which the heaviest connected sender is disconnected.
    pub hard_bytes: u64,
    /// Least time between two checkpoint requests for one channel.
    pub request_interval: std::time::Duration,
}

impl Default for TailBounds {
    fn default() -> Self {
        Self {
            soft_entries: 16_384,
            soft_bytes: 256 * 1024 * 1024,
            hard_entries: 65_536,
            hard_bytes: 1024 * 1024 * 1024,
            request_interval: std::time::Duration::from_secs(30),
        }
    }
}

/// One retained tail entry, as accounted in memory.
#[derive(Clone, Copy, Debug)]
struct TailEntry {
    seq: u64,
    bytes: u64,
    author: [u8; 32],
    /// The publisher's identity for this message.
    entry: [u8; 32],
}

/// The retained base checkpoint of a channel.
#[derive(Clone, Copy, Debug)]
struct RetainedBase {
    entry: [u8; 32],
    /// Digest of the body. Members re-publish their whole document whenever
    /// they attach; a byte-identical republish is not fanned out or
    /// retained again.
    digest: [u8; 32],
}

/// The retained state of one document channel: a base checkpoint plus the
/// tail after it, oldest first.
#[derive(Debug, Default)]
struct RetainedTail {
    entries: std::collections::VecDeque<TailEntry>,
    bytes: u64,
    base: Option<RetainedBase>,
    last_request: Option<std::time::Instant>,
}

impl RetainedTail {
    fn push(&mut self, entry: TailEntry) {
        self.bytes = self.bytes.saturating_add(entry.bytes);
        self.entries.push_back(entry);
    }

    /// Forget front entries while `covered` names them; returns the
    /// sequence of the last one forgotten.
    fn trim_front(&mut self, covered: &HashSet<[u8; 32]>) -> Option<u64> {
        let mut through = None;
        while let Some(front) = self.entries.front().copied() {
            if !covered.contains(&front.entry) {
                break;
            }
            self.entries.pop_front();
            self.bytes = self.bytes.saturating_sub(front.bytes);
            through = Some(front.seq);
        }
        through
    }
}

/// What an arriving checkpoint does to its channel's retained state.
struct CheckpointPlan {
    base: RetainedBase,
    /// Whether it may replace the base: there is none, it supersedes
    /// everything, or it names the base as covered.
    replaces_base: bool,
    /// The entries it names as covered; `None` for a whole-state baseline.
    covered: Option<HashSet<[u8; 32]>>,
}

/// A retained envelope as the tail accounts it.
struct RetainedParts {
    author: [u8; 32],
    entry: [u8; 32],
    /// Body digest, for a checkpoint.
    checkpoint_digest: Option<[u8; 32]>,
}

fn retained_frame_parts(frame: &[u8]) -> Option<RetainedParts> {
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = crate::frame::decode(frame.get(4..)?).ok()?
    else {
        return None;
    };
    Some(match crate::coedit::CoeditPayload::decode(&delta)? {
        crate::coedit::CoeditPayload::Checkpoint { entry, body, .. } => RetainedParts {
            author: author_device_public_key,
            entry,
            checkpoint_digest: Some(*blake3::hash(&body).as_bytes()),
        },
        crate::coedit::CoeditPayload::RetainedDelta { entry, .. } => RetainedParts {
            author: author_device_public_key,
            entry,
            checkpoint_digest: None,
        },
    })
}

impl WorkspaceRuntime {
    /// Build a runtime for a bound workspace. Loads the snapshot
    /// from disk if pinned + present.
    ///
    /// # Errors
    ///
    /// Returns an error if the on-disk store can't be opened, the
    /// durable membership file can't be read, or a pinned snapshot
    /// exists but fails to load as an Automerge document.
    pub fn open(
        state_root: &std::path::Path,
        workspace_id: u64,
        pinned: bool,
        membership: Membership,
    ) -> Result<Self, crate::store::StoreError> {
        let store = WorkspaceStore::open(state_root, workspace_id, pinned)?;
        let mut membership = membership;
        for device in store.load_membership()? {
            membership.insert(device);
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
        let mut last_seq = 0u64;
        let mut tails = HashMap::new();
        for retained in store.load_retained_channels()? {
            let mut tail = RetainedTail {
                base: retained
                    .checkpoint
                    .as_deref()
                    .and_then(retained_frame_parts)
                    .and_then(|parts| {
                        Some(RetainedBase {
                            entry: parts.entry,
                            digest: parts.checkpoint_digest?,
                        })
                    }),
                ..RetainedTail::default()
            };
            for (seq, frame) in retained.tail {
                last_seq = last_seq.max(seq);
                let parts = retained_frame_parts(&frame);
                tail.push(TailEntry {
                    seq,
                    bytes: frame.len() as u64,
                    author: parts.as_ref().map_or([0; 32], |parts| parts.author),
                    entry: parts.as_ref().map_or([0; 32], |parts| parts.entry),
                });
            }
            tails.insert(retained.channel, tail);
        }
        Ok(Self {
            store,
            doc,
            membership,
            admit_authenticated: false,
            admitted_users: HashSet::new(),
            subscribers: HashMap::new(),
            known_peer_relays: HashSet::new(),
            compute: crate::compute_transport::ComputeTransportState::default(),
            membership_stamp: None,
            last_seq,
            tails,
            tail_bounds: TailBounds::default(),
        })
    }

    /// Reload membership from its durable file when (and only when) the file
    /// changed since the last load. Admin tools and admission rewrite it by
    /// atomic rename, so the stamp changes with every rewrite.
    ///
    /// # Errors
    ///
    /// Returns an error if the file's metadata or contents can't be read.
    pub fn refresh_membership(&mut self) -> Result<(), crate::store::StoreError> {
        let stamp = self.store.membership_stamp()?;
        if stamp.is_some() && stamp == self.membership_stamp {
            return Ok(());
        }
        let devices = self.store.load_membership()?;
        self.membership.set(devices);
        self.membership_stamp = stamp;
        Ok(())
    }

    /// A store sequence strictly greater than every earlier one in this
    /// workspace, taken from the relay clock `now_ns` when it has advanced.
    /// Two deltas received in the same nanosecond therefore never share a
    /// pending or retained file name.
    pub fn next_seq(&mut self, now_ns: u64) -> u64 {
        self.last_seq = now_ns.max(self.last_seq.saturating_add(1));
        self.last_seq
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
    ///
    /// # Errors
    ///
    /// Returns an error if persisting the updated membership list fails.
    pub fn admit_device(&mut self, device: [u8; 32]) -> Result<(), crate::store::StoreError> {
        if self.membership.insert(device) {
            self.store.save_membership(self.membership.devices())?;
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
    /// author, AND make it durable: retained document traffic is stored
    /// once per channel, everything else is enqueued per offline member.
    /// Returns how many live subscribers took it and how many per-device
    /// queue entries were written.
    ///
    /// `from_role` is the role of the sending peer. A `PeerRelay`
    /// origin is not re-forwarded to other peer relays — that filter
    /// is the ONE line federation adds to the fan-out; the rest of
    /// the routing (author exclusion, live subscriber loop, offline
    /// store-and-forward via `store.enqueue_pending`) is unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if `envelope` can't be re-encoded for the
    /// durable queue, or if persisting it (pending, checkpoint, or
    /// retained tail) fails.
    pub fn fanout_delta(
        &mut self,
        author: &[u8; 32],
        from_role: PeerRole,
        envelope: &Envelope,
        raw_delta_bytes: &[u8],
        seq: u64,
    ) -> Result<Fanout, crate::store::StoreError> {
        let seq = self.next_seq(seq);
        let payload = crate::coedit::CoeditPayload::decode(raw_delta_bytes);
        let checkpoint = match &payload {
            Some(crate::coedit::CoeditPayload::Checkpoint {
                channel,
                entry,
                coverage,
                body,
            }) => match self.plan_checkpoint(channel, *entry, coverage, body)? {
                Some(plan) => Some(plan),
                None => return Ok(Fanout::default()),
            },
            _ => None,
        };
        // One encode and one allocation for every recipient below.
        let frame = encode_frame(envelope).map_err(|error| {
            crate::store::StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encode: {error}"),
            ))
        })?;
        // 1. Live fan-out — prune closed subscribers as we go.
        let delivered = self.fanout_frame(author, from_role, &frame);
        let mut queued = 0usize;
        // 2. Retained document traffic is stored once per channel. Every
        //    subscriber — a first-time guest, a reconnecting member, a
        //    federating relay — receives the retained checkpoints and tails
        //    on Subscribe, so a per-member pending copy would only duplicate
        //    it across every offline member.
        if let Some(payload) = &payload {
            match (payload, checkpoint) {
                (
                    crate::coedit::CoeditPayload::Checkpoint { channel, .. },
                    Some(CheckpointPlan {
                        base,
                        replaces_base: true,
                        covered,
                    }),
                ) => {
                    self.store.retain_checkpoint(channel, &frame)?;
                    self.tails.entry(*channel).or_default().base = Some(base);
                    match covered {
                        Some(covered) => self.trim_tail(channel, &covered)?,
                        None => self.clear_tail(channel)?,
                    }
                }
                // A checkpoint that does not name the retained base would
                // lose whatever of the base it lacks if it replaced it. As a
                // tail entry it adds its state without removing any.
                (
                    crate::coedit::CoeditPayload::Checkpoint { channel, entry, .. }
                    | crate::coedit::CoeditPayload::RetainedDelta { channel, entry, .. },
                    _,
                ) => {
                    self.store.retain_delta(channel, seq, &frame)?;
                    self.tails.entry(*channel).or_default().push(TailEntry {
                        seq,
                        bytes: frame.len() as u64,
                        author: *author,
                        entry: *entry,
                    });
                    self.enforce_tail_bounds(channel, author);
                }
            }
        } else {
            // 3. Materialize into the pinned doc so the host holds
            //    authoritative current state (the source for anti-entropy
            //    resync and for compacting an overflowing pending queue).
            //    Best-effort and no-op when the workspace is unpinned or the
            //    bytes are not a raw Automerge delta.
            self.materialize(raw_delta_bytes);
            // 4. Store-and-forward for offline members (any pubkey in
            //    `membership` that is not the author and not subscribed) and
            //    offline peer relays — the same per-device queue, suppressed
            //    when the origin is itself a peer relay (echo prevention
            //    across a chain of relays).
            let mut overflowing = Vec::new();
            for member in self.membership.devices() {
                if member == author || self.subscribers.contains_key(member) {
                    continue;
                }
                if self.store.enqueue_pending(member, seq, &frame)? {
                    overflowing.push(*member);
                }
                queued = queued.saturating_add(1);
            }
            if from_role == PeerRole::Device {
                for pr in &self.known_peer_relays {
                    if pr == author || self.subscribers.contains_key(pr) {
                        continue;
                    }
                    if self.store.enqueue_pending(pr, seq, &frame)? {
                        overflowing.push(*pr);
                    }
                    queued = queued.saturating_add(1);
                }
            }
            self.compact_pending(&overflowing, seq)?;
        }
        // Audit log — best-effort; failure does not block routing.
        let _ = self.store.append_log(&LogEntry {
            workspace_id: self.store.workspace_id(),
            received_at_ns: seq,
            author_device_hex: hex::encode(author),
            authored_at_ns: seq,
            delta_len: raw_delta_bytes.len() as u64,
        });
        Ok(Fanout { delivered, queued })
    }

    /// Compact the pending queues of `devices`, which just overflowed. In a
    /// pinned workspace their queued changes are replaced by one delta
    /// carrying the whole materialized document — encoded once for all of
    /// them — so a returning device receives no change whose dependencies
    /// were dropped. An unpinned workspace has no such state and drops the
    /// oldest entries instead.
    fn compact_pending(
        &mut self,
        devices: &[[u8; 32]],
        seq: u64,
    ) -> Result<(), crate::store::StoreError> {
        if devices.is_empty() {
            return Ok(());
        }
        let document = match self.doc.as_mut() {
            Some(doc) => Some(
                encode_frame(&Envelope::Delta {
                    workspace_id: self.store.workspace_id(),
                    author_device_public_key: [0; 32],
                    authored_at_ns: seq,
                    delta: doc.save(),
                    signature: [0; 64],
                })
                .map_err(|error| {
                    crate::store::StoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("encode document: {error}"),
                    ))
                })?,
            ),
            None => None,
        };
        for device in devices {
            self.store
                .compact_pending(device, document.as_deref().map(|frame| (seq, frame)))?;
        }
        Ok(())
    }

    /// Decide what an arriving checkpoint does. What it covers is only ever
    /// what it says: a whole-state baseline supersedes everything retained;
    /// a document merge names the retained entries its publisher read from
    /// the bytes it saved. `None` for the exact checkpoint already retained
    /// (every member republishes on attach), which is neither fanned out nor
    /// retained again — though what it names as covered still holds.
    fn plan_checkpoint(
        &mut self,
        channel: &[u8; 32],
        entry: [u8; 32],
        coverage: &crate::coedit::Coverage,
        body: &[u8],
    ) -> Result<Option<CheckpointPlan>, crate::store::StoreError> {
        let digest = *blake3::hash(body).as_bytes();
        let covered: Option<HashSet<[u8; 32]>> = match coverage {
            crate::coedit::Coverage::Supersedes => None,
            crate::coedit::Coverage::Entries(entries) => Some(entries.iter().copied().collect()),
        };
        let tail = self.tails.entry(*channel).or_default();
        let replaces_base = match (&tail.base, &covered) {
            (None, _) | (_, None) => true,
            (Some(base), Some(covered)) => covered.contains(&base.entry),
        };
        if tail.base.is_some_and(|base| base.digest == digest) {
            if replaces_base && let Some(covered) = &covered {
                self.trim_tail(channel, covered)?;
            }
            return Ok(None);
        }
        Ok(Some(CheckpointPlan {
            base: RetainedBase { entry, digest },
            replaces_base,
            covered,
        }))
    }

    /// Drop the front of `channel`'s tail while the base checkpoint just
    /// retained names each entry as covered.
    fn trim_tail(
        &mut self,
        channel: &[u8; 32],
        covered: &HashSet<[u8; 32]>,
    ) -> Result<(), crate::store::StoreError> {
        let Some(tail) = self.tails.get_mut(channel) else {
            return Ok(());
        };
        if let Some(through) = tail.trim_front(covered) {
            self.store.trim_tail(channel, through)?;
            tail.last_request = None;
        }
        Ok(())
    }

    /// Drop all of `channel`'s tail: a whole-state baseline superseded it.
    fn clear_tail(&mut self, channel: &[u8; 32]) -> Result<(), crate::store::StoreError> {
        let Some(tail) = self.tails.get_mut(channel) else {
            return Ok(());
        };
        if let Some(last) = tail.entries.back().map(|entry| entry.seq) {
            tail.entries.clear();
            tail.bytes = 0;
            self.store.trim_tail(channel, last)?;
        }
        Ok(())
    }

    /// Past the soft bound ask a member for a covering checkpoint; past the
    /// hard bound disconnect the connected member that retained the most.
    fn enforce_tail_bounds(&mut self, channel: &[u8; 32], author: &[u8; 32]) {
        let bounds = self.tail_bounds;
        let Some(tail) = self.tails.get(channel) else {
            return;
        };
        let (entries, bytes) = (tail.entries.len(), tail.bytes);
        if entries <= bounds.soft_entries && bytes <= bounds.soft_bytes {
            return;
        }
        self.request_checkpoint(channel);
        if entries <= bounds.hard_entries && bytes <= bounds.hard_bytes {
            return;
        }
        let mut retained_by: HashMap<[u8; 32], u64> = HashMap::new();
        for entry in self
            .tails
            .get(channel)
            .into_iter()
            .flat_map(|tail| &tail.entries)
        {
            let total = retained_by.entry(entry.author).or_default();
            *total = total.saturating_add(entry.bytes);
        }
        let heaviest = retained_by
            .into_iter()
            .filter(|(device, _)| {
                self.subscribers
                    .get(device)
                    .is_some_and(|sub| sub.role == PeerRole::Device)
            })
            .max_by_key(|(device, bytes)| (*bytes, device == author));
        let Some((heaviest, heaviest_bytes)) = heaviest else {
            tracing::warn!(
                workspace_id = self.store.workspace_id(),
                channel = %hex::encode(channel),
                entries,
                bytes,
                "retained tail is over its hard bound and no sender of it is connected"
            );
            return;
        };
        tracing::warn!(
            workspace_id = self.store.workspace_id(),
            channel = %hex::encode(channel),
            device = %hex::encode(heaviest),
            retained_bytes = heaviest_bytes,
            "retained tail is over its hard bound; disconnecting its heaviest sender"
        );
        if let Some(sub) = self.subscribers.get(&heaviest) {
            let _ = sub.disconnect.try_send(());
        }
        self.remove_subscriber(&heaviest);
    }

    /// Ask one connected member for a fresh checkpoint of every document it
    /// holds, by sending it `Ready` again — what a shell answers with full
    /// checkpoints, exactly as after subscribing. Prefers the most recent
    /// sender on `channel`, which certainly holds that document. At most one
    /// request per channel per [`TailBounds::request_interval`].
    fn request_checkpoint(&mut self, channel: &[u8; 32]) {
        let interval = self.tail_bounds.request_interval;
        let Some(tail) = self.tails.get(channel) else {
            return;
        };
        if tail
            .last_request
            .is_some_and(|last| last.elapsed() < interval)
        {
            return;
        }
        let is_device = |device: &[u8; 32]| {
            self.subscribers
                .get(device)
                .is_some_and(|sub| sub.role == PeerRole::Device)
        };
        let target = tail
            .entries
            .iter()
            .rev()
            .map(|entry| entry.author)
            .find(|author| is_device(author))
            .or_else(|| {
                self.subscribers
                    .iter()
                    .find(|(_, sub)| sub.role == PeerRole::Device)
                    .map(|(device, _)| *device)
            });
        let Some(target) = target else {
            return;
        };
        let request = Envelope::Ready {
            workspace_id: self.store.workspace_id(),
            pending_deltas: 0,
        };
        let Ok(frame) = encode_frame(&request) else {
            return;
        };
        let Some(sub) = self.subscribers.get(&target) else {
            return;
        };
        if sub.tx.try_send(frame).is_err() {
            return;
        }
        if let Some(tail) = self.tails.get_mut(channel) {
            tail.last_request = Some(std::time::Instant::now());
        }
    }

    /// Hand one shared frame to every live subscriber except `author`
    /// (and, for peer-relay traffic, except other peer relays). A full or
    /// closed subscriber is told to disconnect and pruned. Returns how many
    /// subscribers took the frame.
    fn fanout_frame(
        &mut self,
        author: &[u8; 32],
        from_role: PeerRole,
        frame: &OutboundFrame,
    ) -> usize {
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
            if sub.tx.try_send(Arc::clone(frame)).is_err() {
                let _ = sub.disconnect.try_send(());
                dead.push(*peer);
            } else {
                delivered = delivered.saturating_add(1);
            }
        }
        for d in dead {
            self.subscribers.remove(&d);
        }
        delivered
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
        // Anything that is not an Automerge chunk cannot advance the doc;
        // refuse it before paying for a clone of the whole document.
        if !delta_bytes.starts_with(&AUTOMERGE_MAGIC) {
            return false;
        }
        let Some(current) = self.doc.as_ref() else {
            return false;
        };
        // Apply to a clone so a malformed / non-Automerge payload cannot
        // half-apply and corrupt the authoritative doc.
        let mut candidate = current.clone();
        match candidate.load_incoming(delta_bytes) {
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
    ///
    /// # Errors
    ///
    /// Returns an error if fanning the ingested batch back out to
    /// local subscribers fails to persist (see [`Self::fanout_delta`]).
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
        self.fanout_delta(&[0u8; 32], PeerRole::PeerRelay, &env, changes, seq)?;
        Ok(true)
    }

    /// Route a presence envelope (no persistence, drop-on-empty).
    /// Same echo-suppression rule as `fanout_delta`.
    pub fn fanout_presence(
        &mut self,
        author: &[u8; 32],
        from_role: PeerRole,
        envelope: &Envelope,
    ) -> usize {
        match encode_frame(envelope) {
            Ok(frame) => self.fanout_frame(author, from_role, &frame),
            Err(error) => {
                tracing::warn!(%error, "presence envelope could not be encoded");
                0
            }
        }
    }

    /// Deliver a directed envelope to the live subscriber `target` only.
    /// Never queued: a directed request is live traffic. Returns whether it
    /// was handed to that subscriber's connection.
    pub fn route_directed(&mut self, target: &[u8; 32], envelope: &Envelope) -> bool {
        let Some(sub) = self.subscribers.get(target) else {
            return false;
        };
        let Ok(frame) = encode_frame(envelope) else {
            return false;
        };
        if sub.tx.try_send(frame).is_ok() {
            return true;
        }
        let _ = sub.disconnect.try_send(());
        self.subscribers.remove(target);
        false
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

    /// The envelope a subscriber's wire frame carries.
    fn decoded(frame: &OutboundFrame) -> Envelope {
        crate::frame::decode(&frame[4..]).unwrap()
    }

    fn subscriber(
        device: [u8; 32],
        capacity: usize,
    ) -> (Subscriber, mpsc::Receiver<OutboundFrame>) {
        let (tx, rx) = mpsc::channel(capacity);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        (
            Subscriber {
                device_public_key: device,
                role: PeerRole::Device,
                tx,
                disconnect,
            },
            rx,
        )
    }

    fn device(n: u16) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[..2].copy_from_slice(&n.to_le_bytes());
        key[31] = 0xAA;
        key
    }

    fn coedit_delta(
        ws: u64,
        author: [u8; 32],
        payload: &crate::coedit::CoeditPayload,
    ) -> (Envelope, Vec<u8>) {
        let bytes = payload.encode().unwrap();
        (
            Envelope::Delta {
                workspace_id: ws,
                author_device_public_key: author,
                authored_at_ns: 1,
                delta: bytes.clone(),
                signature: [0u8; 64],
            },
            bytes,
        )
    }

    /// A public room: a thousand member devices, a few hundred of them
    /// online. A retained chat message reaches every live subscriber as the
    /// same shared allocation, is stored exactly once on disk, and writes no
    /// per-member pending copy for the offline majority — which still
    /// receive it on subscribe from the retained store.
    #[test]
    fn retained_fanout_to_a_large_room_shares_one_frame_and_writes_no_per_member_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let members: Vec<[u8; 32]> = (0..1000).map(device).collect();
        let mut membership = Membership::default();
        membership.set(members.clone());
        let mut rt = WorkspaceRuntime::open(tmp.path(), 90, true, membership).unwrap();
        let mut receivers = Vec::new();
        for member in &members[..300] {
            let (sub, rx) = subscriber(*member, 8);
            rt.add_subscriber(sub);
            receivers.push(rx);
        }
        let author = members[0];
        let channel = [0x5C; 32];
        let (env, raw) = coedit_delta(90, author, &retained(channel, vec![0xEE; 4096]));
        let fanout = rt
            .fanout_delta(&author, PeerRole::Device, &env, &raw, 10)
            .unwrap();
        assert_eq!(fanout.delivered, 299, "every live member but the author");
        assert_eq!(
            fanout.queued, 0,
            "retained traffic is never copied per member"
        );

        let frames: Vec<OutboundFrame> = receivers[1..]
            .iter_mut()
            .map(|rx| rx.try_recv().unwrap())
            .collect();
        assert!(receivers[0].try_recv().is_err(), "the author is excluded");
        assert!(
            frames
                .windows(2)
                .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])),
            "one encoded frame is shared by every subscriber"
        );
        assert_eq!(decoded(&frames[0]), env);

        for offline in &members[300..] {
            assert_eq!(rt.store.count_pending(offline).unwrap(), 0);
        }
        let pending_root = rt.store.root().join("pending");
        assert_eq!(
            std::fs::read_dir(&pending_root).unwrap().count(),
            0,
            "no pending directory was created for any offline member"
        );
        let retained = rt.store.load_retained().unwrap();
        assert_eq!(retained.len(), 1, "the message is stored once for the room");
        assert_eq!(retained[0].as_slice(), &frames[0][..]);
    }

    /// Every member republishes its whole document when it attaches to the
    /// host. A byte-identical republish of the retained checkpoint is not
    /// fanned out again and does not discard the tail retained after it.
    #[test]
    fn identical_checkpoint_republish_is_neither_fanned_out_nor_retained_again() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 91, true, Membership::default()).unwrap();
        let (sub, mut rx) = subscriber(device(7), 8);
        rt.add_subscriber(sub);
        let channel = [0x11; 32];
        let state = checkpoint(channel, b"state-1".to_vec(), Vec::new());
        let (first, raw) = coedit_delta(91, device(1), &state);
        assert_eq!(
            rt.fanout_delta(&device(1), PeerRole::Device, &first, &raw, 1)
                .unwrap()
                .delivered,
            1
        );
        rx.try_recv().unwrap();
        let (tail, tail_raw) = coedit_delta(91, device(1), &retained(channel, b"after".to_vec()));
        rt.fanout_delta(&device(1), PeerRole::Device, &tail, &tail_raw, 2)
            .unwrap();
        rx.try_recv().unwrap();

        let (again, again_raw) = coedit_delta(91, device(2), &state);
        let fanout = rt
            .fanout_delta(&device(2), PeerRole::Device, &again, &again_raw, 3)
            .unwrap();
        assert_eq!(fanout, Fanout::default());
        assert!(rx.try_recv().is_err(), "nothing new reached the subscriber");
        assert_eq!(
            rt.store.load_retained().unwrap().len(),
            2,
            "checkpoint and its tail are both still retained"
        );

        let (changed, changed_raw) = coedit_delta(
            91,
            device(2),
            &checkpoint(channel, b"state-2".to_vec(), Vec::new()),
        );
        assert_eq!(
            rt.fanout_delta(&device(2), PeerRole::Device, &changed, &changed_raw, 4)
                .unwrap()
                .delivered,
            1
        );
        // The new checkpoint names nothing as covered — not the retained base
        // either — so it is kept after them instead of replacing either.
        assert_eq!(rt.store.load_retained().unwrap().len(), 3);
    }

    /// Apply every retained frame, in replay order, to a fresh document —
    /// what a member joining the room does.
    fn join(rt: &WorkspaceRuntime) -> AutoCommit {
        let mut doc = AutoCommit::new();
        for frame in rt.store.load_retained().unwrap() {
            let Envelope::Delta { delta, .. } = crate::frame::decode(&frame[4..]).unwrap() else {
                panic!("retained frames are deltas");
            };
            let body = match crate::coedit::CoeditPayload::decode(&delta).unwrap() {
                crate::coedit::CoeditPayload::Checkpoint { body, .. }
                | crate::coedit::CoeditPayload::RetainedDelta { body, .. } => body,
            };
            doc.load_incoming(&body).unwrap();
        }
        doc
    }

    fn publish(
        rt: &mut WorkspaceRuntime,
        author: [u8; 32],
        payload: &crate::coedit::CoeditPayload,
    ) -> Fanout {
        let (env, raw) = coedit_delta(rt.store.workspace_id(), author, payload);
        rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 1)
            .unwrap()
    }

    fn edit(doc: &mut AutoCommit, n: usize) -> Vec<u8> {
        use automerge::transaction::Transactable;
        doc.put(automerge::ROOT, format!("k{n}"), i64::try_from(n).unwrap())
            .unwrap();
        doc.save_incremental()
    }

    fn entry_of(body: &[u8]) -> [u8; 32] {
        *blake3::hash(body).as_bytes()
    }

    fn retained(channel: [u8; 32], body: Vec<u8>) -> crate::coedit::CoeditPayload {
        crate::coedit::CoeditPayload::RetainedDelta {
            channel,
            entry: entry_of(&body),
            body,
        }
    }

    fn checkpoint(
        channel: [u8; 32],
        body: Vec<u8>,
        covers: Vec<[u8; 32]>,
    ) -> crate::coedit::CoeditPayload {
        crate::coedit::CoeditPayload::Checkpoint {
            channel,
            entry: entry_of(&body),
            coverage: crate::coedit::Coverage::Entries(covers),
            body,
        }
    }

    /// A tail past its soft bound is never cut in the middle, and is
    /// trimmed only by what a checkpoint names as covered — never by when it
    /// arrived. Here the answer to the relay's checkpoint request was saved
    /// before two later edits reached the relay (the race a sequence would
    /// over-credit): it names only what it holds, those two stay, and a
    /// member joining at any point converges with no change left waiting for
    /// a dependency.
    #[test]
    fn a_joiner_after_tail_trimming_converges_with_no_parked_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 94, false, Membership::default()).unwrap();
        rt.tail_bounds = TailBounds {
            soft_entries: 4,
            hard_entries: 1000,
            request_interval: std::time::Duration::from_secs(3600),
            ..TailBounds::default()
        };
        let writer = device(1);
        let (sub, mut writer_rx) = subscriber(writer, 64);
        rt.add_subscriber(sub);
        let channel = [0x77; 32];
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(b"writer".as_slice()));
        edit(&mut doc, 0);
        let base_body = doc.save();
        let mut ids = vec![entry_of(&base_body)];
        publish(&mut rt, writer, &checkpoint(channel, base_body, Vec::new()));
        for n in 1..=7 {
            let body = edit(&mut doc, n);
            ids.push(entry_of(&body));
            publish(&mut rt, writer, &retained(channel, body));
        }

        // Over the soft bound: one checkpoint request reached the writer,
        // and nothing was dropped while it is outstanding.
        let requests: Vec<Envelope> = std::iter::from_fn(|| writer_rx.try_recv().ok())
            .map(|frame| decoded(&frame))
            .collect();
        assert_eq!(
            requests,
            vec![Envelope::Ready {
                workspace_id: 94,
                pending_deltas: 0
            }]
        );
        assert_eq!(rt.store.load_retained().unwrap().len(), 8);
        let mut early = join(&rt);
        assert!(early.get_missing_deps(&[]).is_empty());
        assert_eq!(early.get_heads(), doc.get_heads());

        // The member saves its answer now, holding the base and seven edits.
        let answer = doc.save();
        let answer_covers = ids.clone();
        // Two more edits reach the relay before the answer does.
        for n in 8..=9 {
            let body = edit(&mut doc, n);
            publish(&mut rt, writer, &retained(channel, body));
        }
        publish(&mut rt, writer, &checkpoint(channel, answer, answer_covers));
        assert_eq!(
            rt.store.load_retained().unwrap().len(),
            1 + 2,
            "the answer replaced the base and the seven edits it holds; the two it lacks stay"
        );
        let mut joiner = join(&rt);
        assert!(
            joiner.get_missing_deps(&[]).is_empty(),
            "no retained change waits for a dependency"
        );
        assert_eq!(joiner.get_heads(), doc.get_heads());

        // A restarted relay rebuilds the same accounting from disk.
        drop(rt);
        let reopened =
            WorkspaceRuntime::open(tmp.path(), 94, false, Membership::default()).unwrap();
        assert_eq!(reopened.tails[&channel].entries.len(), 2);
        assert!(reopened.tails[&channel].base.is_some());
        assert_eq!(join(&reopened).get_heads(), doc.get_heads());
    }

    /// Coverage that skips the oldest tail entry trims nothing — the tail is
    /// cut only from its front.
    #[test]
    fn coverage_with_a_hole_at_the_front_trims_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 98, false, Membership::default()).unwrap();
        let channel = [0x7A; 32];
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(b"writer".as_slice()));
        edit(&mut doc, 0);
        let base_body = doc.save();
        let base = entry_of(&base_body);
        publish(
            &mut rt,
            device(1),
            &checkpoint(channel, base_body, Vec::new()),
        );
        let first = edit(&mut doc, 1);
        publish(&mut rt, device(1), &retained(channel, first));
        let second = edit(&mut doc, 2);
        let second_id = entry_of(&second);
        publish(&mut rt, device(1), &retained(channel, second));
        publish(
            &mut rt,
            device(1),
            &checkpoint(channel, doc.save(), vec![base, second_id]),
        );
        assert_eq!(rt.store.load_retained().unwrap().len(), 3);
        let mut joiner = join(&rt);
        assert!(joiner.get_missing_deps(&[]).is_empty());
        assert_eq!(joiner.get_heads(), doc.get_heads());
    }

    /// An offline device's pending queue that overflows in a pinned
    /// workspace is replaced by one whole-document delta, never cut in the
    /// middle: the device converges with no change waiting for a dependency.
    #[test]
    fn an_overflowing_pending_queue_is_replaced_by_the_whole_document() {
        let tmp = tempfile::tempdir().unwrap();
        let (writer, offline) = (device(1), device(2));
        let mut membership = Membership::default();
        membership.set(vec![writer, offline]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 97, true, membership).unwrap();
        rt.store.set_bounds(crate::store::QueueBounds {
            pending_entries: 4,
            pending_bytes: 1024 * 1024,
        });
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(b"writer".as_slice()));
        for n in 0..11 {
            let delta = edit(&mut doc, n);
            let env = Envelope::Delta {
                workspace_id: 97,
                author_device_public_key: writer,
                authored_at_ns: 1,
                delta: delta.clone(),
                signature: [0; 64],
            };
            rt.fanout_delta(&writer, PeerRole::Device, &env, &delta, 1)
                .unwrap();
            assert!(rt.store.count_pending(&offline).unwrap() <= 5);
        }
        let mut returning = AutoCommit::new();
        for frame in rt.store.drain_pending(&offline).unwrap() {
            let Envelope::Delta { delta, .. } = crate::frame::decode(&frame[4..]).unwrap() else {
                panic!("pending frames are deltas");
            };
            returning.load_incoming(&delta).unwrap();
        }
        assert!(
            returning.get_missing_deps(&[]).is_empty(),
            "no queued change waits for a dependency"
        );
        assert_eq!(returning.get_heads(), doc.get_heads());
    }

    /// A checkpoint that does not name the retained base as covered may
    /// lack part of it, so it neither replaces the base nor trims the tail.
    #[test]
    fn an_uncovered_checkpoint_replaces_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 95, false, Membership::default()).unwrap();
        let channel = [0x78; 32];
        let mut base = AutoCommit::new();
        base.set_actor(automerge::ActorId::from(b"base".as_slice()));
        edit(&mut base, 1);
        publish(
            &mut rt,
            device(1),
            &checkpoint(channel, base.save(), Vec::new()),
        );
        let mut other = AutoCommit::new();
        other.set_actor(automerge::ActorId::from(b"other".as_slice()));
        edit(&mut other, 2);
        let (env, raw) = coedit_delta(
            95,
            device(2),
            &checkpoint(channel, other.save(), Vec::new()),
        );
        rt.fanout_delta(&device(2), PeerRole::PeerRelay, &env, &raw, 1)
            .unwrap();
        let mut joiner = join(&rt);
        let mut both = base.clone();
        both.merge_incoming(&mut other).unwrap();
        assert_eq!(
            joiner.get_heads(),
            both.get_heads(),
            "neither state was lost"
        );
    }

    /// Past the hard bound the relay keeps accepting but disconnects the
    /// connected sender that retained the most, whose reconnect publishes
    /// the checkpoint that lets the tail be trimmed.
    #[test]
    fn a_tail_over_its_hard_bound_disconnects_the_heaviest_connected_sender() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 96, false, Membership::default()).unwrap();
        rt.tail_bounds = TailBounds {
            soft_entries: 2,
            hard_entries: 4,
            request_interval: std::time::Duration::from_secs(3600),
            ..TailBounds::default()
        };
        let (heavy, light) = (device(1), device(2));
        let (heavy_tx, _heavy_rx) = mpsc::channel(64);
        let (heavy_disconnect, mut heavy_disconnected) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: heavy,
            role: PeerRole::Device,
            tx: heavy_tx,
            disconnect: heavy_disconnect,
        });
        let (sub, _light_rx) = subscriber(light, 64);
        rt.add_subscriber(sub);
        let channel = [0x79; 32];
        for (author, size) in [(light, 8), (heavy, 512), (heavy, 512), (light, 8)] {
            publish(&mut rt, author, &retained(channel, vec![0; size]));
        }
        assert!(rt.has_subscriber(&heavy), "at the hard bound, not over it");
        publish(&mut rt, light, &retained(channel, vec![0; 8]));
        assert_eq!(
            rt.store.load_retained().unwrap().len(),
            5,
            "the delta over the bound is still accepted"
        );
        heavy_disconnected
            .try_recv()
            .expect("the heaviest sender is told to disconnect");
        assert!(!rt.has_subscriber(&heavy));
        assert!(rt.has_subscriber(&light));
    }

    /// Deltas that arrive in the same relay-clock nanosecond still get
    /// distinct queue entries, in arrival order.
    #[test]
    fn same_nanosecond_deltas_never_share_a_pending_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let offline = device(3);
        let mut membership = Membership::default();
        membership.set(vec![offline]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 92, false, membership).unwrap();
        for body in [b"one".as_slice(), b"two", b"three"] {
            let env = Envelope::Delta {
                workspace_id: 92,
                author_device_public_key: device(1),
                authored_at_ns: 1,
                delta: body.to_vec(),
                signature: [0u8; 64],
            };
            rt.fanout_delta(&device(1), PeerRole::Device, &env, body, 500)
                .unwrap();
        }
        let bodies: Vec<Vec<u8>> = rt
            .store
            .drain_pending(&offline)
            .unwrap()
            .iter()
            .map(|raw| match crate::frame::decode(&raw[4..]).unwrap() {
                Envelope::Delta { delta, .. } => delta,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            bodies,
            vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
        );
    }

    /// The per-second access audit of every subscriber re-reads membership
    /// only after the file changed, and still sees every rewrite.
    #[test]
    fn membership_refresh_reloads_only_a_changed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 93, false, Membership::default()).unwrap();
        rt.store.save_membership(&[device(1)]).unwrap();
        rt.refresh_membership().unwrap();
        assert!(rt.membership.contains(&device(1)));

        // An in-memory change the file does not carry survives a refresh of
        // the unchanged file: nothing was re-read.
        rt.membership.insert(device(9));
        rt.refresh_membership().unwrap();
        assert!(rt.membership.contains(&device(9)));

        // A rewrite (an admin `add-member`) is picked up.
        rt.store.save_membership(&[device(1), device(2)]).unwrap();
        rt.refresh_membership().unwrap();
        assert!(rt.membership.contains(&device(2)));
        assert!(!rt.membership.contains(&device(9)));
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
            .fanout_delta(&[1u8; 32], PeerRole::Device, &env, b"1 2 3", 111)
            .unwrap();
        assert_eq!(delivered.delivered, 1);
        // Peer 2 saw the delta live.
        assert_eq!(decoded(&rx2.try_recv().unwrap()), env);
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
            .fanout_delta(&[1u8; 32], PeerRole::Device, &env, b"", 1)
            .unwrap();
        assert_eq!(delivered.delivered, 0);
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
        tx.try_send(encode_frame(&env_delta(6, [9u8; 32])).unwrap())
            .unwrap();
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
                &env_delta(6, author),
                b"durable",
                2,
            )
            .unwrap();

        assert_eq!(delivered.delivered, 0);
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
            .fanout_delta(&author, PeerRole::PeerRelay, &env, b"x", 1)
            .unwrap();
        // Only the local device saw it.
        assert_eq!(delivered.delivered, 1);
        assert_eq!(decoded(&rx_dev.try_recv().unwrap()), env);
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
        rt.fanout_delta(&author, PeerRole::Device, &env, b"x", 42)
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
        rt.fanout_delta(&author, PeerRole::PeerRelay, &env, b"x", 42)
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
        rt.fanout_delta(&[1u8; 32], PeerRole::Device, &env, &d1, 1)
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
        doc_peer.load_incoming(&delta_since).unwrap();
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
        rt.fanout_delta(&[5u8; 32], PeerRole::Device, &env, b"not-automerge", 1)
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
            .fanout_delta(&a, PeerRole::Device, &env, &d1, 111)
            .unwrap();
        assert_eq!(
            delivered.delivered, 1,
            "delta fanned out to the live subscriber B"
        );

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
        doc_b.load_incoming(&delta_since).unwrap();

        assert_eq!(
            doc_b.get_heads(),
            doc_a.get_heads(),
            "anti-entropy reconciles B to convergence despite the lost fan-out and empty queue"
        );
    }
}
