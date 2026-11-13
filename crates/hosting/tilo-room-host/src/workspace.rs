//! Per-workspace routing and authorization metadata. Live state storage and
//! the pinned document transfer lazily to the bounded ordered persistence
//! owner. Synchronous persistence methods remain compatibility helpers for
//! runtimes that have not entered shared live service.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use automerge::AutoCommit;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::store::{CommitRecord, WorkspaceStore};
use crate::transport::Envelope;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

/// Membership set for a workspace on this relay, as the workspace's governance
/// or an explicit admission set it. Peers listed here get
/// store-and-forward; peers not listed get their subscribe rejected with
/// `NotAuthorised`.
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
    /// (`crate::authority_epoch`). One that only adds devices narrows
    /// nobody's access and wakes nobody: a public workspace admitting a crowd
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
    /// Bounded outbound envelope channel. Closed or full means the peer can
    /// no longer keep up and is pruned immediately.
    pub tx: mpsc::Sender<Envelope>,
    /// Signals the owning peer task when this subscriber cannot keep up. A
    /// reconnect replays its durable queue instead of growing RAM without
    /// bound or leaving the connection silently stale.
    pub disconnect: mpsc::Sender<()>,
}

/// The standing of each member device of a workspace, by device key.
pub type Standings = std::collections::BTreeMap<[u8; 32], tilo_action_policy::Standing>;

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// The in-process workspace authority governing this workspace, told of each
    /// retained body a device's admitted delta carries. Bounded; a full queue
    /// drops the notice.
    pub(crate) governor_tap: Option<mpsc::Sender<([u8; 32], Vec<u8>)>>,
    /// The in-process authority governing this workspace, if any: it
    /// answers the workspace's calls.
    pub workspace_authority: Option<crate::workspaces::WorkspaceAuthority>,
    /// This host's replica of a workspace it hosts for a governor elsewhere, if
    /// it hosts this workspace: what a subscribing device builds its
    /// own replica from.
    pub(crate) hosted_replica: Option<std::sync::Arc<crate::hosted::HostedReplica>>,
    /// The standing of each member device, as the workspace's governor projects
    /// it: what the relay decides each change's claimed action against.
    /// `None` until a governor projects one; the relay then decides changes
    /// against their app surface alone.
    pub(crate) standings: Option<Arc<Standings>>,
    /// Woken each time [`Self::standings`] is replaced, for a change held
    /// until its author's standing is projected ([`standings_deciding`]).
    pub(crate) standings_changed: Arc<tokio::sync::Notify>,
    /// On-disk state.
    pub store: WorkspaceStore,
    /// Initial pinned document. Transferred to `persistence` on first live
    /// state operation; live anti-entropy accesses that owner, never this field.
    pub doc: Option<AutoCommit>,
    pub(crate) persistence: Option<crate::persistence::Persistence>,
    /// The workspace left the host's registry ([`crate::workspace_io::evict`]):
    /// its files stay, and this runtime writes them no more.
    pub(crate) evicted: bool,
    /// Which of the user's devices are workspace members. Used to
    /// gate subscribes AND to decide who gets store-and-forward
    /// on delta receive.
    pub membership: Membership,
    /// Serializes filesystem authority operations independently of state IO.
    pub(crate) authorization: Arc<AsyncMutex<()>>,
    /// Failed/pending replacements cap what an old external file may grant.
    pub(crate) membership_ceiling: Option<HashSet<[u8; 32]>>,
    pub(crate) membership_revision: u64,
    /// The daemon's watch on this workspace's membership files, attached on the
    /// first fresh read once a watch covers the state root.
    pub(crate) disk_generation: Option<crate::membership_watch::DiskGeneration>,
    /// The watched generation `membership` was last loaded at. `None` until a
    /// live watch vouched for a load, so every read goes to the disk.
    pub(crate) loaded_generation: Option<u64>,
    /// How the workspace admits devices beyond its exact membership: explicitly,
    /// the relay owner's devices on a same-owner home host, or under native
    /// governance. Read only through [`crate::workspace_access::decide`].
    pub(crate) admission: crate::workspace_access::AdmissionMode,
    /// The people this native-governed workspace has as members, by their
    /// roots, whatever devices of theirs it lists now: the one answer to
    /// whether a person is a member here, which the relay-wide handshake and
    /// the certification queue read. Exact workspace admission still uses
    /// `membership`, so an ungoverned sibling device under the same root
    /// does not join.
    pub governed_user_roots: HashSet<[u8; 32]>,
    /// Current live subscribers keyed by device pubkey.
    pub subscribers: HashMap<[u8; 32], Subscriber>,
    pub outbound_leases: HashMap<[u8; 32], Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) state_sessions: HashMap<[u8; 32], crate::peer::StateSession>,
    /// Device pubkeys of the hosts this workspace is attached to: the hosts
    /// of its person that serve it, recorded as each announces what it
    /// carries (its hello). A change one of them sent is not carried on to
    /// another, and it carries its members' changes under the members' own
    /// keys.
    pub attached_hosts: HashSet<[u8; 32]>,
    /// The session each attached host that dialed here last opened, by its
    /// device: what ends it, so an answer to it ends with it
    /// ([`crate::attach::HostDialer::answer`]).
    pub(crate) attach_sessions: HashMap<[u8; 32], mpsc::Sender<()>>,
    /// What services beside the session stack keep for this workspace (a
    /// relay's compute routing), one value per type.
    pub extensions: Extensions,
    /// Per live device subscriber: the retained-checkpoint channels a live
    /// checkpoint has already been fanned to since the device last asked for a
    /// baseline. A device asks on subscribe and whenever it sends a presence
    /// frame (the browser sends presence only for its hello, a `SyncReq`, and a
    /// rejected checkpoint), which resets its served set. The native publishes
    /// its checkpoint as one set of messages — one per channel (the workspace plus
    /// each record) — so the gate is per channel: a full bootstrap set passes
    /// once, and the next idle set, on the same channels, is withheld from a
    /// device that already holds every delta ([`crate::peer`]'s live fan-out).
    checkpoint_served: HashMap<[u8; 32], HashSet<[u8; 32]>>,
    /// The on-demand records each subscriber holds, by channel: those it
    /// opened, and the chain parts it was replayed or followed live while
    /// they were their chain's head. Live changes to an on-demand record
    /// reach only these ([`crate::store::record_sync`]).
    held_records: HashMap<[u8; 32], HashSet<[u8; 32]>>,
    /// The connections of each device whose subscription was admitted and
    /// has not registered yet, oldest first. A device that reconnects (its
    /// network dropped, its shell re-dialled) subscribes again while the
    /// host still holds its stale connection; the order here says which of
    /// the two the device opened last, so the newer one takes the slot.
    subscribing: HashMap<[u8; 32], Vec<mpsc::WeakSender<Envelope>>>,
    /// Devices admitted through the workspace's door that have not
    /// subscribed yet, with what was committed for them alone meanwhile
    /// (their channel Welcome), which their replay writes first.
    pub(crate) arriving: crate::peer::Arriving,
    /// The presence mailbox of each subscriber whose connection takes its
    /// presence batched ([`crate::presence_mailbox`]). Set
    /// right after the subscription it belongs to and dropped with it; a
    /// subscriber without one gets each presence on its frame queue.
    presence_mailboxes: HashMap<[u8; 32], Arc<crate::presence_mailbox::PresenceMailbox>>,
    /// The devices on this workspace's live plane ([`Self::live_join`]):
    /// each is told of the others as they join and leave, so a device knows
    /// whom its host reaches for content and presence.
    live_members: HashSet<[u8; 32]>,
    /// Host order of the last admitted delta.
    last_seq: u64,
    /// Recently admitted commits, for de-duplicating resends.
    commits: CommitLedger,
    /// Commits being admitted, by author and operation id, each held from
    /// its ledger check until it is admitted and recorded
    /// ([`Self::claim_commit`]): two copies of one commit cannot both be
    /// admitted, while different commits are admitted side by side. One
    /// gate for the whole workspace held every commit through the previous one's
    /// durability and publication, so a busy workspace committed one change at a
    /// time.
    commit_claims: CommitClaims,
    /// Held from a commit's [`Self::remember_commit`] until its ledger write
    /// is done, so ledger writes reach the file in the order they were
    /// remembered.
    pub(crate) commit_ledger_lane: Arc<AsyncMutex<()>>,
}

/// The commits a workspace is admitting, by author and operation id, each with
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
    /// Whether the person whose root is `user` is admitted to this workspace as
    /// a person of its own, on a relay whose owner is `own_user`: a member
    /// its governance names, or the relay owner in a workspace of their own
    /// devices. What [`crate::workspace_access`] admits their devices by.
    #[must_use]
    pub fn admits_person(&self, user: &[u8; 32], own_user: &[u8; 32]) -> bool {
        self.governed_user_roots.contains(user)
            || (self.admission == crate::workspace_access::AdmissionMode::SameOwner
                && user == own_user)
    }

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
        workspace_id: WorkspaceId,
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
        workspace_id: WorkspaceId,
        pinned: bool,
        membership: Membership,
        payloads: std::sync::Arc<dyn crate::payloads::ShellPayloads>,
    ) -> Result<Self, crate::store::StoreError> {
        let store = WorkspaceStore::open_with_payloads(state_root, workspace_id, pinned, payloads)?;
        let governed_access_record = store.load_governed_access()?;
        // A record governance owns keeps the workspace governed; one that
        // only lists the devices this host enrolled leaves the host's own rule
        // (a same-owner home host's) deciding who else joins.
        let admission = if governed_access_record
            .as_ref()
            .is_some_and(|access| access.governed)
        {
            crate::workspace_access::AdmissionMode::Governed
        } else {
            crate::workspace_access::AdmissionMode::Explicit
        };
        let governed_user_roots = governed_access_record.map_or_else(HashSet::new, |access| {
            access.member_user_roots.into_iter().collect()
        });
        let mut membership = membership;
        let kept = store.load_membership()?;
        for device in kept {
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
            evicted: false,
            membership,
            authorization: Arc::new(AsyncMutex::new(())),
            membership_ceiling: None,
            membership_revision: 0,
            disk_generation: None,
            loaded_generation: None,
            admission,
            governed_user_roots,
            subscribers: HashMap::new(),
            attached_hosts: HashSet::new(),
            attach_sessions: HashMap::new(),
            extensions: Extensions::default(),
            state_sessions: HashMap::new(),
            outbound_leases: HashMap::new(),
            checkpoint_served: HashMap::new(),
            held_records: HashMap::new(),
            subscribing: HashMap::new(),
            arriving: crate::peer::Arriving::default(),
            presence_mailboxes: HashMap::new(),
            live_members: HashSet::new(),
            governor_tap: None,
            workspace_authority: None,
            hosted_replica: None,
            standings: None,
            standings_changed: Arc::default(),
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

    /// Claim `author`'s commit `op` for admission. Called under the workspace's
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

    /// Note that `device` is a host this workspace is attached to.
    /// Idempotent; called as a host announces what it carries, and as a
    /// device hands one of its workspaces to a host to serve.
    pub fn note_attached_host(&mut self, device: [u8; 32]) {
        self.attached_hosts.insert(device);
    }

    /// Whether `device` is a host this workspace is attached to.
    pub(crate) fn attached(&self, device: &[u8; 32]) -> bool {
        self.attached_hosts.contains(device)
    }

    /// Register a subscription. Replaces any prior subscription
    /// from the same device.
    pub fn add_subscriber(&mut self, sub: Subscriber) {
        if !self
            .subscribers
            .get(&sub.device_public_key)
            .is_some_and(|current| current.tx.same_channel(&sub.tx))
        {
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

    /// The connection `tx` of `device` was let subscribe here: answer the
    /// device's earlier connections whose subscriptions are still under
    /// way. This one is newer than each of them, so it replaces whichever
    /// of them registers first.
    pub(crate) fn subscription_admitted(
        &mut self,
        device: [u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) -> Vec<mpsc::WeakSender<Envelope>> {
        let pending = self.subscribing.entry(device).or_default();
        // A connection that ended is nobody's predecessor.
        pending.retain(|earlier| {
            earlier
                .upgrade()
                .is_some_and(|earlier| !earlier.is_closed() && !earlier.same_channel(tx))
        });
        let earlier = pending.clone();
        pending.push(tx.downgrade());
        earlier
    }

    /// The subscription of `device`'s connection `tx` completed, registered
    /// or not: it is no longer under way.
    pub(crate) fn subscription_completed(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) {
        if let Some(pending) = self.subscribing.get_mut(device) {
            pending.retain(|other| other.upgrade().is_some_and(|other| !other.same_channel(tx)));
            if pending.is_empty() {
                self.subscribing.remove(device);
            }
        }
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
    /// Decide this workspace's changes against `standings` from now on, and
    /// wake each change held until its author's standing was projected.
    pub(crate) fn project_standings(&mut self, standings: Standings) {
        self.standings = Some(Arc::new(standings));
        self.standings_changed.notify_waiters();
    }

    pub fn admit_device(&mut self, device: [u8; 32]) -> Result<(), crate::store::StoreError> {
        if !self.membership.contains(&device) {
            let mut devices = self.membership.devices.clone();
            devices.push(device);
            self.store.save_membership(&devices)?;
            self.membership.set(devices);
        }
        Ok(())
    }

    /// Stop listing `device` in this workspace ([`Self::admit_device`]'s
    /// other half): a device the host started for the workspace and stopped.
    pub fn forget_device(&mut self, device: &[u8; 32]) -> Result<(), crate::store::StoreError> {
        if self.membership.contains(device) {
            let mut devices = self.membership.devices.clone();
            devices.retain(|listed| listed != device);
            self.store.save_membership(&devices)?;
            self.membership.set(devices);
        }
        Ok(())
    }

    /// Add subscriber `device` to the live plane and tell the other live
    /// members. Returns the members already present. A device that is not
    /// subscribed cannot join.
    pub fn live_join(
        &mut self,
        device: [u8; 32],
        workspace_id: WorkspaceId,
    ) -> Option<Vec<[u8; 32]>> {
        if !self.subscribers.contains_key(&device) {
            return None;
        }
        let existing: Vec<[u8; 32]> = self
            .live_members
            .iter()
            .copied()
            .filter(|member| *member != device)
            .collect();
        if self.live_members.insert(device) {
            self.announce_live_member(device, workspace_id, true);
        }
        Some(existing)
    }

    /// Remove `device` from the live plane and tell the remaining members.
    pub fn live_leave(&mut self, device: &[u8; 32], workspace_id: WorkspaceId) {
        if self.live_members.remove(device) {
            self.announce_live_member(*device, workspace_id, false);
        }
    }

    /// Tell every other live member that `device` is on the plane (`active`)
    /// or left it. Ephemeral: a member notice lost to a full queue costs
    /// only a stale peer list, never a change.
    fn announce_live_member(&self, device: [u8; 32], workspace_id: WorkspaceId, active: bool) {
        let envelope = Envelope::RetiredLiveMember {
            workspace_id,
            device_public_key: device,
            active,
        };
        for member in &self.live_members {
            if *member == device {
                continue;
            }
            if let Some(sub) = self.subscribers.get(member) {
                let _ = sub.tx.try_send(envelope.clone());
            }
        }
    }

    /// Drop a subscription for `device`, and its live membership. Idempotent.
    pub fn remove_subscriber(&mut self, device: &[u8; 32]) {
        if self.subscribers.remove(device).is_some() {
            // A connection whose subscription went is ended by its next
            // access audit.
            crate::authority_epoch::bump();
        }
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
        self.refuse_live_members(removed, "workspace membership changed");
    }

    /// Tell every live device of the workspace it is no longer authorised,
    /// and why, then unregister them: the workspace was deleted.
    pub(crate) fn refuse_every_live_member(&mut self, reason: &str) {
        let live: Vec<[u8; 32]> = self.subscribers.keys().copied().collect();
        self.refuse_live_members(&live, reason);
    }

    fn refuse_live_members(&mut self, removed: &[[u8; 32]], reason: &str) {
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
                reason: reason.into(),
            });
        }
        for device in removed {
            if self.subscribers.contains_key(device) {
                self.remove_subscriber(device);
            }
        }
    }

    /// Route a presence envelope (no persistence, drop-on-empty). What an
    /// attached host sent is not carried on to another. A subscriber whose
    /// connection has a presence mailbox gets it there; the payload is shared
    /// by every such subscriber, not copied.
    pub fn fanout_presence(&mut self, author: &[u8; 32], envelope: &Envelope) -> usize {
        let mut delivered = 0usize;
        let mut dead: Vec<[u8; 32]> = Vec::new();
        let mut shared: Option<Arc<[u8]>> = None;
        let from_attached = self.attached(author);
        let now = tokio::time::Instant::now();
        for (peer, sub) in &self.subscribers {
            if peer == author {
                continue;
            }
            if from_attached && self.attached(peer) {
                continue;
            }
            if let (
                Some(mailbox),
                Envelope::RetiredPresence {
                    workspace_id,
                    payload,
                },
            ) = (self.presence_mailboxes.get(peer), envelope)
            {
                let payload = shared
                    .get_or_insert_with(|| Arc::from(payload.as_slice()))
                    .clone();
                mailbox.post(*workspace_id, payload, now);
                delivered = delivered.saturating_add(1);
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
}

/// Shorthand: a workspace runtime held under an async mutex.
pub type SharedWorkspace = Arc<AsyncMutex<WorkspaceRuntime>>;

/// Longest a change waits for its author's standing to be projected: the
/// time a device's admission takes to settle in the workspace's governance
/// when the workspace is busy, not the time any check may take.
pub(crate) const STANDING_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The standings to decide `author`'s change in `ws` against, and the
/// workspace's payload adapter.
///
/// A device can hold a record's keys, and write, before the host has settled
/// its own admission of that device: the keys come from another participant,
/// and a page counts itself in once its person is a member, while its device's
/// row is still on its way into the workspace's governance. Decided against
/// the projection of that moment, such a change would be refused for a grant
/// the host is about to record. So a change the projection of the moment
/// `refuses`, while the workspace projects standings and none is `author`'s,
/// waits for the next projection, up to [`STANDING_WAIT`], and is decided
/// against that one. Every other change, the author's presence and
/// governance among them, is decided at once. Nothing is decided without a
/// standing: an author that has none when the wait ends is decided against
/// the projection as it is, and refused as before.
pub(crate) async fn standings_deciding(
    ws: &SharedWorkspace,
    author: [u8; 32],
    refuses: impl Fn(Option<&Standings>, &dyn crate::payloads::ShellPayloads) -> bool,
) -> (
    Option<Arc<Standings>>,
    Arc<dyn crate::payloads::ShellPayloads>,
) {
    let deadline = tokio::time::Instant::now()
        .checked_add(STANDING_WAIT)
        .unwrap_or_else(tokio::time::Instant::now);
    loop {
        let changed = Arc::clone(&ws.lock().await.standings_changed);
        let next = changed.notified();
        tokio::pin!(next);
        // Registered before the standings are read, so a projection that
        // lands between the read and the wait still wakes it.
        next.as_mut().enable();
        let (standings, payloads) = {
            let runtime = ws.lock().await;
            (runtime.standings.clone(), runtime.store.payloads().clone())
        };
        let waiting = standings
            .as_ref()
            .is_some_and(|standings| !standings.contains_key(&author))
            && refuses(standings.as_deref(), &*payloads);
        if !waiting || tokio::time::timeout_at(deadline, next).await.is_err() {
            return (standings, payloads);
        }
    }
}

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
        let mut ws = WorkspaceRuntime::open(
            tmp.path(),
            WorkspaceId::from_u128(5),
            false,
            Membership::default(),
        )
        .unwrap();
        let total = MAX_REMEMBERED_COMMITS as u64 * 2 + 10;
        for n in 1..=total {
            ws.remember_commit(record(n)).persist(&ws.store);
        }
        drop(ws);
        let ws = WorkspaceRuntime::open(
            tmp.path(),
            WorkspaceId::from_u128(5),
            false,
            Membership::default(),
        )
        .unwrap();
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
            workspace_id: WorkspaceId::from_u128(7),
            author_device_public_key: author,
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        }
    }

    /// Retain `env` as the relay keeps an admitted change, at host order `seq`.
    fn publish(rt: &WorkspaceRuntime, env: &Envelope, seq: u64) {
        let Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = env
        else {
            panic!("delta")
        };
        let raw = crate::transport::encode(env).unwrap();
        match crate::coedit::retention_metadata(delta) {
            Some((channel, crate::coedit::RetentionKind::Checkpoint)) => {
                rt.store.retain_checkpoint(&channel, &raw).unwrap();
            }
            Some((channel, crate::coedit::RetentionKind::Delta)) => {
                rt.store.retain_delta(&channel, seq, &raw).unwrap();
            }
            None => panic!("{author_device_public_key:?} wrote no retained change"),
        }
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
        let rt = WorkspaceRuntime::open(tmp.path(), WorkspaceId::from_u128(7), false, membership)
            .unwrap();
        let other_author = retained_envelope([2; 32], [3; 32], true, 201);
        let other_channel = retained_envelope([1; 32], [4; 32], true, 202);
        publish(&rt, &other_author, 1);
        publish(&rt, &other_channel, 2);
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
                    workspace_id: WorkspaceId::from_u128(7),
                    author_device_public_key: [1; 32],
                    authored_at_ns: u64::from(round),
                    delta: payload.encode().unwrap(),
                    signature: [0; 64],
                };
                publish(&rt, &envelope, 10 + u64::from(round) * 2 + offset as u64);
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
    }

    #[test]
    fn opaque_retained_history_is_shared_without_claiming_checkpoint_supersedence() {
        let mut sizes = Vec::new();
        for offline_peers in [1, 32] {
            let tmp = tempfile::tempdir().unwrap();
            let membership = Membership {
                devices: (10..10 + offline_peers).map(|key| [key; 32]).collect(),
            };
            let runtime =
                WorkspaceRuntime::open(tmp.path(), WorkspaceId::from_u128(7), false, membership)
                    .unwrap();
            for round in 1..=24u8 {
                publish(
                    &runtime,
                    &retained_envelope([1; 32], [3; 32], true, round),
                    u64::from(round),
                );
            }
            assert_eq!(
                runtime.store.load_retained().unwrap().len(),
                24,
                "unknown checkpoints have no causal supersedence proof"
            );
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

    /// Presence to a subscriber with a mailbox goes there, batched, and the
    /// subscriber without one gets it on its frame queue.
    #[test]
    fn presence_goes_to_the_mailbox_of_the_subscriber_that_has_one() {
        use crate::presence_mailbox::{BATCH_WINDOW, PresenceMailbox};
        use tokio::time::Instant;

        const AUTHOR: [u8; 32] = [1; 32];
        const RECIPIENT: [u8; 32] = [2; 32];
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(
            tmp.path(),
            WorkspaceId::from_u128(9),
            false,
            Membership::default(),
        )
        .unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: RECIPIENT,
            tx,
            disconnect,
        });
        let mailbox = Arc::new(PresenceMailbox::default());
        rt.route_presence(RECIPIENT, Arc::clone(&mailbox));
        let presence = Envelope::RetiredPresence {
            workspace_id: WorkspaceId::from_u128(9),
            payload: b"caret".to_vec(),
        };

        assert_eq!(rt.fanout_presence(&AUTHOR, &presence), 1);
        assert!(
            rx.try_recv().is_err(),
            "the mailbox took it, not the frame queue"
        );
        assert_eq!(mailbox.take(Instant::now() + BATCH_WINDOW, false).len(), 1);

        rt.presence_mailboxes.remove(&RECIPIENT);
        assert_eq!(rt.fanout_presence(&AUTHOR, &presence), 1);
        assert_eq!(rx.try_recv().unwrap(), presence);
    }

    /// A commit's claim is its own: a copy of it waits for the claim and
    /// then finds it recorded, while another commit is claimed at once.
    #[tokio::test]
    async fn a_commit_claim_holds_off_its_copies_only() {
        const AUTHOR: [u8; 32] = [1; 32];
        let tmp = tempfile::tempdir().unwrap();
        let rt = WorkspaceRuntime::open(
            tmp.path(),
            WorkspaceId::from_u128(9),
            false,
            Membership::default(),
        )
        .unwrap();
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

/// What services beside the session stack keep per workspace, one value per
/// type: a relay keeps its compute routing here. The workspace knows nothing
/// of what they hold.
#[derive(Default)]
pub struct Extensions(HashMap<std::any::TypeId, Box<dyn std::any::Any + Send>>);

impl Extensions {
    /// The value of type `T`, when one was made.
    #[must_use]
    pub fn get<T: std::any::Any + Send>(&self) -> Option<&T> {
        self.0
            .get(&std::any::TypeId::of::<T>())
            .and_then(|value| value.downcast_ref())
    }

    /// The value of type `T`, made by `make` the first time.
    pub fn get_or_insert_with<T: std::any::Any + Send>(
        &mut self,
        make: impl FnOnce() -> T,
    ) -> &mut T {
        let value = self
            .0
            .entry(std::any::TypeId::of::<T>())
            .or_insert_with(|| Box::new(make()));
        #[expect(
            clippy::expect_used,
            reason = "the entry under a type's id is only ever made as that type"
        )]
        let value = value
            .downcast_mut()
            .expect("an extension is kept under its own type");
        value
    }
}

/// How a change whose author's standing is not projected yet is decided
/// ([`standings_deciding`]).
#[cfg(test)]
mod standing_wait_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::significant_drop_tightening,
        reason = "in a test these are the assertion, not a hazard; each projection is one step"
    )]

    use super::*;
    fn standing() -> tilo_action_policy::Standing {
        tilo_action_policy::Standing::person(
            tilo_action_policy::WorkspacePolicy::new("workspace", "ada"),
            "ada",
        )
    }

    fn shared(tmp: &tempfile::TempDir) -> SharedWorkspace {
        Arc::new(AsyncMutex::new(
            WorkspaceRuntime::open(
                tmp.path(),
                WorkspaceId::from_u128(9),
                false,
                Membership::default(),
            )
            .unwrap(),
        ))
    }

    const LAPTOP: [u8; 32] = [1; 32];
    const PAGE: [u8; 32] = [2; 32];

    /// A page's change that reaches the host before the host has projected
    /// the page's standing (its keys came from another participant, its
    /// admission is still settling) waits, and is decided against the
    /// projection that admits the page, not refused against the one before.
    #[tokio::test]
    async fn a_change_before_its_authors_standing_waits_and_is_decided_against_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        ws.lock()
            .await
            .project_standings(Standings::from([(LAPTOP, standing())]));

        let deciding = standings_deciding(&ws, PAGE, |_, _| true);
        tokio::pin!(deciding);
        // Polled once: the change arrived, and is held, not decided.
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, &mut deciding)
                .await
                .is_err(),
            "a change whose author has no standing yet is not decided"
        );
        // A projection that still leaves the page out keeps it waiting.
        ws.lock()
            .await
            .project_standings(Standings::from([(LAPTOP, standing())]));
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, &mut deciding)
                .await
                .is_err()
        );
        // The page's admission settles.
        ws.lock()
            .await
            .project_standings(Standings::from([(LAPTOP, standing()), (PAGE, standing())]));
        let (standings, _) = deciding.await;
        assert!(standings.unwrap().contains_key(&PAGE));
    }

    /// A change whose author already has a standing, or of a workspace that
    /// projects none, is decided at once.
    #[tokio::test]
    async fn a_change_whose_author_has_a_standing_is_decided_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        let unprojected = standings_deciding(&ws, PAGE, |_, _| true);
        tokio::pin!(unprojected);
        let (standings, _) = tokio::time::timeout(std::time::Duration::ZERO, &mut unprojected)
            .await
            .expect("no projection: decided at once");
        assert!(standings.is_none());

        ws.lock()
            .await
            .project_standings(Standings::from([(PAGE, standing())]));
        let projected = standings_deciding(&ws, PAGE, |_, _| true);
        tokio::pin!(projected);
        let (standings, _) = tokio::time::timeout(std::time::Duration::ZERO, &mut projected)
            .await
            .expect("a standing already projected: decided at once");
        assert!(standings.unwrap().contains_key(&PAGE));
        // A change the projection does not refuse, the author's presence or
        // governance among them, is decided at once, standing or not.
        let unrefused = standings_deciding(&ws, LAPTOP, |_, _| false);
        tokio::pin!(unrefused);
        tokio::time::timeout(std::time::Duration::ZERO, &mut unrefused)
            .await
            .expect("a change nothing refuses: decided at once");
    }

    /// An author whose standing never arrives is decided, after the wait,
    /// against the projection that leaves it out: refused as before, never
    /// let through for having waited.
    #[tokio::test(start_paused = true)]
    async fn an_author_never_granted_is_decided_without_a_standing_after_the_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        ws.lock()
            .await
            .project_standings(Standings::from([(LAPTOP, standing())]));
        let started = tokio::time::Instant::now();
        let (standings, _) = standings_deciding(&ws, PAGE, |_, _| true).await;
        assert!(started.elapsed() >= STANDING_WAIT);
        let standings = standings.unwrap();
        assert!(standings.contains_key(&LAPTOP));
        assert!(!standings.contains_key(&PAGE));
    }
}
