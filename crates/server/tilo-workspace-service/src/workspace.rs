//! Per-workspace routing and authorization metadata. Live state storage and
//! the pinned document transfer lazily to the bounded ordered persistence
//! owner. Synchronous persistence methods remain compatibility helpers for
//! runtimes that have not entered shared live service.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use automerge::AutoCommit;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::store::{CommitEntry, WorkspaceStore};
use crate::transport::Envelope;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

/// Membership set for a workspace on this server, as the workspace's governance
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
    /// Peer device pubkey. A device may hold several subscriptions at once,
    /// one per connection, told apart by their channel (`tx`).
    pub device_public_key: [u8; 32],
    /// Bounded outbound envelope channel. Closed or full means the peer can
    /// no longer keep up and is pruned immediately.
    pub tx: mpsc::Sender<Envelope>,
    /// Signals the owning peer task when this subscriber cannot keep up. A
    /// reconnect replays its durable queue instead of growing RAM without
    /// bound or leaving the connection silently stale.
    pub disconnect: mpsc::Sender<()>,
    /// Pieces of record content this connection's queue could not take yet,
    /// one queue per sender, served in turn so one sender's burst cannot
    /// crowd another's out ([`crate::content_queue`]).
    pub content_waiting: Arc<crate::content_queue::ContentQueue>,
}

/// One connection's subscription to the workspace and what the host keeps
/// for that connection alone. A device may hold several at once (a second
/// tab, an agent's session beside its person's page, a page that re-dialled while
/// its old connection is still closing): each is its own, and none displaces
/// another, so one connection entering never ends a sibling's writes.
pub(crate) struct Session {
    pub(crate) subscriber: Subscriber,
    /// The connection's state stream, registered beside its subscription.
    pub(crate) state: Option<crate::peer::StateSession>,
    /// Where presence for this connection goes when it takes it batched
    /// ([`crate::presence_mailbox`]); without one, presence goes on its frame
    /// queue.
    presence: Option<Arc<crate::presence_mailbox::PresenceMailbox>>,
    /// The retained-checkpoint channels a live checkpoint has already been
    /// fanned to since the connection last asked for a baseline. A connection
    /// asks on subscribe and whenever it sends a presence frame (the browser
    /// sends presence only for its hello, a `SyncReq`, and a rejected
    /// checkpoint), which resets its served set. The native publishes its
    /// checkpoint as one set of messages, one per channel (the workspace plus
    /// each resource), so the check is per channel: a full bootstrap set passes
    /// once, and the next idle set, on the same channels, is withheld from a
    /// connection that already holds every delta ([`crate::peer`]'s live
    /// fan-out).
    checkpoint_served: HashSet<[u8; 32]>,
    /// The on-demand resources this connection holds, by channel: those it
    /// opened, and the chain parts it was replayed or followed live while
    /// they were their chain's head. Live changes to an on-demand record
    /// reach only these ([`crate::store::document_sync`]).
    held_documents: HashSet<[u8; 32]>,
}

impl Session {
    /// Whether this is the connection whose outbound channel is `tx`.
    pub(crate) fn is(&self, tx: &mpsc::Sender<Envelope>) -> bool {
        self.subscriber.tx.same_channel(tx)
    }

    /// Whether this connection holds the on-demand resource on `channel`.
    pub(crate) fn holds(&self, channel: &[u8; 32]) -> bool {
        self.held_documents.contains(channel)
    }

    /// Whether a live checkpoint for `channel` would be fanned to this
    /// connection now.
    pub(crate) fn needs_checkpoint(&self, channel: &[u8; 32]) -> bool {
        !self.checkpoint_served.contains(channel)
    }

    fn end(&self) {
        if let Some(state) = &self.state {
            state.live.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// The access of each member device of a workspace, by device key.
pub type DeviceAccess = std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>;

/// A write the server kept, for the workspace's governor to take into its copy.
pub(crate) struct GovernorNotice {
    pub(crate) author: [u8; 32],
    pub(crate) body: Vec<u8>,
    /// Answered once the governor's copy took the write in, or refused it.
    pub(crate) taken: tokio::sync::oneshot::Sender<()>,
}

impl GovernorNotice {
    /// `author`'s write `body`, and what answers once the governor's copy took it in.
    pub(crate) fn new(
        author: [u8; 32],
        body: Vec<u8>,
    ) -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let (taken, answer) = tokio::sync::oneshot::channel();
        (
            Self {
                author,
                body,
                taken,
            },
            answer,
        )
    }
}

/// The queue the workspace's governor is told through, `capacity` notices deep.
pub(crate) fn governor_tap(
    capacity: usize,
) -> (mpsc::Sender<GovernorNotice>, mpsc::Receiver<GovernorNotice>) {
    mpsc::channel(capacity)
}

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// The in-process workspace authority governing this workspace, told of each
    /// retained body a device's admitted delta carries. Bounded; a full queue
    /// holds the sender back.
    pub(crate) governor_tap: Option<mpsc::Sender<GovernorNotice>>,
    /// Taken once the last commit admitted here was told to the governor: the next one is
    /// told after it, so the governor takes each author's writes in the order they were
    /// admitted.
    pub(crate) governor_turn: Option<tokio::sync::oneshot::Receiver<()>>,
    /// The in-process authority serving this workspace, if any: it answers
    /// the workspace's calls.
    pub workspace_authority: Option<crate::workspaces::WorkspaceAuthority>,
    /// Whether this server's own device governs the workspace: it made it
    /// here. A workspace another server hands over is governed where it was
    /// made, and this server carries its calls.
    pub(crate) governs_here: bool,
    /// The access of each member device, as the workspace's governor projects
    /// it: what the server decides each change's claimed action against.
    /// `None` until a governor projects one; the server then decides changes
    /// against their app surface alone.
    pub(crate) device_access: Option<Arc<DeviceAccess>>,
    /// The governance [`Self::device_access`] was read from, to ask whether
    /// the workspace's governance is past it.
    pub(crate) governance_heads: tilo_peer_state::GovernanceHeads,
    /// How many changes of each writer wait now for governance past
    /// [`Self::governance_heads`] ([`access_deciding`]).
    governance_waits: Arc<std::sync::Mutex<HashMap<[u8; 32], usize>>>,
    /// The heads a test holds the workspace's governance at, in place of a
    /// governing server's.
    #[cfg(test)]
    pub(crate) test_governance: Option<Vec<[u8; 32]>>,
    /// Woken each time [`Self::device_access`] is replaced, for a change held
    /// until its author's access is projected ([`access_deciding`]).
    pub(crate) access_changed: Arc<tokio::sync::Notify>,
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
    /// check subscribes AND to decide who gets store-and-forward
    /// on delta receive.
    pub membership: Membership,
    /// Serializes filesystem authority operations independently of state IO.
    pub(crate) authorization: Arc<AsyncMutex<()>>,
    /// Failed/pending replacements cap what an old external file may grant.
    pub(crate) membership_ceiling: Option<HashSet<[u8; 32]>>,
    pub(crate) membership_revision: u64,
    /// The devices this workspace keeps as keys its person revoked: refused with how to approve
    /// another wherever they ask, until their person's approval admits a new key.
    pub(crate) revoked_keys: HashSet<[u8; 32]>,
    /// The daemon's watch on this workspace's membership files, attached on the
    /// first fresh read once a watch covers the state root.
    pub(crate) disk_generation: Option<crate::membership_watch::DiskGeneration>,
    /// The watched generation `membership` was last loaded at. `None` until a
    /// live watch vouched for a load, so every read goes to the disk.
    pub(crate) loaded_generation: Option<u64>,
    /// How the workspace admits devices beyond its exact membership:
    /// explicitly, or under native governance. Read only through
    /// [`crate::workspace_access::decide`].
    pub(crate) admission: crate::workspace_access::AdmissionMode,
    /// The people this native-governed workspace has as members, by their
    /// roots, whatever devices of theirs it lists now: the one answer to
    /// whether a person is a member here, which the server-wide handshake and
    /// the certification queue read. Exact workspace admission still uses
    /// `membership`, so an ungoverned sibling device under the same root
    /// does not join.
    pub governed_user_roots: HashSet<[u8; 32]>,
    /// The live subscribed connections of each device, oldest first
    /// ([`Session`]).
    sessions: HashMap<[u8; 32], Vec<Session>>,
    pub outbound_leases: HashMap<[u8; 32], Arc<std::sync::atomic::AtomicBool>>,
    /// Device pubkeys of the hosts this workspace is attached to: the hosts
    /// of its person that serve it, recorded as each announces what it
    /// carries (its hello). A change one of them sent is not carried on to
    /// another, and it carries its members' changes under the members' own
    /// keys.
    pub attached_hosts: HashSet<[u8; 32]>,
    /// What services beside the session stack keep for this workspace (a
    /// server's compute routing), one value per type.
    pub extensions: Extensions,
    /// The host's content reads one member may hold at once, so one
    /// member's pieces cannot take the blocking pool from another's
    /// ([`crate::content_work`]).
    pub(crate) content_work: Arc<crate::content_work::ContentWork>,
    /// Devices admitted through the workspace's entry that have not
    /// subscribed yet, with what was committed for them alone meanwhile
    /// (their channel Welcome), which their replay writes first.
    pub(crate) arriving: crate::peer::Arriving,
    /// The devices on this workspace's live session ([`Self::live_join`]):
    /// each is told of the others as they join and leave, so a device knows
    /// whom its host reaches for content and presence.
    live_members: HashSet<[u8; 32]>,
    /// Host order of the last admitted delta.
    last_seq: u64,
    /// Recently admitted commits, for de-duplicating resends.
    commits: CommitLog,
    /// Commits being admitted, by author and operation id, each held from
    /// its log check until it is admitted and recorded
    /// ([`Self::claim_commit`]): two copies of one commit cannot both be
    /// admitted, while different commits are admitted side by side. One
    /// check for the whole workspace held every commit through the previous one's
    /// durability and publication, so a busy workspace committed one change at a
    /// time.
    commit_claims: CommitClaims,
    /// Held from a commit's [`Self::remember_commit`] until its log write
    /// is done, so log writes reach the file in the order they were
    /// remembered.
    pub(crate) commit_log_queue: Arc<AsyncMutex<()>>,
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

/// The log write that makes one [`WorkspaceRuntime::remember_commit`]
/// durable.
#[must_use]
pub enum CommitLogWrite {
    Append(CommitEntry),
    /// Replace the log, dropping evicted records.
    Rewrite(Vec<CommitEntry>),
}

impl CommitLogWrite {
    /// Write it to `store`. A failure is logged, not fatal: the commit itself
    /// is already durable, and a resend it would have recognised is merged
    /// by the document again.
    pub fn persist(&self, store: &WorkspaceStore) {
        let persisted = match self {
            Self::Append(record) => store.append_commit(record),
            Self::Rewrite(records) => store.rewrite_commits(records),
        };
        if let Err(error) = persisted {
            tracing::warn!(%error, "commit log not persisted");
        }
    }
}

#[derive(Default)]
struct CommitLog {
    by_op: HashMap<([u8; 32], [u8; 16]), CommitEntry>,
    order: VecDeque<([u8; 32], [u8; 16])>,
    /// Records in the on-disk log, evicted ones included.
    logged: usize,
}

impl CommitLog {
    fn load(records: Vec<CommitEntry>) -> Self {
        let mut log = Self {
            logged: records.len(),
            ..Self::default()
        };
        for record in records {
            log.insert(record);
        }
        log
    }

    fn insert(&mut self, record: CommitEntry) {
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

/// What a workspace tells a key its person revoked: the one command that approves another, in the
/// words every surface uses.
pub(crate) fn key_revoked_reason() -> String {
    format!(
        "{}: this key was revoked. {}",
        tilo_actions::remote::KEY_REVOKED,
        tilo_actions::remote::APPROVE_AGAIN
    )
}

/// Whether `bytes` can be an Automerge chunk. Automerge logs every chunk it
/// cannot parse before returning the error, and the deltas a server routes
/// are mostly co-edit wrappers and sealed channel payloads, which never start
/// with its magic: only bytes that do are worth handing to its parser.
pub(crate) fn is_automerge_chunk(bytes: &[u8]) -> bool {
    bytes.starts_with(&AUTOMERGE_MAGIC)
}

impl WorkspaceRuntime {
    /// The next commit's turn to tell the governor: what answers once the commit admitted
    /// before it was told, and what it drops once it was told.
    pub(crate) fn next_governor_turn(
        &mut self,
    ) -> (
        Option<tokio::sync::oneshot::Receiver<()>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (told, turn) = tokio::sync::oneshot::channel();
        (self.governor_turn.replace(turn), told)
    }

    /// Whether the person whose root is `user` is admitted to this workspace as
    /// a member its governance names. What [`crate::workspace_access`] admits
    /// their devices by.
    #[must_use]
    pub fn admits_person(&self, user: &[u8; 32]) -> bool {
        self.governed_user_roots.contains(user)
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
        // only lists the devices this host admitted leaves explicit
        // membership deciding who else joins.
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
        let commits = CommitLog::load(store.load_commits()?);
        Ok(Self {
            store,
            doc,
            persistence: None,
            evicted: false,
            membership,
            authorization: Arc::new(AsyncMutex::new(())),
            membership_ceiling: None,
            membership_revision: 0,
            revoked_keys: HashSet::new(),
            disk_generation: None,
            loaded_generation: None,
            admission,
            governed_user_roots,
            sessions: HashMap::new(),
            attached_hosts: HashSet::new(),
            extensions: Extensions::default(),
            content_work: Arc::default(),
            outbound_leases: HashMap::new(),
            arriving: crate::peer::Arriving::default(),
            live_members: HashSet::new(),
            governor_tap: None,
            governor_turn: None,
            workspace_authority: None,
            governs_here: false,
            device_access: None,
            governance_heads: tilo_peer_state::GovernanceHeads::default(),
            governance_waits: Arc::default(),
            #[cfg(test)]
            test_governance: None,
            access_changed: Arc::default(),
            last_seq: 0,
            commits,
            commit_claims: Arc::default(),
            commit_log_queue: Arc::new(AsyncMutex::new(())),
        })
    }

    /// The admission of `author`'s commit `op`, if this host remembers it.
    pub fn admitted_commit(&self, author: &[u8; 32], op: &[u8; 16]) -> Option<CommitEntry> {
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
    /// workspace lock and in remembering order (see `commit_log_queue`).
    pub fn remember_commit(&mut self, record: CommitEntry) -> CommitLogWrite {
        self.commits.insert(record);
        self.commits.logged = self.commits.logged.saturating_add(1);
        if self.commits.logged > MAX_REMEMBERED_COMMITS.saturating_mul(2) {
            let kept: Vec<CommitEntry> = self
                .commits
                .order
                .iter()
                .filter_map(|key| self.commits.by_op.get(key).copied())
                .collect();
            self.commits.logged = kept.len();
            CommitLogWrite::Rewrite(kept)
        } else {
            CommitLogWrite::Append(record)
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

    /// Register the subscription of `sub`'s connection. Another connection
    /// of the same device keeps its own; registering the same connection
    /// again starts its checkpoints and held resources afresh.
    pub fn add_subscriber(&mut self, sub: Subscriber) {
        // Its replay holds every chain's head.
        let held_documents: HashSet<[u8; 32]> = self.store.chain_heads().into_iter().collect();
        let sessions = self.sessions.entry(sub.device_public_key).or_default();
        let same = sessions.iter().position(|session| session.is(&sub.tx));
        if let Some(session) = same.and_then(|at| sessions.get_mut(at)) {
            session.checkpoint_served.clear();
            session.held_documents = held_documents;
            session.subscriber = sub;
            return;
        }
        sessions.push(Session {
            subscriber: sub,
            state: None,
            presence: None,
            checkpoint_served: HashSet::new(),
            held_documents,
        });
    }

    /// Register the state stream of `device`'s subscribed connection `tx`
    /// beside its subscription.
    pub(crate) fn set_state_session(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
        state: crate::peer::StateSession,
    ) {
        if let Some(session) = self.session_mut(device, tx) {
            session.state = Some(state);
        }
    }

    /// The subscription of `device`'s connection `tx`, while it holds one.
    pub(crate) fn session(
        &self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) -> Option<&Session> {
        self.sessions_of(device)
            .iter()
            .find(|session| session.is(tx))
    }

    fn session_mut(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) -> Option<&mut Session> {
        self.sessions
            .get_mut(device)?
            .iter_mut()
            .find(|session| session.is(tx))
    }

    /// The subscribed connections of `device`, oldest first.
    pub(crate) fn sessions_of(&self, device: &[u8; 32]) -> &[Session] {
        self.sessions.get(device).map_or(&[], Vec::as_slice)
    }

    /// Every subscribed connection, with its device.
    pub(crate) fn all_sessions(&self) -> impl Iterator<Item = (&[u8; 32], &Session)> {
        self.sessions
            .iter()
            .flat_map(|(device, sessions)| sessions.iter().map(move |session| (device, session)))
    }

    /// The devices holding at least one subscription here.
    pub(crate) fn subscribed_devices(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.sessions.keys()
    }

    /// The subscriber of `device`'s newest connection: where what is
    /// addressed to the device alone goes.
    pub fn subscriber(&self, device: &[u8; 32]) -> Option<&Subscriber> {
        self.sessions_of(device)
            .last()
            .map(|session| &session.subscriber)
    }

    /// The state stream of `device`'s subscribed connection `tx`.
    pub(crate) fn state_of(
        &self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) -> Option<&crate::peer::StateSession> {
        self.session(device, tx)?.state.as_ref()
    }

    /// The state stream of `device`'s newest subscribed connection that has
    /// one.
    pub(crate) fn newest_state(&self, device: &[u8; 32]) -> Option<&crate::peer::StateSession> {
        self.sessions_of(device)
            .iter()
            .rev()
            .find_map(|session| session.state.as_ref())
    }

    /// `device`'s connection `tx` now holds the resource on `channel`: it
    /// opened it, or took a live change to it while it was its chain's head.
    pub(crate) fn hold_document(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
        channel: [u8; 32],
    ) {
        if let Some(session) = self.session_mut(device, tx) {
            session.held_documents.insert(channel);
        }
    }

    pub(crate) fn unhold_document(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
        digest: &[u8; 32],
    ) {
        if let Some(session) = self.session_mut(device, tx) {
            session.held_documents.remove(digest);
        }
    }

    /// Hand the presence fanned out to `device`'s connection `tx` to that
    /// connection's `mailbox` rather than its frame queue. Call right after
    /// [`Self::add_subscriber`] registered that connection; removing the
    /// subscription drops the route.
    pub fn route_presence(
        &mut self,
        device: [u8; 32],
        tx: &mpsc::Sender<Envelope>,
        mailbox: Arc<crate::presence_mailbox::PresenceMailbox>,
    ) {
        if let Some(session) = self.session_mut(&device, tx) {
            session.presence = Some(mailbox);
        }
    }

    /// `device`'s connection `tx` asked for a baseline (its hello, a
    /// `SyncReq`, or a rejected checkpoint arrive as presence): the next live
    /// checkpoint on every channel reaches it once more.
    pub fn request_checkpoint(&mut self, device: &[u8; 32], tx: &mpsc::Sender<Envelope>) {
        if let Some(session) = self.session_mut(device, tx) {
            session.checkpoint_served.clear();
        }
    }

    /// Note that one live checkpoint reached `device`'s connection `tx`.
    /// Call only after the delivery queue accepted the checkpoint.
    pub(crate) fn mark_checkpoint_served(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
        channel: [u8; 32],
    ) {
        if let Some(session) = self.session_mut(device, tx) {
            session.checkpoint_served.insert(channel);
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
    /// Decide this workspace's changes against `device_access` from now on, and
    /// wake each change held until its author's access was projected.
    pub(crate) fn project_access(
        &mut self,
        device_access: DeviceAccess,
        heads: tilo_peer_state::GovernanceHeads,
    ) {
        self.device_access = Some(Arc::new(device_access));
        self.governance_heads = heads;
        self.access_changed.notify_waiters();
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

    /// Add subscriber `device` to the live session and tell the other live
    /// members. Returns the members already present. A device that is not
    /// subscribed cannot join.
    pub fn live_join(
        &mut self,
        device: [u8; 32],
        workspace_id: WorkspaceId,
    ) -> Option<Vec<[u8; 32]>> {
        if !self.has_subscriber(&device) {
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

    /// Remove `device` from the live session and tell the remaining members.
    pub fn live_leave(&mut self, device: &[u8; 32], workspace_id: WorkspaceId) {
        if self.live_members.remove(device) {
            self.announce_live_member(*device, workspace_id, false);
        }
    }

    /// Tell every other live member that `device` is on the live session (`active`)
    /// or left it. Ephemeral: a member notice lost to a full queue costs
    /// only a stale peer list, never a change.
    fn announce_live_member(&self, device: [u8; 32], workspace_id: WorkspaceId, active: bool) {
        let envelope = Envelope::LiveMember {
            workspace_id,
            device_public_key: device,
            active,
        };
        for member in &self.live_members {
            if *member == device {
                continue;
            }
            for session in self.sessions_of(member) {
                let _ = session.subscriber.tx.try_send(envelope.clone());
            }
        }
    }

    /// Drop every subscription of `device`, and its live membership.
    /// Idempotent.
    pub fn remove_subscriber(&mut self, device: &[u8; 32]) {
        if let Some(sessions) = self.sessions.remove(device) {
            sessions.iter().for_each(Session::end);
            // A connection whose subscription went is ended by its next
            // access audit.
            crate::authority_epoch::bump();
        }
        let workspace_id = self.store.workspace_id();
        self.live_leave(device, workspace_id);
    }

    /// Remove only the subscription of `device`'s connection `tx`: the
    /// device's other connections keep theirs, and a closing connection's
    /// delayed teardown touches no other. The device leaves the live session
    /// with its last connection.
    pub(crate) fn remove_subscriber_session(
        &mut self,
        device: &[u8; 32],
        tx: &mpsc::Sender<Envelope>,
    ) {
        let Some(sessions) = self.sessions.get_mut(device) else {
            return;
        };
        let Some(at) = sessions.iter().position(|session| session.is(tx)) else {
            return;
        };
        sessions.remove(at).end();
        crate::authority_epoch::bump();
        if sessions.is_empty() {
            self.remove_subscriber(device);
        }
    }

    /// Whether `device` currently holds a subscription here.
    pub fn has_subscriber(&self, device: &[u8; 32]) -> bool {
        !self.sessions_of(device).is_empty()
    }

    /// Whether `device`'s connection `tx` holds a subscription here: the
    /// connection whose writes and requests the workspace takes.
    pub(crate) fn current_session(&self, device: &[u8; 32], tx: &mpsc::Sender<Envelope>) -> bool {
        self.session(device, tx).is_some()
    }

    /// Whether this workspace keeps `device` as a key its person revoked: nothing it asks is
    /// answered as a member's until they approve another.
    #[must_use]
    pub(crate) fn revoked_key(&self, device: &[u8; 32]) -> bool {
        self.revoked_keys.contains(device)
    }

    /// Tell live devices that just lost membership they are no longer
    /// authorised, while their connection is still open. Removals publish
    /// this before durable IO so a concurrent audit cannot close the session
    /// without an authenticated refusal. The subscriber is then unregistered
    /// so a follow-up write closes instead of emitting a second refusal.
    pub(crate) fn refuse_removed_live_members(&mut self, removed: &[[u8; 32]]) {
        let (revoked, others): (Vec<[u8; 32]>, Vec<[u8; 32]>) = removed
            .iter()
            .copied()
            .partition(|device| self.revoked_key(device));
        if !revoked.is_empty() {
            self.refuse_live_members(
                &revoked,
                &tilo_server_protocol::refusal::Refusal::NotAMember(key_revoked_reason()),
            );
        }
        if !others.is_empty() {
            self.refuse_live_members(
                &others,
                &tilo_server_protocol::refusal::Refusal::NotAMember(
                    "workspace membership changed".into(),
                ),
            );
        }
    }

    /// Tell every live device of the workspace it is no longer authorised,
    /// and why, then unregister them: the workspace was deleted.
    pub(crate) fn refuse_every_live_member(
        &mut self,
        refusal: &tilo_server_protocol::refusal::Refusal,
    ) {
        let live: Vec<[u8; 32]> = self.subscribed_devices().copied().collect();
        self.refuse_live_members(&live, refusal);
    }

    fn refuse_live_members(
        &mut self,
        removed: &[[u8; 32]],
        refusal: &tilo_server_protocol::refusal::Refusal,
    ) {
        if removed.is_empty() {
            return;
        }
        let workspace_id = self.store.workspace_id();
        for device in removed {
            for session in self.sessions_of(device) {
                let _ = session.subscriber.tx.try_send(Envelope::NotAuthorised {
                    workspace_id,
                    refusal: refusal.clone(),
                });
            }
        }
        for device in removed {
            if self.has_subscriber(device) {
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
        let mut dead: Vec<([u8; 32], mpsc::Sender<Envelope>)> = Vec::new();
        let mut shared: Option<Arc<[u8]>> = None;
        let from_attached = self.attached(author);
        let now = tokio::time::Instant::now();
        for (peer, session) in self.all_sessions() {
            let sub = &session.subscriber;
            if peer == author {
                continue;
            }
            if from_attached && self.attached(peer) {
                continue;
            }
            if let (
                Some(mailbox),
                Envelope::Presence {
                    workspace_id,
                    payload,
                },
            ) = (session.presence.as_ref(), envelope)
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
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    dead.push((*peer, sub.tx.clone()));
                }
            }
        }
        for (device, tx) in dead {
            self.remove_subscriber_session(&device, &tx);
        }
        delivered
    }

    /// Deliver a directed envelope to the live subscriber `target` only, on
    /// its newest connection: a request is carried out once, not once per connection.
    /// Never queued: a directed request is live traffic. Returns whether it
    /// was handed to that connection.
    pub fn route_directed(&mut self, target: &[u8; 32], envelope: Envelope) -> bool {
        let Some(sub) = self.subscriber(target) else {
            return false;
        };
        if sub.tx.try_send(envelope).is_ok() {
            return true;
        }
        let _ = sub.disconnect.try_send(());
        let tx = sub.tx.clone();
        self.remove_subscriber_session(target, &tx);
        false
    }

    /// Deliver a piece of record content `from` sent to the subscriber
    /// `target` only, through its fairness queue ([`crate::content_queue`]):
    /// a piece the connection's queue cannot take waits with its sender's
    /// own pieces, never ahead of another sender's. Returns whether `target`
    /// is subscribed. Each of `target`'s connections gets it: whichever of
    /// them asked for the resource takes it.
    pub fn route_content(&self, from: &[u8; 32], target: &[u8; 32], envelope: &Envelope) -> bool {
        let sessions = self.sessions_of(target);
        for session in sessions {
            let sub = &session.subscriber;
            sub.content_waiting
                .deliver_piece(*from, envelope.clone(), &sub.tx);
        }
        !sessions.is_empty()
    }
}

#[cfg(test)]
impl WorkspaceRuntime {
    /// The state stream of `device`'s newest subscribed connection, to change.
    pub(crate) fn newest_state_mut(
        &mut self,
        device: &[u8; 32],
    ) -> Option<&mut crate::peer::StateSession> {
        self.sessions
            .get_mut(device)?
            .iter_mut()
            .rev()
            .find_map(|session| session.state.as_mut())
    }

    /// Give `device`'s newest subscribed connection the state stream `state`.
    pub(crate) fn put_state(&mut self, device: &[u8; 32], state: crate::peer::StateSession) {
        if let Some(session) = self.sessions.get_mut(device).and_then(|all| all.last_mut()) {
            session.state = Some(state);
        }
    }

    /// Drop every connection's state stream, keeping the subscriptions.
    pub(crate) fn clear_states(&mut self) {
        for session in self.sessions.values_mut().flatten() {
            session.state = None;
        }
    }

    /// Whether a live checkpoint for `channel` would reach `device`'s newest
    /// connection now.
    pub(crate) fn needs_checkpoint(&self, device: &[u8; 32], channel: &[u8; 32]) -> bool {
        self.sessions_of(device)
            .last()
            .is_some_and(|session| session.needs_checkpoint(channel))
    }
}

/// Shorthand: a workspace runtime held under an async mutex.
pub type SharedWorkspace = Arc<AsyncMutex<WorkspaceRuntime>>;

/// Longest a change waits for its author's access to be projected: the
/// time a device's admission takes to settle in the workspace's governance
/// when the workspace is busy, not the time any check may take.
pub(crate) const ACCESS_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Most changes of one writer that wait at once for governance past what
/// the workspace's access was read from: a writer whose changes the access
/// refuses holds no more of the server than this while it waits.
pub(crate) const GOVERNANCE_WAITS_PER_WRITER: usize = 16;

/// What [`access_deciding`] decides a change against.
pub(crate) struct AccessDecision {
    pub(crate) device_access: Option<Arc<DeviceAccess>>,
    pub(crate) payloads: Arc<dyn crate::payloads::ShellPayloads>,
    /// The change would be refused, and the access was still read from
    /// governance behind the workspace's when the wait ended: refused for that
    /// ([`collab_types::GOVERNANCE_NEVER_ARRIVED`]), not for the access it was
    /// decided against.
    pub(crate) governance_behind: bool,
}

/// One of a writer's changes waiting for governance, counted while it waits.
struct GovernanceWait {
    waits: Arc<std::sync::Mutex<HashMap<[u8; 32], usize>>>,
    author: [u8; 32],
}

impl GovernanceWait {
    /// Count a wait of `author`'s, unless as many wait already as one
    /// writer may have.
    fn begin(
        waits: &Arc<std::sync::Mutex<HashMap<[u8; 32], usize>>>,
        author: [u8; 32],
    ) -> Option<Self> {
        let mut counts = waits.lock_recover();
        let count = counts.entry(author).or_default();
        if *count >= GOVERNANCE_WAITS_PER_WRITER {
            return None;
        }
        *count = count.saturating_add(1);
        drop(counts);
        Some(Self {
            waits: Arc::clone(waits),
            author,
        })
    }
}

impl Drop for GovernanceWait {
    fn drop(&mut self) {
        let mut counts = self.waits.lock_recover();
        if let Some(count) = counts.get_mut(&self.author) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.author);
            }
        }
    }
}

/// Where the workspace's governance as this server holds it now is read.
struct Governing {
    authority: Option<crate::workspaces::WorkspaceAuthority>,
    #[cfg(test)]
    pinned: Option<Vec<[u8; 32]>>,
}

impl Governing {
    fn of(runtime: &WorkspaceRuntime) -> Self {
        Self {
            authority: runtime.workspace_authority.clone(),
            #[cfg(test)]
            pinned: runtime.test_governance.clone(),
        }
    }

    /// The heads of the governance this server holds for the workspace, now;
    /// none for a workspace it does not govern.
    async fn heads(&self) -> Vec<[u8; 32]> {
        #[cfg(test)]
        if let Some(pinned) = &self.pinned {
            return pinned.clone();
        }
        let Some(authority) = &self.authority else {
            return Vec::new();
        };
        let Some(workspaces) = authority.workspaces() else {
            return Vec::new();
        };
        workspaces
            .governance_heads(authority.workspace())
            .await
            .unwrap_or_default()
    }
}

/// What the workspace's devices may do, to decide `author`'s change in `ws`
/// against, which names the governance heads `claimed`
/// ([`crate::surfaces::governance_named`]), and the workspace's payload
/// adapter.
///
/// A device can hold a record's keys, and write, before the host has settled
/// its own admission of that device: the keys come from another key holder,
/// and a page counts itself in once its person is a member, while its device's
/// row is still on its way into the workspace's governance. A device can also
/// write under a role given to it a moment ago, which the server has written
/// into its governance but not yet read its access from. Decided against
/// the projection of that moment, such a change would be refused for a grant
/// the host is about to record. So a change the projection of the moment
/// `refuses` waits for the next projection, up to [`ACCESS_WAIT`], while the
/// workspace projects nothing for `author`'s device, while this server's own
/// governance is past the state the projection was read from, or while the
/// change names governance heads the projection was not read from
/// ([`collab_types::SignedOp::governance`]); then it is
/// decided against the projection as it is then, with every revocation in it.
/// The governance says when to decide, never what against. Every other
/// change is decided at once.
/// Nothing is decided without access: an author that has none when the wait
/// ends is decided against the projection as it is, and refused as before.
pub(crate) async fn access_deciding(
    ws: &SharedWorkspace,
    author: [u8; 32],
    claimed: &[[u8; 4]],
    refuses: impl Fn(Option<&DeviceAccess>, &dyn crate::payloads::ShellPayloads) -> bool,
) -> AccessDecision {
    let deadline = tokio::time::Instant::now()
        .checked_add(ACCESS_WAIT)
        .unwrap_or_else(tokio::time::Instant::now);
    let mut counted: Option<Option<GovernanceWait>> = None;
    loop {
        let changed = Arc::clone(&ws.lock().await.access_changed);
        let next = changed.notified();
        tokio::pin!(next);
        // Registered before the device_access are read, so a projection that
        // lands between the read and the wait still wakes it.
        next.as_mut().enable();
        let (device_access, payloads, heads, waits, authority) = {
            let runtime = ws.lock().await;
            (
                runtime.device_access.clone(),
                runtime.store.payloads().clone(),
                runtime.governance_heads.clone(),
                Arc::clone(&runtime.governance_waits),
                Governing::of(&runtime),
            )
        };
        let refusing = refuses(device_access.as_deref(), &*payloads);
        let unprojected = device_access
            .as_ref()
            .is_some_and(|device_access| !device_access.contains_key(&author));
        let behind = refusing
            && device_access.is_some()
            && (!heads.holds_named(claimed) || !heads.holds(&Box::pin(authority.heads()).await));
        // A writer with as many changes waiting for governance as it may
        // have is not waited for again.
        let may_wait_behind = behind
            && counted
                .get_or_insert_with(|| GovernanceWait::begin(&waits, author))
                .is_some();
        let waiting = refusing && (unprojected || (behind && may_wait_behind));
        if !waiting || tokio::time::timeout_at(deadline, next).await.is_err() {
            return AccessDecision {
                device_access,
                payloads,
                governance_behind: behind,
            };
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
    fn the_commit_log_is_durable_and_forgets_its_oldest_commits_first() {
        let tmp = tempfile::tempdir().unwrap();
        let record = |n: u64| CommitEntry {
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
            "the on-disk log is compacted"
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

    /// Retain `env` as the server keeps an admitted change, at host order `seq`.
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
            tx: tx.clone(),
            disconnect,
            content_waiting: Arc::default(),
        });
        let mailbox = Arc::new(PresenceMailbox::default());
        rt.route_presence(RECIPIENT, &tx, Arc::clone(&mailbox));
        let presence = Envelope::Presence {
            workspace_id: WorkspaceId::from_u128(9),
            payload: b"caret".to_vec(),
        };

        assert_eq!(rt.fanout_presence(&AUTHOR, &presence), 1);
        assert!(
            rx.try_recv().is_err(),
            "the mailbox took it, not the frame queue"
        );
        assert_eq!(mailbox.take(Instant::now() + BATCH_WINDOW, false).len(), 1);

        rt.session_mut(&RECIPIENT, &tx).unwrap().presence = None;
        assert_eq!(rt.fanout_presence(&AUTHOR, &presence), 1);
        assert_eq!(rx.try_recv().unwrap(), presence);
    }

    /// A burst of content from one sender waits for the connection's queue
    /// without taking the room another sender's piece needs.
    #[test]
    fn a_content_burst_from_one_sender_leaves_room_for_another() {
        const A: [u8; 32] = [1; 32];
        const B: [u8; 32] = [2; 32];
        const TO: [u8; 32] = [3; 32];
        const WS: WorkspaceId = WorkspaceId::from_u128(9);
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = WorkspaceRuntime::open(tmp.path(), WS, false, Membership::default()).unwrap();
        let (tx, mut rx) = mpsc::channel(2);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        rt.add_subscriber(Subscriber {
            device_public_key: TO,
            tx: tx.clone(),
            disconnect,
            content_waiting: Arc::default(),
        });
        let piece = |from: [u8; 32], byte: u8| Envelope::DocumentContent {
            workspace_id: WS,
            to_device_public_key: TO,
            from_device_public_key: from,
            frame: vec![byte],
        };
        for number in 0..20 {
            assert!(rt.route_content(&A, &TO, &piece(A, number)));
        }
        assert!(rt.route_content(&B, &TO, &piece(B, 0xBB)));
        assert!(
            !rt.route_content(&B, &[9; 32], &piece(B, 0xBB)),
            "a piece for a device that is not subscribed goes nowhere"
        );

        let queue = Arc::clone(&rt.subscriber(&TO).unwrap().content_waiting);
        let mut seen = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let Envelope::DocumentContent {
                from_device_public_key,
                ..
            } = envelope
            {
                seen.push(from_device_public_key);
            }
            // The connection's writer takes one, the queue's content returns.
            queue.serve_waiting(&tx);
        }
        assert_eq!(seen.len(), 21, "the burst under the bound arrives whole");
        let b_at = seen.iter().position(|from| *from == B).unwrap();
        assert!(
            b_at <= 4,
            "B's piece came within a turn, after {b_at} of A's, not behind the whole burst"
        );
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
        let claim = rt.claim_commit(AUTHOR, [1; 16]).unwrap();
        let other = rt.claim_commit(AUTHOR, [2; 16]).unwrap();
        let elsewhere = rt.claim_commit([2; 32], [1; 16]).unwrap();
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
        // the log is the one to answer it.
        drop(rt.claim_commit(AUTHOR, [1; 16]).unwrap());
        drop((other, elsewhere));
    }
}

/// What services beside the session stack keep per workspace, one value per
/// type: a server keeps its compute routing here. The workspace knows nothing
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

/// How a change whose author's access is not projected yet is decided
/// ([`access_deciding`]).
#[cfg(test)]
mod access_wait_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::significant_drop_tightening,
        reason = "in a test these are the assertion, not a hazard; each projection is one step"
    )]

    use super::*;
    fn access() -> tilo_action_policy::MemberAccess {
        tilo_action_policy::MemberAccess::person(
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
    /// the page's access (its keys came from another key holder, its
    /// admission is still settling) waits, and is decided against the
    /// projection that admits the page, not refused against the one before.
    #[tokio::test]
    async fn a_change_before_its_authors_access_waits_and_is_decided_against_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        ws.lock().await.project_access(
            DeviceAccess::from([(LAPTOP, access())]),
            tilo_peer_state::GovernanceHeads::default(),
        );

        let deciding = access_deciding(&ws, PAGE, &[], |_, _| true);
        tokio::pin!(deciding);
        // Polled once: the change arrived, and is held, not decided.
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, &mut deciding)
                .await
                .is_err(),
            "a change whose author has no access yet is not decided"
        );
        // A projection that still leaves the page out keeps it waiting.
        ws.lock().await.project_access(
            DeviceAccess::from([(LAPTOP, access())]),
            tilo_peer_state::GovernanceHeads::default(),
        );
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, &mut deciding)
                .await
                .is_err()
        );
        // The page's admission settles.
        ws.lock().await.project_access(
            DeviceAccess::from([(LAPTOP, access()), (PAGE, access())]),
            tilo_peer_state::GovernanceHeads::default(),
        );
        let device_access = deciding.await.device_access;
        assert!(device_access.unwrap().contains_key(&PAGE));
    }

    /// A change whose author already has an access, or of a workspace that
    /// projects none, is decided at once.
    #[tokio::test]
    async fn a_change_whose_author_has_access_is_decided_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        let unprojected = access_deciding(&ws, PAGE, &[], |_, _| true);
        tokio::pin!(unprojected);
        let device_access = tokio::time::timeout(std::time::Duration::ZERO, &mut unprojected)
            .await
            .expect("no projection: decided at once")
            .device_access;
        assert!(device_access.is_none());

        ws.lock().await.project_access(
            DeviceAccess::from([(PAGE, access())]),
            tilo_peer_state::GovernanceHeads::default(),
        );
        let projected = access_deciding(&ws, PAGE, &[], |_, _| true);
        tokio::pin!(projected);
        let device_access = tokio::time::timeout(std::time::Duration::ZERO, &mut projected)
            .await
            .expect("an access already projected: decided at once")
            .device_access;
        assert!(device_access.unwrap().contains_key(&PAGE));
        // A change the projection does not refuse, the author's presence or
        // governance among them, is decided at once, whether the author has member access or not.
        let unrefused = access_deciding(&ws, LAPTOP, &[], |_, _| false);
        tokio::pin!(unrefused);
        tokio::time::timeout(std::time::Duration::ZERO, &mut unrefused)
            .await
            .expect("a change nothing refuses: decided at once");
    }

    /// An author whose access never arrives is decided, after the wait,
    /// against the projection that leaves it out: refused as before, never
    /// let through for having waited.
    #[tokio::test(start_paused = true)]
    async fn an_author_never_granted_is_decided_without_access_after_the_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        ws.lock().await.project_access(
            DeviceAccess::from([(LAPTOP, access())]),
            tilo_peer_state::GovernanceHeads::default(),
        );
        let started = tokio::time::Instant::now();
        let decision = access_deciding(&ws, PAGE, &[], |_, _| true).await;
        assert!(started.elapsed() >= ACCESS_WAIT);
        assert!(!decision.governance_behind);
        let device_access = decision.device_access.unwrap();
        assert!(device_access.contains_key(&LAPTOP));
        assert!(!device_access.contains_key(&PAGE));
    }

    /// A change its author's access refuses while this server's governance is
    /// past what the access was read from (a role given a moment ago) waits
    /// for the access to be read again; when it never is, it is refused for
    /// that after the wait, never let through.
    #[tokio::test(start_paused = true)]
    async fn a_change_refused_while_governance_is_ahead_waits_then_is_refused_for_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        {
            let mut runtime = ws.lock().await;
            runtime.project_access(
                DeviceAccess::from([(PAGE, access())]),
                tilo_peer_state::GovernanceHeads::default(),
            );
            runtime.test_governance = Some(vec![[9; 32]]);
        }
        let deciding = access_deciding(&ws, PAGE, &[], |_, _| true);
        tokio::pin!(deciding);
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, &mut deciding)
                .await
                .is_err(),
            "a change refused while the access is behind is not decided yet"
        );
        let started = tokio::time::Instant::now();
        let decision = deciding.await;
        assert!(
            started.elapsed() >= ACCESS_WAIT.saturating_sub(std::time::Duration::from_millis(1))
        );
        assert!(
            decision.governance_behind,
            "refused for the governance it waited for"
        );
        // One the access lets through is decided at once.
        let allowed = access_deciding(&ws, PAGE, &[], |_, _| false);
        tokio::pin!(allowed);
        let decision = tokio::time::timeout(std::time::Duration::ZERO, &mut allowed)
            .await
            .expect("a change nothing refuses: decided at once");
        assert!(!decision.governance_behind);
    }

    /// One writer has at most [`GOVERNANCE_WAITS_PER_WRITER`] changes waiting
    /// for governance at once: past that, a change is refused at once.
    #[tokio::test(start_paused = true)]
    async fn a_writer_waits_for_governance_with_a_bounded_number_of_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = shared(&tmp);
        {
            let mut runtime = ws.lock().await;
            runtime.project_access(
                DeviceAccess::from([(PAGE, access())]),
                tilo_peer_state::GovernanceHeads::default(),
            );
            runtime.test_governance = Some(vec![[9; 32]]);
        }
        let waiting: Vec<_> = (0..GOVERNANCE_WAITS_PER_WRITER)
            .map(|_| {
                let ws = Arc::clone(&ws);
                tokio::spawn(async move { access_deciding(&ws, PAGE, &[], |_, _| true).await })
            })
            .collect();
        tokio::task::yield_now().await;
        while ws
            .lock()
            .await
            .governance_waits
            .lock_recover()
            .get(&PAGE)
            .copied()
            != Some(GOVERNANCE_WAITS_PER_WRITER)
        {
            tokio::task::yield_now().await;
        }
        let over = access_deciding(&ws, PAGE, &[], |_, _| true);
        tokio::pin!(over);
        let decision = tokio::time::timeout(std::time::Duration::ZERO, &mut over)
            .await
            .expect("a change past the writer's bound is decided at once");
        assert!(decision.governance_behind);
        // Another writer still waits.
        let mut other = Box::pin(access_deciding(&ws, LAPTOP, &[], |_, _| true));
        ws.lock().await.project_access(
            DeviceAccess::from([(PAGE, access()), (LAPTOP, access())]),
            tilo_peer_state::GovernanceHeads::default(),
        );
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, &mut other)
                .await
                .is_err()
        );
        for wait in waiting {
            assert!(wait.await.unwrap().governance_behind);
        }
        drop(other);
        assert!(ws.lock().await.governance_waits.lock_recover().is_empty());
    }

    /// A burst from one sender is carried whole, and another sender's piece
    /// gets in within a turn of the queue rather than behind the burst.
    #[test]
    fn a_queue_carries_a_burst_whole_and_lets_another_sender_in() {
        let piece = |from: [u8; 32], frame: Vec<u8>| Envelope::DocumentContent {
            workspace_id: WorkspaceId::from_u128(7),
            to_device_public_key: [3; 32],
            from_device_public_key: from,
            frame,
        };
        let queue = crate::content_queue::ContentQueue::default();
        let (tx, mut rx) = mpsc::channel(2);
        let first = [1_u8; 32];
        let second = [2_u8; 32];
        for number in 0..40_u8 {
            queue.deliver_piece(first, piece(first, vec![number]), &tx);
        }
        queue.deliver_piece(second, piece(second, vec![0xBB]), &tx);
        let mut received = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let Envelope::DocumentContent {
                from_device_public_key,
                ..
            } = envelope
            {
                received.push(from_device_public_key);
            }
            queue.serve_waiting(&tx);
        }
        assert_eq!(received.len(), 41, "a burst under the bound arrives whole");
        let second_at = received.iter().position(|from| *from == second).unwrap();
        assert!(
            second_at <= 4,
            "the second sender's piece came within a turn, after {second_at} of the first's"
        );
    }

    /// A repository-sized upload of pieces is carried whole: none is
    /// refused for being too much content.
    #[test]
    fn a_repositorys_worth_of_pieces_is_carried_whole() {
        let piece_bytes = 192 * 1024;
        let pieces = 350;
        assert!(
            pieces * piece_bytes > 64 * 1024 * 1024,
            "a repository-sized upload"
        );
        let queue = crate::content_queue::ContentQueue::default();
        let (tx, mut rx) = mpsc::channel(32);
        let sender = [1_u8; 32];
        let mut received = 0;
        for _ in 0..pieces {
            queue.deliver_piece(
                sender,
                Envelope::DocumentContent {
                    workspace_id: WorkspaceId::from_u128(7),
                    to_device_public_key: [3; 32],
                    from_device_public_key: sender,
                    frame: vec![0; piece_bytes],
                },
                &tx,
            );
            // The connection's writer takes what its queue holds as it goes.
            while rx.try_recv().is_ok() {
                received += 1;
                queue.serve_waiting(&tx);
            }
        }
        while rx.try_recv().is_ok() {
            received += 1;
            queue.serve_waiting(&tx);
        }
        assert_eq!(received, pieces, "no piece of the upload was refused");
        assert_eq!(queue.waiting_pieces(), 0);
    }

    /// One sender's waiting pieces are bounded, so a flood spills its own
    /// pieces rather than growing without bound.
    #[test]
    fn a_senders_waiting_pieces_are_bounded() {
        let queue = crate::content_queue::ContentQueue::default();
        let (tx, _rx) = mpsc::channel(1);
        let sender = [1_u8; 32];
        for number in 0..=u8::MAX {
            queue.deliver_piece(
                sender,
                Envelope::DocumentContent {
                    workspace_id: WorkspaceId::from_u128(7),
                    to_device_public_key: [3; 32],
                    from_device_public_key: sender,
                    frame: vec![number],
                },
                &tx,
            );
        }
        assert!(queue.waiting_pieces() <= crate::content_queue::PER_SENDER);
    }

    /// A member takes its places at content work and gives them back; another
    /// member is not held behind it.
    #[test]
    fn a_member_takes_no_more_than_its_places() {
        let work = Arc::new(crate::content_work::ContentWork::default());
        let member = [1_u8; 32];
        let places: Vec<_> = (0..crate::content_work::PER_MEMBER)
            .map(|_| work.take_place(&member).unwrap())
            .collect();
        assert!(
            work.take_place(&member).is_none(),
            "its places are all at work"
        );
        assert!(
            work.take_place(&[2_u8; 32]).is_some(),
            "another member is not held behind it"
        );
        drop(places);
        assert!(
            work.take_place(&member).is_some(),
            "a finished read gives its place back"
        );
    }
}
