//! Per-workspace routing and authorization metadata. Live state storage and
//! the pinned document transfer lazily to the bounded ordered persistence
//! owner. Synchronous persistence methods remain compatibility helpers for
//! runtimes that have not entered shared live service.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tilo_automerge::ApplyIncoming;

use automerge::{AutoCommit, ChangeHash};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::PeerRole;
use crate::store::{CommitRecord, WorkspaceStore};
use crate::transport::Envelope;
use tilo_lock::MutexRecover as _;

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

    /// Replace the membership with a new set. A set that leaves out a device
    /// of the old one wakes the sessions admitted under it to re-check
    /// ([`crate::authority_epoch`]). One that only adds devices narrows
    /// nobody's access and wakes nobody: a public room admitting a crowd
    /// re-audited every member's session for every newcomer.
    pub fn set(&mut self, devices: Vec<[u8; 32]>) {
        if self.devices != devices {
            let narrowed = leaves_out(&self.devices, &devices);
            self.devices = devices;
            if narrowed {
                crate::authority_epoch::bump();
            }
        }
    }
}

/// Whether `next` leaves out a device `previous` lists.
fn leaves_out(previous: &[[u8; 32]], next: &[[u8; 32]]) -> bool {
    let kept: std::collections::HashSet<&[u8; 32]> = next.iter().collect();
    previous.iter().any(|device| !kept.contains(device))
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

/// The standing of each member device of a room, by device key.
pub type Standings = std::collections::BTreeMap<[u8; 32], tilo_action_policy::Standing>;

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    pub(crate) sync_refresh: Option<crate::peer::sync_refresh::Mailbox>,
    /// The in-process room authority governing this workspace, told of each
    /// retained body a device's admitted delta carries. Bounded; a full queue
    /// drops the notice.
    pub(crate) governor_tap: Option<mpsc::Sender<([u8; 32], Vec<u8>)>>,
    /// The in-process authority governing this workspace's room, if any: it
    /// answers the room's calls.
    pub(crate) room_authority: Option<crate::rooms::RoomAuthority>,
    /// The standing of each member device, as the room's governor projects
    /// it: what the relay decides each change's claimed action against.
    /// `None` until a governor projects one; the relay then decides changes
    /// against their app surface alone.
    pub(crate) standings: Option<Arc<Standings>>,
    /// On-disk state.
    pub store: WorkspaceStore,
    /// Initial pinned document. Transferred to `persistence` on first live
    /// state operation; live anti-entropy accesses that owner, never this field.
    pub doc: Option<AutoCommit>,
    pub(crate) persistence: Option<crate::persistence::Persistence>,
    /// Which of the user's devices are workspace members. Used to
    /// gate subscribes AND to decide who gets store-and-forward
    /// on delta receive.
    pub membership: Membership,
    /// Serializes filesystem authority operations independently of state IO.
    pub(crate) authorization: Arc<AsyncMutex<()>>,
    /// Failed/pending replacements cap what an old external file may grant.
    pub(crate) membership_ceiling: Option<HashSet<[u8; 32]>>,
    pub(crate) membership_revision: u64,
    /// The daemon's watch on this room's membership files, attached on the
    /// first fresh read once a watch covers the state root.
    pub(crate) disk_generation: Option<crate::membership_watch::DiskGeneration>,
    /// The watched generation `membership` was last loaded at. `None` until a
    /// live watch vouched for a load, so every read goes to the disk.
    pub(crate) loaded_generation: Option<u64>,
    /// How the room admits devices beyond its exact membership: explicitly,
    /// the relay owner's devices on a same-owner home host, or under native
    /// governance. Read only through [`crate::room_access::decide`].
    pub(crate) admission: crate::room_access::AdmissionMode,
    /// Cross-user roots admitted by the signed authority for this exact room.
    pub admitted_users: HashSet<[u8; 32]>,
    /// User roots needed only to pass the relay-wide handshake for devices in
    /// this native-governed room. Exact room admission still uses
    /// `membership`; keeping this separate from `admitted_users` prevents an
    /// ungoverned sibling device under the same root from joining.
    pub governed_user_roots: HashSet<[u8; 32]>,
    /// Who may watch this room read-only without being one of its members,
    /// when its host lets visitors watch it. Read only through
    /// [`crate::room_access::decide`].
    pub(crate) spectators: Option<crate::room_access::Spectators>,
    pub(crate) checkpoint_prefix_recovery: crate::config::CheckpointPrefixRecovery,
    /// Current live subscribers keyed by device pubkey.
    pub subscribers: HashMap<[u8; 32], Subscriber>,
    pub(crate) outbound_federation: HashMap<[u8; 32], Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) state_sessions: HashMap<[u8; 32], crate::peer::StateSession>,
    pub(crate) action_targets: HashMap<[u8; 32], crate::peer::ActionTarget>,
    /// Device pubkeys of peer relays this workspace has federated
    /// with at least once since the daemon started. Ops arriving
    /// while such a relay is offline are queued via the same
    /// store-and-forward path as offline devices — reuses
    /// `store.enqueue_pending` verbatim; no new persistence code.
    pub known_peer_relays: HashSet<[u8; 32]>,
    /// Ephemeral eligible workers plus durable exact-route job bindings.
    pub compute: crate::compute_transport::ComputeTransportState,
    /// Per live device subscriber: the retained-checkpoint channels a live
    /// checkpoint has already been fanned to since the device last asked for a
    /// baseline. A device asks on subscribe and whenever it sends a presence
    /// frame (the browser sends presence only for its hello, a `SyncReq`, and a
    /// rejected checkpoint), which resets its served set. The native publishes
    /// its checkpoint as one set of messages — one per channel (the room plus
    /// each record) — so the gate is per channel: a full bootstrap set passes
    /// once, and the next idle set, on the same channels, is withheld from a
    /// device that already holds every delta — see `fanout_delta`.
    checkpoint_served: HashMap<[u8; 32], HashSet<[u8; 32]>>,
    /// The on-demand records each subscriber holds, by channel: those it
    /// opened, and the chain parts it was replayed or followed live while
    /// they were their chain's head. Live changes to an on-demand record
    /// reach only these ([`crate::store::record_sync`]).
    held_records: HashMap<[u8; 32], HashSet<[u8; 32]>>,
    /// The presence mailbox of each subscriber whose connection takes its
    /// presence coalesced and batched ([`crate::presence_mailbox`]). Set
    /// right after the subscription it belongs to and dropped with it; a
    /// subscriber without one gets each presence on its frame queue.
    presence_mailboxes: HashMap<[u8; 32], Arc<crate::presence_mailbox::PresenceMailbox>>,
    /// Authors with a delta admitted here and not yet fanned out, and when
    /// it arrived: the caret and typing that follow it wait up to
    /// [`crate::presence_mailbox::DELTA_HOLD`] to go with it.
    /// Published deltas leave at once; entries older than the hold are
    /// dropped whenever one is noted, so it holds at most the authors typing
    /// right now.
    recent_deltas: HashMap<[u8; 32], tokio::time::Instant>,
    /// Subscribers that joined the live plane, with their rate budgets.
    live_members: HashMap<[u8; 32], LiveBudget>,
    /// Host order of the last admitted delta.
    last_seq: u64,
    /// Recently admitted commits, for de-duplicating resends.
    commits: CommitLedger,
    /// Commits being admitted, by author and operation id, each held from
    /// its ledger check until it is admitted and recorded
    /// ([`Self::claim_commit`]): two copies of one commit cannot both be
    /// admitted, while different commits are admitted side by side. One
    /// gate for the whole room held every commit through the previous one's
    /// durability and publication, so a busy room committed one change at a
    /// time.
    commit_claims: CommitClaims,
    /// Held from a commit's [`Self::remember_commit`] until its ledger write
    /// is done, so ledger writes reach the file in the order they were
    /// remembered.
    pub(crate) commit_ledger_lane: Arc<AsyncMutex<()>>,
}

/// The commits a room is admitting, by author and operation id, each with
/// the lock its copies wait on.
type CommitClaims = Arc<std::sync::Mutex<HashMap<([u8; 32], [u8; 16]), Arc<AsyncMutex<()>>>>>;

/// A commit's claim on its author and operation id
/// ([`WorkspaceRuntime::claim_commit`]), released when dropped.
pub(crate) struct CommitClaim {
    claims: CommitClaims,
    key: ([u8; 32], [u8; 16]),
    // Dropped after the entry is gone, waking the copies that waited on it.
    _held: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for CommitClaim {
    fn drop(&mut self) {
        self.claims.lock_recover().remove(&self.key);
    }
}

/// Admitted commits a workspace remembers. A browser resends its whole
/// outbox after every reconnect, and a commit whose `Committed` answer was
/// lost is resent too; within this window the resend is answered with the
/// original host order instead of being persisted and fanned out again.
/// Older commits are forgotten first; a resend of one of those is admitted
/// again, which the document merge absorbs.
pub const MAX_REMEMBERED_COMMITS: usize = 16_384;

/// The ledger write that makes one [`WorkspaceRuntime::remember_commit`]
/// durable.
#[must_use]
pub enum CommitLedgerWrite {
    /// Append one record.
    Append(CommitRecord),
    /// Replace the ledger, dropping evicted records.
    Rewrite(Vec<CommitRecord>),
}

impl CommitLedgerWrite {
    /// Write it to `store`. A failure is logged, not fatal: the commit itself
    /// is already durable, and a resend it would have recognised is merged
    /// by the document again.
    pub fn persist(&self, store: &WorkspaceStore) {
        let persisted = match self {
            Self::Append(record) => store.append_commit(record),
            Self::Rewrite(records) => store.rewrite_commits(records),
        };
        if let Err(error) = persisted {
            tracing::warn!(%error, "commit ledger not persisted");
        }
    }
}

#[derive(Default)]
struct CommitLedger {
    by_op: HashMap<([u8; 32], [u8; 16]), CommitRecord>,
    order: VecDeque<([u8; 32], [u8; 16])>,
    /// Records in the on-disk ledger, evicted ones included.
    logged: usize,
}

impl CommitLedger {
    fn load(records: Vec<CommitRecord>) -> Self {
        let mut ledger = Self {
            logged: records.len(),
            ..Self::default()
        };
        for record in records {
            ledger.insert(record);
        }
        ledger
    }

    fn insert(&mut self, record: CommitRecord) {
        let key = (record.author, record.op);
        if self.by_op.insert(key, record).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MAX_REMEMBERED_COMMITS {
            if let Some(evicted) = self.order.pop_front() {
                self.by_op.remove(&evicted);
            }
        }
    }
}

/// Per-publisher live rate budget over one-second windows.
#[derive(Default)]
struct LiveBudget {
    window_start_ns: u64,
    presence: u32,
    ops: u32,
}

impl LiveBudget {
    fn admit(&mut self, presence: bool, now_ns: u64) -> bool {
        if now_ns.saturating_sub(self.window_start_ns) >= 1_000_000_000 {
            *self = Self {
                window_start_ns: now_ns,
                ..Self::default()
            };
        }
        let (count, limit) = if presence {
            (&mut self.presence, crate::live::MAX_PRESENCE_RATE)
        } else {
            (&mut self.ops, crate::live::MAX_OPS_RATE)
        };
        if *count >= limit {
            return false;
        }
        *count = count.saturating_add(1);
        true
    }
}

/// The first bytes of every Automerge chunk (document, change or bundle).
const AUTOMERGE_MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];

/// Whether `bytes` can be an Automerge chunk. Automerge logs every chunk it
/// cannot parse before returning the error, and the deltas a relay routes
/// are mostly co-edit wrappers and sealed channel payloads, which never start
/// with its magic: only bytes that do are worth handing to its parser.
pub(crate) fn is_automerge_chunk(bytes: &[u8]) -> bool {
    bytes.starts_with(&AUTOMERGE_MAGIC)
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
        Self::open_with_payloads(
            state_root,
            workspace_id,
            pinned,
            membership,
            crate::payloads::neighbour(),
        )
    }

    /// [`Self::open`], its store classifying retained co-edit bodies with
    /// `payloads`, the adapter the host was started with.
    ///
    /// # Errors
    ///
    /// The same as [`Self::open`].
    pub fn open_with_payloads(
        state_root: &std::path::Path,
        workspace_id: u64,
        pinned: bool,
        membership: Membership,
        payloads: std::sync::Arc<dyn crate::payloads::ShellPayloads>,
    ) -> Result<Self, crate::store::StoreError> {
        Self::open_inner(
            state_root,
            workspace_id,
            pinned,
            membership,
            payloads,
            false,
        )
    }

    /// Open a pinned workspace only when its durable snapshot already exists.
    /// Native interactive hosting uses this path so a missing baseline cannot
    /// silently become a new empty authoritative document.
    pub(crate) fn open_with_required_snapshot(
        state_root: &std::path::Path,
        workspace_id: u64,
        pinned: bool,
        membership: Membership,
        payloads: std::sync::Arc<dyn crate::payloads::ShellPayloads>,
    ) -> Result<Self, crate::store::StoreError> {
        Self::open_inner(state_root, workspace_id, pinned, membership, payloads, true)
    }

    fn open_inner(
        state_root: &std::path::Path,
        workspace_id: u64,
        pinned: bool,
        membership: Membership,
        payloads: std::sync::Arc<dyn crate::payloads::ShellPayloads>,
        require_snapshot: bool,
    ) -> Result<Self, crate::store::StoreError> {
        let store = WorkspaceStore::open_with_payloads(state_root, workspace_id, pinned, payloads)?;
        let governed_access_record = store.load_governed_access()?;
        let admission = if governed_access_record.is_some() {
            crate::room_access::AdmissionMode::Governed
        } else {
            crate::room_access::AdmissionMode::Explicit
        };
        let governed_user_roots = governed_access_record.map_or_else(HashSet::new, |access| {
            access.member_user_roots.into_iter().collect()
        });
        let mut membership = membership;
        let stored_membership = store.load_membership()?;
        for device in stored_membership {
            if !membership.contains(&device) {
                membership.devices.push(device);
            }
        }
        let doc = if pinned {
            let snapshot = store.load_snapshot()?;
            match snapshot {
                Some(bytes) => Some(AutoCommit::load(&bytes).map_err(|e| {
                    crate::store::StoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("automerge load: {e}"),
                    ))
                })?),
                None if require_snapshot => {
                    return Err(crate::store::StoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "required durable workspace snapshot is missing",
                    )));
                }
                None => Some(AutoCommit::new()),
            }
        } else {
            None
        };
        let commits = CommitLedger::load(store.load_commits()?);
        Ok(Self {
            store,
            doc,
            persistence: None,
            membership,
            authorization: Arc::new(AsyncMutex::new(())),
            membership_ceiling: None,
            membership_revision: 0,
            disk_generation: None,
            loaded_generation: None,
            admission,
            admitted_users: HashSet::new(),
            governed_user_roots,
            spectators: None,
            checkpoint_prefix_recovery: crate::config::CheckpointPrefixRecovery::Disabled,
            subscribers: HashMap::new(),
            known_peer_relays: HashSet::new(),
            compute: crate::compute_transport::ComputeTransportState::default(),
            action_targets: HashMap::new(),
            state_sessions: HashMap::new(),
            outbound_federation: HashMap::new(),
            checkpoint_served: HashMap::new(),
            held_records: HashMap::new(),
            presence_mailboxes: HashMap::new(),
            recent_deltas: HashMap::new(),
            sync_refresh: None,
            governor_tap: None,
            room_authority: None,
            standings: None,
            live_members: HashMap::new(),
            last_seq: 0,
            commits,
            commit_claims: Arc::default(),
            commit_ledger_lane: Arc::new(AsyncMutex::new(())),
        })
    }

    /// The admission of `author`'s commit `op`, if this host remembers it.
    pub fn admitted_commit(&self, author: &[u8; 32], op: &[u8; 16]) -> Option<CommitRecord> {
        self.commits.by_op.get(&(*author, *op)).copied()
    }

    /// Claim `author`'s commit `op` for admission. Called under the room's
    /// lock right after [`Self::admitted_commit`] found no record of it, so a
    /// copy of the commit either sees the claim or, once the claim is
    /// released after [`Self::remember_commit`], the record. A copy that
    /// finds the claim gets its lock to wait on, and checks both again.
    pub(crate) fn claim_commit(
        &self,
        author: [u8; 32],
        op: [u8; 16],
    ) -> Result<CommitClaim, Arc<AsyncMutex<()>>> {
        let key = (author, op);
        let mut claims = self.commit_claims.lock_recover();
        if let Some(held) = claims.get(&key) {
            return Err(held.clone());
        }
        let lock = Arc::new(AsyncMutex::new(()));
        let Ok(held) = lock.clone().try_lock_owned() else {
            // A lock no one else has seen is free.
            return Err(lock);
        };
        claims.insert(key, lock);
        drop(claims);
        Ok(CommitClaim {
            claims: self.commit_claims.clone(),
            key,
            _held: held,
        })
    }

    /// Remember an admitted commit. The returned write makes it durable, so
    /// a resend is recognised also after a host restart; run it off the
    /// workspace lock and in remembering order (see `commit_ledger_lane`).
    pub fn remember_commit(&mut self, record: CommitRecord) -> CommitLedgerWrite {
        self.commits.insert(record);
        self.commits.logged = self.commits.logged.saturating_add(1);
        if self.commits.logged > MAX_REMEMBERED_COMMITS.saturating_mul(2) {
            let kept: Vec<CommitRecord> = self
                .commits
                .order
                .iter()
                .filter_map(|key| self.commits.by_op.get(key).copied())
                .collect();
            self.commits.logged = kept.len();
            CommitLedgerWrite::Rewrite(kept)
        } else {
            CommitLedgerWrite::Append(record)
        }
    }

    /// The host order for the next admitted delta: wall-clock nanoseconds,
    /// forced strictly past the previous one so a clock step never reorders.
    pub fn next_seq(&mut self, now_ns: u64) -> u64 {
        self.last_seq = now_ns.max(self.last_seq.saturating_add(1));
        self.last_seq
    }

    /// Add subscriber `device` to the live plane and tell the other live
    /// members. Returns the members already present. A device that is not
    /// subscribed cannot join.
    pub fn live_join(&mut self, device: [u8; 32], workspace_id: u64) -> Option<Vec<[u8; 32]>> {
        if !self.subscribers.contains_key(&device) {
            return None;
        }
        let existing: Vec<[u8; 32]> = self
            .live_members
            .keys()
            .copied()
            .filter(|member| *member != device)
            .collect();
        if self
            .live_members
            .insert(device, LiveBudget::default())
            .is_none()
        {
            self.announce_live_member(device, workspace_id, true);
        }
        Some(existing)
    }

    /// Remove `device` from the live plane and tell the remaining members.
    pub fn live_leave(&mut self, device: &[u8; 32], workspace_id: u64) {
        if self.live_members.remove(device).is_some() {
            self.announce_live_member(*device, workspace_id, false);
        }
    }

    /// Whether `device` is a live member.
    pub fn is_live_member(&self, device: &[u8; 32]) -> bool {
        self.live_members.contains_key(device)
    }

    /// Relay a live envelope that arrived from outside this workspace's
    /// iroh live plane (a browser's WebTransport session, already validated
    /// and rate-limited by the live relay) to every iroh live member other
    /// than its author. Returns how many members received it.
    pub fn relay_bridged_live(&self, author: &[u8; 32], envelope: &Envelope) -> usize {
        let mut delivered = 0usize;
        for member in self.live_members.keys() {
            if member == author {
                continue;
            }
            if let Some(sub) = self.subscribers.get(member)
                && sub.tx.try_send(envelope.clone()).is_ok()
            {
                delivered = delivered.saturating_add(1);
            }
        }
        delivered
    }

    /// Relay one validated live envelope from `author` to every other live
    /// member, within the author's rate budget. Live traffic is lossy: a
    /// member whose queue is full misses the frame rather than being
    /// disconnected. Returns how many members received it.
    pub fn fanout_live(
        &mut self,
        author: &[u8; 32],
        presence: bool,
        envelope: &Envelope,
        now_ns: u64,
    ) -> usize {
        let Some(budget) = self.live_members.get_mut(author) else {
            return 0;
        };
        if !budget.admit(presence, now_ns) {
            return 0;
        }
        let mut delivered = 0usize;
        for member in self.live_members.keys() {
            if member == author {
                continue;
            }
            if let Some(sub) = self.subscribers.get(member)
                && sub.tx.try_send(envelope.clone()).is_ok()
            {
                delivered = delivered.saturating_add(1);
            }
        }
        delivered
    }

    fn announce_live_member(&self, device: [u8; 32], workspace_id: u64, active: bool) {
        let envelope = Envelope::LiveMember {
            workspace_id,
            device_public_key: device,
            active,
        };
        for member in self.live_members.keys() {
            if *member == device {
                continue;
            }
            if let Some(sub) = self.subscribers.get(member) {
                let _ = sub.tx.try_send(envelope.clone());
            }
        }
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
        if !self
            .subscribers
            .get(&sub.device_public_key)
            .is_some_and(|current| current.tx.same_channel(&sub.tx))
        {
            self.action_targets.remove(&sub.device_public_key);
            self.presence_mailboxes.remove(&sub.device_public_key);
            if let Some(session) = self.state_sessions.remove(&sub.device_public_key) {
                session
                    .live
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
            if self.subscribers.contains_key(&sub.device_public_key) {
                // The displaced connection is no longer the device's current
                // session; its next access audit ends it.
                crate::authority_epoch::bump();
            }
        }
        self.checkpoint_served
            .insert(sub.device_public_key, HashSet::new());
        // Its replay holds every chain's head.
        self.held_records.insert(
            sub.device_public_key,
            self.store.chain_heads().into_iter().collect(),
        );
        self.subscribers.insert(sub.device_public_key, sub);
    }

    /// Whether subscriber `device` holds the on-demand record on `channel`.
    pub(crate) fn holds_record(&self, device: &[u8; 32], channel: &[u8; 32]) -> bool {
        self.held_records
            .get(device)
            .is_some_and(|held| held.contains(channel))
    }

    /// Subscriber `device` now holds the record on `channel`: it opened it,
    /// or took a live change to it while it was its chain's head.
    pub(crate) fn hold_record(&mut self, device: &[u8; 32], channel: [u8; 32]) {
        if let Some(held) = self.held_records.get_mut(device) {
            held.insert(channel);
        }
    }

    /// Hand the presence fanned out to subscriber `device` to its
    /// connection's `mailbox` rather than its frame queue. Call right after
    /// [`Self::add_subscriber`] registered that connection; replacing or
    /// removing the subscription drops the route.
    pub fn route_presence(
        &mut self,
        device: [u8; 32],
        mailbox: Arc<crate::presence_mailbox::PresenceMailbox>,
    ) {
        if self.subscribers.contains_key(&device) {
            self.presence_mailboxes.insert(device, mailbox);
        }
    }

    /// A device asked for a baseline (its hello, a `SyncReq`, or a rejected
    /// checkpoint arrive as presence): the next live checkpoint on every
    /// channel reaches it once more.
    pub fn request_checkpoint(&mut self, device: &[u8; 32]) {
        if self.subscribers.contains_key(device) {
            self.checkpoint_served.insert(*device, HashSet::new());
        }
    }

    /// Whether a live checkpoint for `channel` would currently be fanned to
    /// `device`.
    pub fn needs_checkpoint(&self, device: &[u8; 32], channel: &[u8; 32]) -> bool {
        self.checkpoint_served
            .get(device)
            .is_some_and(|served| !served.contains(channel))
    }

    /// Record that one live checkpoint reached the current device session.
    /// Call only after the delivery queue accepted the checkpoint.
    pub(crate) fn mark_checkpoint_served(&mut self, device: &[u8; 32], channel: [u8; 32]) {
        if let Some(served) = self.checkpoint_served.get_mut(device) {
            served.insert(channel);
        }
    }

    /// Whether `membership` reflects the files on disk without reading them:
    /// a live watch has seen no change to them since they were last loaded.
    pub(crate) fn membership_is_current(&self) -> bool {
        self.loaded_generation.is_some()
            && self.loaded_generation
                == self
                    .disk_generation
                    .as_ref()
                    .and_then(crate::membership_watch::DiskGeneration::current)
    }

    /// Remember an authenticated device before sharing this runtime, so deltas
    /// received while it is offline are queued across daemon restarts. Live
    /// sessions use the serialized async admission in `workspace_io`.
    ///
    /// # Errors
    ///
    /// Returns an error if persisting the updated membership list fails.
    pub fn admit_device(&mut self, device: [u8; 32]) -> Result<(), crate::store::StoreError> {
        if !self.membership.contains(&device) {
            let mut devices = self.membership.devices.clone();
            devices.push(device);
            self.store.save_membership(&devices)?;
            self.membership.set(devices);
        }
        Ok(())
    }

    /// Drop a subscription for `device`, and its live membership. Idempotent.
    pub fn remove_subscriber(&mut self, device: &[u8; 32]) {
        if self.subscribers.remove(device).is_some() {
            // A connection whose subscription went is ended by its next
            // access audit.
            crate::authority_epoch::bump();
        }
        self.action_targets.remove(device);
        self.presence_mailboxes.remove(device);
        if let Some(session) = self.state_sessions.remove(device) {
            session
                .live
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        self.checkpoint_served.remove(device);
        self.held_records.remove(device);
        let workspace_id = self.store.workspace_id();
        self.live_leave(device, workspace_id);
    }

    /// Remove only the session that registered this queue. An old connection
    /// must not unregister its replacement when its delayed teardown runs.
    pub(crate) fn remove_subscriber_session(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) {
        if self
            .subscribers
            .get(device)
            .is_some_and(|sub| sub.tx.same_channel(tx))
        {
            self.remove_subscriber(device);
        }
    }

    /// Whether `device` currently holds a subscription here.
    pub fn has_subscriber(&self, device: &[u8; 32]) -> bool {
        self.subscribers.contains_key(device)
    }

    /// Tell live devices that just lost membership they are no longer
    /// authorised, while their connection is still open. Removals publish
    /// this before durable IO so a concurrent audit cannot close the session
    /// without an authenticated refusal. The subscriber is then unregistered
    /// so a follow-up write closes instead of emitting a second refusal.
    pub(crate) fn refuse_removed_live_members(&mut self, removed: &[[u8; 32]]) {
        if removed.is_empty() {
            return;
        }
        let workspace_id = self.store.workspace_id();
        for device in removed {
            let Some(sub) = self.subscribers.get(device) else {
                continue;
            };
            let _ = sub.tx.try_send(Envelope::NotAuthorised {
                workspace_id,
                reason: "workspace membership changed".into(),
            });
        }
        for device in removed {
            if self.subscribers.contains_key(device) {
                self.remove_subscriber(device);
            }
        }
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
    ) -> Result<usize, crate::store::StoreError> {
        use tokio::sync::mpsc::error::TrySendError;
        // Shared retained coverage is the durable delivery obligation for
        // every reconnect (including newly admitted devices). Commit before
        // live publication, and never duplicate these payloads per member.
        let raw = crate::transport::encode(envelope).map_err(|error| {
            crate::store::StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encode: {error}"),
            ))
        })?;
        let retained = if let Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        } = envelope
        {
            if workspace_id != &self.store.workspace_id() || author_device_public_key != author {
                return Err(crate::store::StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "delta workspace or authenticated author mismatch",
                )));
            }
            crate::coedit::retention_metadata(delta)
        } else {
            None
        };
        match &retained {
            Some((channel, crate::coedit::RetentionKind::Checkpoint)) => {
                self.store.retain_checkpoint(channel, &raw)?;
            }
            Some((channel, crate::coedit::RetentionKind::Delta)) => {
                self.store.retain_delta(channel, seq, &raw)?;
            }
            None => {}
        }
        // 1. Live fan-out. A FULL queue is backpressure, never a disconnect:
        //    the durable delta spills to the slow subscriber's
        //    store-and-forward queue below and the subscriber stays connected.
        //    Only a CLOSED channel (the receiving task is gone) prunes the
        //    subscriber. Dropping slow subscribers here caused the native host
        //    to be disconnected mid-checkpoint, reconnect, receive the room's
        //    retained baselines as a replay, republish, and storm.
        // A retained checkpoint is a bootstrap/compaction artifact. A
        // device that is connected and current holds every delta already, so
        // the native's periodic (idle) full checkpoint — multi-megabyte, and a
        // >100 ms frame for a browser that must rebuild its room doc — is fanned
        // live to a device only while that device needs one: on subscribe (its
        // retained replay may be stale) and after it asks again via presence
        // (hello / SyncReq / rejected checkpoint). Peer relays always receive
        // it so they can federate; retention and offline queues are unchanged.
        let checkpoint_channel = match &retained {
            Some((channel, crate::coedit::RetentionKind::Checkpoint)) => Some(*channel),
            _ => None,
        };
        let mut delivered = 0usize;
        let mut dead: Vec<[u8; 32]> = Vec::new();
        let mut backpressured: Vec<[u8; 32]> = Vec::new();
        let mut satisfied: Vec<[u8; 32]> = Vec::new();
        for (peer, sub) in &self.subscribers {
            if peer == author {
                continue;
            }
            // Federation echo-suppression: a message originating on a
            // peer relay must not bounce onto other peer relays.
            if from_role == PeerRole::PeerRelay && sub.role == PeerRole::PeerRelay {
                continue;
            }
            let gated_checkpoint = checkpoint_channel.is_some() && sub.role == PeerRole::Device;
            if let Some(channel) = checkpoint_channel
                && sub.role == PeerRole::Device
                && !self.needs_checkpoint(peer, &channel)
            {
                continue;
            }
            match sub.tx.try_send(envelope.clone()) {
                Ok(()) => {
                    delivered = delivered.saturating_add(1);
                    if gated_checkpoint {
                        satisfied.push(*peer);
                    }
                }
                // A full queue leaves the need in place: the next checkpoint
                // (or the pending spill below) still serves it.
                Err(TrySendError::Full(_)) => backpressured.push(*peer),
                Err(TrySendError::Closed(_)) => dead.push(*peer),
            }
        }
        for d in dead {
            self.remove_subscriber(&d);
        }
        if let Some(channel) = checkpoint_channel {
            for d in satisfied {
                self.mark_checkpoint_served(&d, channel);
            }
        }
        // 2. Store-and-forward for offline members AND for live subscribers
        //    the queue could not accept this instant. A member is any pubkey in
        //    `membership` that is not the author and not currently draining
        //    live; a backpressured subscriber is treated as briefly offline for
        //    this one delta and reconciles it on its next reconnect or
        //    anti-entropy, so no durable delta is lost to a slow consumer.
        if retained.is_none() {
            let queue_for: Vec<[u8; 32]> = self
                .membership
                .devices
                .iter()
                .filter(|member| **member != *author && !self.subscribers.contains_key(*member))
                .chain(backpressured.iter())
                .copied()
                .collect();
            for member in &queue_for {
                self.store.enqueue_pending(member, seq, &raw)?;
            }
        }
        // 3. Store-and-forward for offline peer relays — reuses the
        //    same `store.enqueue_pending` path keyed by device pubkey.
        //    Suppressed when the origin is itself a peer relay (echo
        //    prevention across a chain of federated relays).
        if retained.is_none() && from_role == PeerRole::Device {
            for pr in &self.known_peer_relays {
                if pr == author {
                    continue;
                }
                if self.subscribers.contains_key(pr) {
                    continue;
                }
                self.store.enqueue_pending(pr, seq, &raw)?;
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
        if !is_automerge_chunk(delta_bytes) {
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

    /// Note that `author` sent a delta at `now`, before it is admitted: the
    /// caret and typing it sends next are held to go with it
    /// ([`Self::fanout_presence`]) until it is published.
    pub fn note_delta(&mut self, author: [u8; 32], now: tokio::time::Instant) {
        let hold = crate::presence_mailbox::DELTA_HOLD;
        self.recent_deltas
            .retain(|_, at| now.saturating_duration_since(*at) < hold);
        self.recent_deltas.insert(author, now);
    }

    /// `author`'s delta is being fanned out: what it sends from now on has
    /// nothing to wait for.
    pub fn delta_published(&mut self, author: &[u8; 32]) {
        self.recent_deltas.remove(author);
    }

    /// Route a presence envelope (no persistence, drop-on-empty).
    /// Same echo-suppression rule as `fanout_delta`. A subscriber whose
    /// connection has a presence mailbox gets it there, coalesced under
    /// `slot` with the author's later presence; the payload is shared by every
    /// such subscriber, not copied. Presence from an author who sent a delta
    /// within [`crate::presence_mailbox::DELTA_HOLD`] is posted held, to go
    /// with that delta.
    pub fn fanout_presence(
        &mut self,
        author: &[u8; 32],
        from_role: PeerRole,
        envelope: &Envelope,
        slot: Option<neighbour_wire::PresenceSlot>,
    ) -> usize {
        let mut delivered = 0usize;
        let mut dead: Vec<[u8; 32]> = Vec::new();
        let mut shared: Option<Arc<[u8]>> = None;
        let now = tokio::time::Instant::now();
        let held = self.recent_deltas.get(author).is_some_and(|at| {
            now.saturating_duration_since(*at) < crate::presence_mailbox::DELTA_HOLD
        });
        for (peer, sub) in &self.subscribers {
            if peer == author {
                continue;
            }
            if from_role == PeerRole::PeerRelay && sub.role == PeerRole::PeerRelay {
                continue;
            }
            if let (
                Some(mailbox),
                Envelope::Presence {
                    workspace_id,
                    payload,
                },
            ) = (self.presence_mailboxes.get(peer), envelope)
            {
                let payload = shared
                    .get_or_insert_with(|| Arc::from(payload.as_slice()))
                    .clone();
                if mailbox.post(*workspace_id, *author, slot, payload, now, held) {
                    delivered = delivered.saturating_add(1);
                }
                continue;
            }
            match sub.tx.try_send(envelope.clone()) {
                Ok(()) => delivered = delivered.saturating_add(1),
                // Presence is ephemeral: a full queue simply drops this frame
                // (backpressure), it never disconnects the subscriber. A closed
                // channel means the task is gone, so prune it.
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => dead.push(*peer),
            }
        }
        for d in dead {
            self.remove_subscriber(&d);
        }
        delivered
    }

    /// Deliver a directed envelope to the live subscriber `target` only.
    /// Never queued: a directed request is live traffic. Returns whether it
    /// was handed to that subscriber's connection.
    pub fn route_directed(&mut self, target: &[u8; 32], envelope: Envelope) -> bool {
        let Some(sub) = self.subscribers.get(target) else {
            return false;
        };
        if sub.tx.try_send(envelope).is_ok() {
            return true;
        }
        let _ = sub.disconnect.try_send(());
        self.subscribers.remove(target);
        crate::authority_epoch::bump();
        false
    }

    /// Deliver a directed live envelope to the subscriber `target` only, or
    /// drop it when that subscriber's queue is full: like presence, a busy
    /// reader loses the envelope, never its connection.
    pub fn route_lossy(&self, target: &[u8; 32], envelope: Envelope) -> bool {
        self.subscribers
            .get(target)
            .is_some_and(|sub| sub.tx.try_send(envelope).is_ok())
    }

    /// The outbound queue of the live subscriber `target`, for a caller
    /// that waits for room in it rather than dropping what does not fit.
    pub fn subscriber_queue(&self, target: &[u8; 32]) -> Option<mpsc::Sender<Envelope>> {
        self.subscribers.get(target).map(|sub| sub.tx.clone())
    }
}

/// Shorthand: a workspace runtime held under an async mutex.
pub type SharedWorkspace = Arc<AsyncMutex<WorkspaceRuntime>>;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#[allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
mod tests {
    use super::*;
    use crate::transport::Envelope;

    /// Only a membership that leaves a device out narrows anyone's access,
    /// and so wakes the sessions admitted under it.
    #[test]
    fn only_a_membership_that_leaves_a_device_out_narrows() {
        assert!(!leaves_out(&[[1; 32]], &[[1; 32], [2; 32]]));
        assert!(!leaves_out(&[[1; 32], [2; 32]], &[[2; 32], [1; 32]]));
        assert!(!leaves_out(&[], &[[3; 32]]));
        assert!(leaves_out(&[[1; 32], [2; 32]], &[[2; 32], [3; 32]]));
        assert!(leaves_out(&[[1; 32]], &[]));
    }

    /// An auto-bound workspace is pinned, and every delta a session sends is
    /// a co-edit wrapper: routing one must not reach Automerge's parser,
    /// which logs each chunk it rejects at error level.
    #[test]
    fn co_edit_payload_logs_no_automerge_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tracing_subscriber::layer::SubscriberExt as _;

        struct Errors(Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Errors {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() == tracing::Level::ERROR {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 57, true, Membership::default()).unwrap();
        let payload = crate::coedit::CoeditPayload::Checkpoint {
            channel: [3; 32],
            body: b"opaque".to_vec(),
        }
        .encode()
        .unwrap();
        let errors = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Errors(Arc::clone(&errors)));
        tracing::subscriber::with_default(subscriber, || {
            rt.fanout_delta(
                &[6u8; 32],
                PeerRole::Device,
                &env_delta(57, [6u8; 32]),
                &payload,
                1,
            )
            .unwrap();
        });
        assert_eq!(errors.load(Ordering::SeqCst), 0);
        assert!(rt.current_heads().is_empty());
    }

    #[test]
    fn the_commit_ledger_is_durable_and_forgets_its_oldest_commits_first() {
        let tmp = tempfile::tempdir().unwrap();
        let record = |n: u64| CommitRecord {
            author: [1; 32],
            op: {
                let mut op = [0; 16];
                op[..8].copy_from_slice(&n.to_le_bytes());
                op
            },
            seq: n,
            digest: [2; 32],
        };
        let mut ws = WorkspaceRuntime::open(tmp.path(), 5, false, Membership::default()).unwrap();
        let total = MAX_REMEMBERED_COMMITS as u64 * 2 + 10;
        for n in 1..=total {
            ws.remember_commit(record(n)).persist(&ws.store);
        }
        drop(ws);
        let ws = WorkspaceRuntime::open(tmp.path(), 5, false, Membership::default()).unwrap();
        let oldest_kept = total - MAX_REMEMBERED_COMMITS as u64 + 1;
        assert_eq!(
            ws.admitted_commit(&[1; 32], &record(total).op),
            Some(record(total))
        );
        assert_eq!(
            ws.admitted_commit(&[1; 32], &record(oldest_kept).op),
            Some(record(oldest_kept))
        );
        assert_eq!(
            ws.admitted_commit(&[1; 32], &record(oldest_kept - 1).op),
            None
        );
        assert!(
            ws.store.load_commits().unwrap().len() <= MAX_REMEMBERED_COMMITS * 2,
            "the on-disk ledger is compacted"
        );
    }

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
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn fanout_reaches_live_subscribers_and_queues_for_offline_ones() {
        // Phase `fanout_excludes_author_and_enqueues_offline`.
        {
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
            assert_eq!(delivered, 1);
            // Peer 2 saw the delta live.
            assert_eq!(rx2.try_recv().unwrap(), env);
            // Peer 3 has one pending delta on disk.
            let peer3 = [3u8; 32];
            assert_eq!(rt.store.count_pending(&peer3).unwrap(), 1);
        }
        // Phase `fanout_prunes_dead_subscribers`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut rt =
                WorkspaceRuntime::open(tmp.path(), 5, false, Membership::default()).unwrap();
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
            assert_eq!(delivered, 0);
            assert!(rt.subscribers.is_empty());
        }
        // Phase `fanout_from_peer_relay_skips_other_peer_relays`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut rt =
                WorkspaceRuntime::open(tmp.path(), 8, false, Membership::default()).unwrap();
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
            assert_eq!(delivered, 1);
            assert_eq!(rx_dev.try_recv().unwrap(), env);
            assert!(rx_pr.try_recv().is_err(), "peer relay must not echo");
        }
        // Phase `known_peer_relay_gets_pending_when_offline_and_origin_is_device`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut rt =
                WorkspaceRuntime::open(tmp.path(), 12, false, Membership::default()).unwrap();
            let pr_pk = [7u8; 32];
            rt.record_peer_relay(pr_pk);
            // A local device authors — pr is offline, should be queued.
            let author = [1u8; 32];
            let env = env_delta(12, author);
            rt.fanout_delta(&author, PeerRole::Device, &env, b"x", 42)
                .unwrap();
            assert_eq!(rt.store.count_pending(&pr_pk).unwrap(), 1);
        }
        // Phase `known_peer_relay_pending_suppressed_when_origin_is_peer_relay`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut rt =
                WorkspaceRuntime::open(tmp.path(), 13, false, Membership::default()).unwrap();
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
    }

    #[test]
    fn materialized_deltas_are_served_and_recovered_by_anti_entropy() {
        // Phase `materialize_pins_delta_and_serves_it_via_changes_since`.
        // A pinned host materializes an incoming raw Automerge delta into
        // its doc and can then serve it back via `changes_since` — the
        // source of truth for anti-entropy. A fresh peer converges from
        // that alone.
        {
            use automerge::transaction::Transactable;
            use automerge::{AutoCommit, ROOT};

            let tmp = tempfile::tempdir().unwrap();
            let mut rt =
                WorkspaceRuntime::open(tmp.path(), 55, true, Membership::default()).unwrap();

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
        // Phase `materialize_skips_non_automerge_payload`.
        // A non-Automerge payload (the byte-blob deltas some peers publish,
        // or a co-edit wrapper) must not corrupt or advance the pinned doc,
        // and must not write a snapshot — materialization is best-effort and
        // isolated from routing.
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut rt =
                WorkspaceRuntime::open(tmp.path(), 56, true, Membership::default()).unwrap();
            let env = env_delta(56, [5u8; 32]);
            // fanout still succeeds (routing is unaffected)...
            rt.fanout_delta(&[5u8; 32], PeerRole::Device, &env, b"not-automerge", 1)
                .unwrap();
            // ...but the doc did not advance and no snapshot was written.
            assert!(rt.current_heads().is_empty());
            assert!(rt.store.load_snapshot().unwrap().is_none());
            assert!(rt.changes_since(&[]).is_empty());
        }
        // Phase `anti_entropy_recovers_delta_lost_in_half_closed_channel_race`.
        // The previously-unrecoverable disconnect race, at the runtime
        // level: a delta fanned out to a *live* subscriber whose channel
        // then dies before the write lands. Because the subscriber was
        // live, the delta is counted delivered and is NEVER written to its
        // pending queue — so the store-and-forward tier has nothing to
        // recover from. Anti-entropy against the host's materialized doc
        // reconciles the peer to convergence anyway.
        //
        // This exercises the real `WorkspaceRuntime` (`fanout_delta`,
        // `changes_since`, the production mpsc subscriber channel) — no
        // mock of the code under test.
        {
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
            doc_b.load_incoming(&delta_since).unwrap();

            assert_eq!(
                doc_b.get_heads(),
                doc_a.get_heads(),
                "anti-entropy reconciles B to convergence despite the lost fan-out and empty queue"
            );
        }
    }

    fn retained_envelope(
        author: [u8; 32],
        channel: [u8; 32],
        checkpoint: bool,
        value: u8,
    ) -> Envelope {
        let body = vec![value; 4096];
        let payload = if checkpoint {
            crate::coedit::CoeditPayload::Checkpoint { channel, body }
        } else {
            crate::coedit::CoeditPayload::RetainedDelta { channel, body }
        };
        Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: author,
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        }
    }

    fn publish(rt: &mut WorkspaceRuntime, env: &Envelope, seq: u64) {
        let Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = env
        else {
            panic!("delta")
        };
        rt.fanout_delta(author_device_public_key, PeerRole::Device, env, delta, seq)
            .unwrap();
    }

    fn disk_usage(path: &std::path::Path) -> (u64, usize) {
        let mut total = (0_u64, 0_usize);
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let (bytes, files) = if path.is_dir() {
                disk_usage(&path)
            } else {
                (path.metadata().unwrap().len(), 1)
            };
            total.0 = total.0.saturating_add(bytes);
            total.1 = total.1.saturating_add(files);
        }
        total
    }

    #[test]
    fn retained_growth_is_independent_of_obsolete_checkpoints_and_offline_peers() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let membership = Membership {
            devices: (10..42).map(|key| [key; 32]).collect(),
        };
        let mut rt = WorkspaceRuntime::open(tmp.path(), 7, false, membership).unwrap();
        let other_author = retained_envelope([2; 32], [3; 32], true, 201);
        let other_channel = retained_envelope([1; 32], [4; 32], true, 202);
        publish(&mut rt, &other_author, 1);
        publish(&mut rt, &other_channel, 2);
        let opaque = env_delta(7, [1; 32]);
        publish(&mut rt, &opaque, 3);
        let mut document = AutoCommit::new();
        document.set_actor(automerge::ActorId::from(b"retained-growth".as_slice()));
        document
            .put(automerge::ROOT, "payload", "x".repeat(4096))
            .unwrap();
        let mut latest = Vec::new();
        for round in 1..=24u8 {
            document
                .put(automerge::ROOT, "checkpoint", u64::from(round))
                .unwrap();
            let checkpoint = crate::coedit::CoeditPayload::Checkpoint {
                channel: [3; 32],
                body: document.save(),
            };
            document
                .put(automerge::ROOT, "tail", u64::from(round))
                .unwrap();
            let tail = crate::coedit::CoeditPayload::RetainedDelta {
                channel: [3; 32],
                body: document.save_incremental(),
            };
            latest.clear();
            for (offset, payload) in [checkpoint, tail].into_iter().enumerate() {
                let envelope = Envelope::Delta {
                    workspace_id: 7,
                    author_device_public_key: [1; 32],
                    authored_at_ns: u64::from(round),
                    delta: payload.encode().unwrap(),
                    signature: [0; 64],
                };
                publish(
                    &mut rt,
                    &envelope,
                    10 + u64::from(round) * 2 + offset as u64,
                );
                latest.push(envelope);
            }
        }
        let (bytes, files) = disk_usage(rt.store.root());
        eprintln!("retained growth fixture: {bytes} logical bytes in {files} files");
        assert!(
            bytes < 100_000 && files < 100,
            "retained payload amplification: {bytes} bytes in {files} files"
        );
        let retained = rt.store.load_retained().unwrap();
        assert_eq!(retained.len(), 4);
        for env in [other_author, other_channel].into_iter().chain(latest) {
            assert!(retained.contains(&crate::transport::encode(&env).unwrap()));
        }
        for peer in &rt.membership.devices {
            assert_eq!(
                rt.store.drain_pending(peer).unwrap(),
                vec![crate::transport::encode(&opaque).unwrap()]
            );
        }
    }

    #[test]
    fn opaque_retained_history_is_shared_without_claiming_checkpoint_supersedence() {
        let mut sizes = Vec::new();
        for offline_peers in [1, 32] {
            let tmp = tempfile::tempdir().unwrap();
            let membership = Membership {
                devices: (10..10 + offline_peers).map(|key| [key; 32]).collect(),
            };
            let mut runtime = WorkspaceRuntime::open(tmp.path(), 7, false, membership).unwrap();
            for round in 1..=24u8 {
                publish(
                    &mut runtime,
                    &retained_envelope([1; 32], [3; 32], true, round),
                    u64::from(round),
                );
            }
            assert_eq!(
                runtime.store.load_retained().unwrap().len(),
                24,
                "unknown checkpoints have no causal supersedence proof"
            );
            for peer in &runtime.membership.devices {
                assert_eq!(runtime.store.count_pending(peer).unwrap(), 0);
            }
            sizes.push(disk_usage(runtime.store.root()));
        }
        assert_eq!(
            sizes[0], sizes[1],
            "opaque retained storage multiplied by offline membership"
        );
        eprintln!(
            "opaque retained fixture: {} logical bytes in {} files for either 1 or 32 offline peers",
            sizes[0].0, sizes[0].1
        );
    }

    #[test]
    fn failed_retained_commit_never_publishes_to_live_subscriber() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 7, false, Membership::default()).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let (disconnect, _) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: [8; 32],
            role: PeerRole::Device,
            tx,
            disconnect,
        });
        std::fs::remove_dir_all(rt.store.root().join("retained")).unwrap();
        std::fs::write(rt.store.root().join("retained"), b"blocked").unwrap();
        let env = retained_envelope([1; 32], [3; 32], true, 1);
        let Envelope::Delta { delta, .. } = &env else {
            panic!("delta")
        };
        assert!(
            rt.fanout_delta(&[1; 32], PeerRole::Device, &env, delta, 1)
                .is_err()
        );
        assert!(
            rx.try_recv().is_err(),
            "uncommitted checkpoint reached live queue"
        );
    }

    #[test]
    fn fanout_backpressures_saturated_subscriber_without_dropping_it() {
        // A full live queue is backpressure, never a disconnect: a
        // slow subscriber (a browser on a busy main thread, or the native host
        // publishing a large checkpoint) must stay subscribed while the missed
        // durable delta spills to its store-and-forward queue. Dropping it here
        // caused the reconnect -> retained-replay -> republish storm.
        let tmp = tempfile::tempdir().unwrap();
        let author = [1u8; 32];
        let slow_peer = [2u8; 32];
        let mut membership = Membership::default();
        membership.set(vec![author, slow_peer]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 6, false, membership).unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let (disconnect, mut disconnect_rx) = mpsc::channel(1);
        // Saturate the one-slot live queue so the next fan-out cannot be sent.
        tx.try_send(env_delta(6, [9u8; 32])).unwrap();
        rt.add_subscriber(Subscriber {
            device_public_key: slow_peer,
            role: PeerRole::Device,
            tx,
            disconnect,
        });

        // Fan out several durable deltas past the queue bound.
        for seq in 2..5 {
            rt.fanout_delta(
                &author,
                PeerRole::Device,
                &env_delta(6, author),
                b"durable",
                seq,
            )
            .unwrap();
        }

        // Never dropped, never told to disconnect, and every missed durable
        // delta is preserved in the subscriber's pending queue.
        assert!(
            rt.subscribers.contains_key(&slow_peer),
            "a slow subscriber must not be dropped within its queue bound"
        );
        assert!(
            disconnect_rx.try_recv().is_err(),
            "a full queue must apply backpressure, never signal a disconnect"
        );
        assert_eq!(
            rt.store.count_pending(&slow_peer).unwrap(),
            3,
            "each durable delta the slow subscriber missed live is store-and-forwarded"
        );
    }

    fn checkpoint_envelope(author: [u8; 32], seq: u64, body: Vec<u8>) -> (Envelope, Vec<u8>) {
        checkpoint_on(author, seq, [9u8; 32], body)
    }

    fn checkpoint_on(
        author: [u8; 32],
        seq: u64,
        channel: [u8; 32],
        body: Vec<u8>,
    ) -> (Envelope, Vec<u8>) {
        let raw = crate::coedit::CoeditPayload::Checkpoint { channel, body }
            .encode()
            .unwrap();
        (
            Envelope::Delta {
                workspace_id: 6,
                author_device_public_key: author,
                authored_at_ns: seq,
                delta: raw.clone(),
                signature: [7u8; 64],
            },
            raw,
        )
    }

    fn retained_delta_envelope(author: [u8; 32], seq: u64) -> (Envelope, Vec<u8>) {
        let raw = crate::coedit::CoeditPayload::RetainedDelta {
            channel: [9u8; 32],
            body: vec![4, 5],
        }
        .encode()
        .unwrap();
        (
            Envelope::Delta {
                workspace_id: 6,
                author_device_public_key: author,
                authored_at_ns: seq,
                delta: raw.clone(),
                signature: [7u8; 64],
            },
            raw,
        )
    }

    #[test]
    fn live_checkpoint_reaches_a_device_once_per_request_then_stops() {
        // A connected device already holds every delta, so the
        // native's idle checkpoint (2-3 MB, thousands of frames, a 124 ms frame
        // when measured during pointer input) must not be fanned to it live. A device
        // needs a live checkpoint exactly when it subscribes (its retained
        // replay may be stale) and after it asks again; each request is served
        // by one checkpoint, and the flag clears once one has been fanned.
        let tmp = tempfile::tempdir().unwrap();
        let author = [1u8; 32];
        let device = [2u8; 32];
        let mut membership = Membership::default();
        membership.set(vec![author, device]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 6, false, membership).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let (disconnect, _drx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: device,
            role: PeerRole::Device,
            tx,
            disconnect,
        });
        let (room, record) = ([9u8; 32], [10u8; 32]);
        assert!(
            rt.needs_checkpoint(&device, &room),
            "a fresh subscriber needs its first live checkpoint"
        );
        assert!(rt.needs_checkpoint(&device, &record));

        // The native publishes its checkpoint as one message per channel (the
        // room plus each record); the whole first set after subscribe passes.
        let (env, raw) = checkpoint_on(author, 2, room, vec![1]);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 2)
                .unwrap(),
            1
        );
        let (env, raw) = checkpoint_on(author, 3, record, vec![1]);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 3)
                .unwrap(),
            1
        );
        assert!(
            rx.try_recv().is_ok() && rx.try_recv().is_ok(),
            "every channel of the first set is delivered live"
        );
        assert!(
            !rt.needs_checkpoint(&device, &room),
            "one fanned checkpoint serves its channel"
        );
        assert!(!rt.needs_checkpoint(&device, &record));

        // The next (idle) set on the same channels is NOT fanned to the still-connected device.
        let (env, raw) = checkpoint_on(author, 4, room, vec![2]);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 4)
                .unwrap(),
            0
        );
        let (env, raw) = checkpoint_on(author, 5, record, vec![2]);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 5)
                .unwrap(),
            0
        );
        assert!(
            rx.try_recv().is_err(),
            "an idle checkpoint must not reach a device that holds every delta"
        );
        assert!(
            rt.subscribers.contains_key(&device),
            "the device stays subscribed"
        );
        // The synthetic bodies are opaque rather than valid Automerge documents,
        // so bounded retention preserves both versions without inventing causal
        // supersedence. The live fanout gate remains one checkpoint per channel.
        assert_eq!(rt.store.load_retained().unwrap().len(), 4);
        // A checkpoint on a channel this device has never been served passes.
        let (env, raw) = checkpoint_on(author, 6, [11u8; 32], vec![3]);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 6)
                .unwrap(),
            1
        );
        rx.try_recv().unwrap();

        // Retained deltas keep flowing live regardless.
        let (env, raw) = retained_delta_envelope(author, 4);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 4)
                .unwrap(),
            1
        );
        assert!(
            rx.try_recv().is_ok(),
            "retained deltas are always delivered live"
        );

        // The device asks again (a presence frame: hello / SyncReq / rejected
        // checkpoint): one more checkpoint per channel is delivered, then it stops.
        rt.request_checkpoint(&device);
        assert!(rt.needs_checkpoint(&device, &room));
        let (env, raw) = checkpoint_on(author, 7, room, vec![3]);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 7)
                .unwrap(),
            1
        );
        assert!(
            rx.try_recv().is_ok(),
            "a requested checkpoint is delivered once"
        );
        let (env, raw) = checkpoint_on(author, 8, room, vec![4]);
        assert_eq!(
            rt.fanout_delta(&author, PeerRole::Device, &env, &raw, 8)
                .unwrap(),
            0
        );
        assert!(
            rx.try_recv().is_err(),
            "the request is satisfied by one checkpoint per channel"
        );
    }

    #[test]
    fn bootstrap_stale_retained_then_sync_req_gets_exactly_one_live_checkpoint() {
        // The bootstrap path the browser actually takes: it subscribes, the
        // subscribe-time retained replay is stale (>300 s attestation window)
        // and rejected, so it sends a SyncReq as presence; the native answers
        // with a fresh checkpoint that must reach it live — once. Model the
        // stale replay as the first (unsatisfying) checkpoint having already
        // been consumed by the subscribe, i.e. the flag cleared, then presence.
        let tmp = tempfile::tempdir().unwrap();
        let native = [1u8; 32];
        let browser = [2u8; 32];
        let mut membership = Membership::default();
        membership.set(vec![native, browser]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 6, false, membership).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let (disconnect, _drx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: browser,
            role: PeerRole::Device,
            tx,
            disconnect,
        });
        let (room, record) = ([9u8; 32], [10u8; 32]);
        // The stale baseline set was replayed on subscribe and rejected by the browser.
        for (seq, ch) in [(1u64, room), (2, record)] {
            let (env, raw) = checkpoint_on(native, seq, ch, b"stale".to_vec());
            rt.fanout_delta(&native, PeerRole::Device, &env, &raw, seq)
                .unwrap();
            rx.try_recv().unwrap();
        }
        assert!(!rt.needs_checkpoint(&browser, &room) && !rt.needs_checkpoint(&browser, &record));
        // Browser: "checkpoint does not match" -> SyncReq (a presence frame).
        rt.request_checkpoint(&browser);
        // Native: refresh + publish a fresh SET (room + record) -> the whole set is delivered live, once.
        for (seq, ch) in [(3u64, room), (4, record)] {
            let (env, raw) = checkpoint_on(native, seq, ch, b"fresh".to_vec());
            assert_eq!(
                rt.fanout_delta(&native, PeerRole::Device, &env, &raw, seq)
                    .unwrap(),
                1,
                "every channel of the fresh set reaches the requester"
            );
            let Envelope::Delta { delta, .. } = rx.try_recv().unwrap() else {
                panic!("delta")
            };
            assert!(
                matches!(crate::coedit::CoeditPayload::decode(&delta), Some(crate::coedit::CoeditPayload::Checkpoint { body, .. }) if body == b"fresh")
            );
        }
        // The native's next idle set is not fanned to the now-current browser.
        for (seq, ch) in [(5u64, room), (6, record)] {
            let (env, raw) = checkpoint_on(native, seq, ch, b"idle".to_vec());
            assert_eq!(
                rt.fanout_delta(&native, PeerRole::Device, &env, &raw, seq)
                    .unwrap(),
                0
            );
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn peer_relays_keep_receiving_every_checkpoint_live() {
        // Federation is unchanged: a peer relay must be able to forward every
        // checkpoint onward, so the device-only gate does not apply to it.
        let tmp = tempfile::tempdir().unwrap();
        let author = [1u8; 32];
        let relay_peer = [3u8; 32];
        let mut membership = Membership::default();
        membership.set(vec![author, relay_peer]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 6, false, membership).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let (disconnect, _drx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: relay_peer,
            role: PeerRole::PeerRelay,
            tx,
            disconnect,
        });
        for seq in 2..5u64 {
            let (env, raw) = checkpoint_envelope(author, seq, vec![u8::try_from(seq).unwrap()]);
            assert_eq!(
                rt.fanout_delta(&author, PeerRole::Device, &env, &raw, seq)
                    .unwrap(),
                1
            );
            assert!(rx.try_recv().is_ok());
        }
    }

    #[test]
    fn fanout_still_prunes_a_subscriber_whose_wire_is_closed() {
        // Backpressure covers a full queue; a genuinely dead receiver (its task
        // and rx dropped) is still pruned so fan-out does not keep cloning into
        // a channel no one will ever read.
        let tmp = tempfile::tempdir().unwrap();
        let author = [1u8; 32];
        let dead_peer = [2u8; 32];
        let mut membership = Membership::default();
        membership.set(vec![author, dead_peer]);
        let mut rt = WorkspaceRuntime::open(tmp.path(), 6, false, membership).unwrap();
        let (tx, rx) = mpsc::channel(1);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        drop(rx); // the receiving task is gone
        rt.add_subscriber(Subscriber {
            device_public_key: dead_peer,
            role: PeerRole::Device,
            tx,
            disconnect,
        });
        rt.fanout_delta(
            &author,
            PeerRole::Device,
            &env_delta(6, author),
            b"durable",
            2,
        )
        .unwrap();
        assert!(!rt.subscribers.contains_key(&dead_peer));
        assert_eq!(rt.store.count_pending(&dead_peer).unwrap(), 1);
    }

    /// A keystroke's caret and typing follow its delta from the same author;
    /// they are held to go with it, and nobody else's are.
    #[test]
    fn presence_after_an_authors_delta_is_held_for_it() {
        use crate::presence_mailbox::{BATCH_WINDOW, DELTA_HOLD, PresenceMailbox};
        use tokio::time::Instant;

        const AUTHOR: [u8; 32] = [1; 32];
        const RECIPIENT: [u8; 32] = [2; 32];
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), 9, false, Membership::default()).unwrap();
        let (tx, _rx) = mpsc::channel(8);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: RECIPIENT,
            role: PeerRole::Device,
            tx,
            disconnect,
        });
        let mailbox = Arc::new(PresenceMailbox::default());
        rt.route_presence(RECIPIENT, Arc::clone(&mailbox));
        let caret = Some(neighbour_wire::PresenceSlot {
            id: 1,
            motion: false,
        });
        let presence = Envelope::Presence {
            workspace_id: 9,
            payload: b"caret".to_vec(),
        };

        // No delta: the caret goes after the batch window.
        assert_eq!(
            rt.fanout_presence(&AUTHOR, PeerRole::Device, &presence, caret),
            1
        );
        assert_eq!(mailbox.take(Instant::now() + BATCH_WINDOW, false).len(), 1);

        // Right after the author's delta: held for it.
        let before = Instant::now();
        rt.note_delta(AUTHOR, before);
        assert_eq!(
            rt.fanout_presence(&AUTHOR, PeerRole::Device, &presence, caret),
            1
        );
        assert!(
            mailbox
                .next_due()
                .is_some_and(|due| due >= before + DELTA_HOLD)
        );
        assert_eq!(
            mailbox.take(Instant::now(), true).len(),
            1,
            "it rides with the delta"
        );

        // Once the delta is published, nothing waits for it.
        rt.note_delta(AUTHOR, Instant::now());
        rt.delta_published(&AUTHOR);
        assert_eq!(
            rt.fanout_presence(&AUTHOR, PeerRole::Device, &presence, caret),
            1
        );
        assert_eq!(mailbox.take(Instant::now() + BATCH_WINDOW, false).len(), 1);

        // Another author's delta holds nothing of this author's.
        rt.note_delta([3; 32], Instant::now());
        let other = Instant::now() - DELTA_HOLD;
        rt.note_delta(AUTHOR, other);
        assert_eq!(
            rt.fanout_presence(&AUTHOR, PeerRole::Device, &presence, caret),
            1
        );
        assert_eq!(mailbox.take(Instant::now() + BATCH_WINDOW, false).len(), 1);
    }

    /// A commit's claim is its own: a copy of it waits for the claim and
    /// then finds it recorded, while another commit is claimed at once.
    #[tokio::test]
    async fn a_commit_claim_holds_off_its_copies_only() {
        const AUTHOR: [u8; 32] = [1; 32];
        let tmp = tempfile::tempdir().unwrap();
        let rt = WorkspaceRuntime::open(tmp.path(), 9, false, Membership::default()).unwrap();
        let claim = rt.claim_commit(AUTHOR, [1; 16]).ok().unwrap();
        let other = rt.claim_commit(AUTHOR, [2; 16]).ok().unwrap();
        let elsewhere = rt.claim_commit([2; 32], [1; 16]).ok().unwrap();
        let Err(copy) = rt.claim_commit(AUTHOR, [1; 16]) else {
            panic!("a copy of a claimed commit was claimed again");
        };
        assert!(
            copy.try_lock().is_err(),
            "a copy must wait while the commit is admitted"
        );
        let waiting = tokio::spawn(async move { drop(copy.lock().await) });
        drop(claim);
        waiting.await.unwrap();
        // Released: the copy (or a later resend) claims it again, now that
        // the ledger is the one to answer it.
        drop(rt.claim_commit(AUTHOR, [1; 16]).ok().unwrap());
        drop((other, elsewhere));
    }
}
