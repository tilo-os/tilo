//! This device's workspaces: the engine that holds the state of every workspace
//! the person joined, and the enrollment that brings a joined workspace's state
//! here.
//! A workspace's state — its people, what its apps recorded, its history — is held
//! by the peer-state engine beside the desk ([`NativeWorkspaceOwner`]), in a file of
//! its own under this device's state ([`FILE`]). Joining redeems the invitation its
//! door answered with ([`tilo_peer_state::owner::enroll::prepare_and_complete`]),
//! and a late joiner reads the whole history, not only what is recorded from now
//! on. The engine keeps the id each workspace on the desk is held under
//! ([`Slots`]), because an enrollment installs into the workspace made for it.

// The page's session constructs no engine: it wires its workspaces with its own host, so only a
// desktop's build reads the state held here.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page's session wires its workspaces with its own host"
    )
)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use tilo_clock::time::SystemTime;

use tilo_action_policy::WorkspacePolicy;
use tilo_channels::{ChannelAccess, ChannelKeyProvider};
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
use tilo_identity_host::custody::HostIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::KeptRecord;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::join::MemberRole;
#[cfg(target_os = "linux")]
use tilo_peer_state::join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, PendingInvitation,
};
#[cfg(not(target_arch = "wasm32"))]
use tilo_peer_state::owner::Change;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_peer_state::owner::{DocumentRead, History, Holding};
#[cfg(not(target_arch = "wasm32"))]
use tilo_peer_state::owner::{FileHost, open_file_with, owner_file_of};
use tilo_user_identity::RevocationList;

use crate::channels;
use crate::custody::Custody;
#[cfg(not(target_arch = "wasm32"))]
use crate::identity;

/// The engine's record, under this device's state directory: the name an earlier build kept it
/// under, so the state it wrote is still read.
const FILE: &str = "rooms.json";

/// The largest document the session reads out of a workspace.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// Someone a workspace has in it: the person, as their user key in hex, the name the workspace
/// knows them by, and the part they take in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// The person (user root, hex).
    pub person: String,
    /// The name the workspace knows them by, if it knows one.
    pub name: String,
    /// The part they take in the workspace.
    pub role: MemberRole,
    /// Whether the workspace removed them.
    pub removed: bool,
}

/// Why a workspace is not the person's to delete here, when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotDeletable {
    /// This device does not hold it.
    NotHeld,
    /// Someone else is still in it, so it is not this device's to delete.
    OthersInIt,
    /// Another of the person's devices holds it too: deleting it here would not delete it for them.
    OtherDevice,
    /// It belongs to someone else.
    NotOwner,
}

/// An invitation this device made: the link a person hands over, the part it brings whoever redeems
/// it in at, and when it stops working.
#[derive(Clone, Debug)]
pub struct Issued {
    /// The invitation's id, as the workspace's own records name it.
    pub invite: String,
    /// The text a device joins with.
    pub link: String,
    /// The part it brings the person in at.
    pub role: MemberRole,
    /// When it stops working, in nanoseconds since the Unix epoch.
    pub expires_at_ns: u64,
}

/// The workspace id each workspace on the desk is held under, as the engine's record names them and
/// the desk names its own.
#[derive(Default)]
pub struct Slots(std::sync::Mutex<BTreeMap<WorkspaceSlot, WorkspaceId>>);

impl std::fmt::Debug for Slots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.0.lock_recover().iter()).finish()
    }
}

impl Slots {
    fn of(workspaces: impl IntoIterator<Item = (WorkspaceSlot, WorkspaceId)>) -> Self {
        Self(std::sync::Mutex::new(workspaces.into_iter().collect()))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl FileHost for Slots {
    fn slot_workspace(&self, slot: WorkspaceSlot) -> Option<WorkspaceId> {
        self.0.lock_recover().get(&slot).copied()
    }

    /// Bind the workspace an enrollment installs into to its workspace id before the record reaches
    /// the disk: a publication refused here installs nothing.
    fn publish(
        &self,
        _next: &tilo_peer_state::owner::NativeAuthorityStorage,
        change: Change<'_>,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let bound = match change {
            Change::Workspace {
                workspace,
                into: Some(target),
                ..
            } => Some((target.workspace, workspace)),
            _ => None,
        };
        if let Some((workspace, held)) = bound {
            self.0.lock_recover().insert(workspace, held);
        }
        let written = write();
        if written.is_err()
            && let Some((workspace, _)) = bound
        {
            self.0.lock_recover().remove(&workspace);
        }
        if let Change::Released { workspace } = change {
            self.0.lock_recover().retain(|_, held| *held != workspace);
        }
        written
    }
}

/// What this device may seal a workspace's record with ([`Engine::seal_keys`]): the channel's keys,
/// the wait for them, or this device's own lack of a secret store to hold any.
pub enum SealKeys {
    /// The keys this device seals the record's writes with.
    Here(channel_crypto::ChannelContentKeys),
    /// The keys are not here yet: the answer arrives as the workspace's other messages do, and the
    /// write waits for it.
    Waiting,
    /// This device has no secret store, so it holds no channel keys of any
    /// workspace: a sealed record is one it can neither write nor read here,
    /// and no workspace refused anything.
    NoSecretStore,
}

/// This device's workspaces, and the identity they are held with.
pub struct Engine {
    owner: NativeWorkspaceOwner,
    slots: Arc<Slots>,
    identity: HostIdentity,
    /// Where the workspaces' channels find this device's keys, once the channel service is running:
    /// the session opens the workspaces before it opens the service, and every reader of a sealed
    /// record goes through this.
    keys: ChannelKeyProvider,
    /// How this host proves this device still has custody of a workspace before it commits there.
    custody: Arc<dyn crate::custody::Custody>,
    /// The workspace transport of this session's collaboration plane, once it runs: a record the
    /// workspace's hosts send only once asked is asked for here, and the plane is gone while this
    /// is `None` .
    link: std::sync::Mutex<Option<Arc<channels::ChannelLink>>>,
    /// The desk's workspaces this device could not found here: founding one is asked for again only
    /// by a later session, not by every pass over the desk.
    unfounded: std::sync::Mutex<std::collections::BTreeSet<WorkspaceId>>,
    /// This device's workspaces as its other devices reach them, when it keeps a state directory to
    /// hold their frames in.
    #[cfg(not(target_arch = "wasm32"))]
    directed: Option<tilo_peer_state::direct::Owning>,
    /// What lands here from the workspaces' other devices: what a read waiting for a record it
    /// holds none of wakes on.
    arrivals: Arc<crate::arrivals::Arrivals>,
    /// This device's own writes its replica has not taken in yet ([`HeldWrites`]).
    held: HeldWrites,
    /// Keys of records whose channel this device takes no part in, handed to it by the records'
    /// host ([`Self::hand_keys`]): a sealed record they open reads with them.
    handed: std::sync::Mutex<RouteKeys>,
    /// The keys each record's channel last gave this device: they seal and open while the channel
    /// asks for access again.
    channel_keys: std::sync::Mutex<RouteKeys>,
    /// Sealed records as this device last opened them for a reader, so the next read opens only
    /// what was sealed since: a record its apps follow is opened once per write, not whole per read.
    kept: std::sync::Mutex<KeptRecords>,
}

/// The sealed records this device keeps opened, by workspace, app and record, with the keys that
/// opened each.
type KeptRecords = BTreeMap<(WorkspaceId, String, String), (KeysUsed, KeptRecord)>;

/// The keys a sealed record was opened with: the channel and epoch of the record's own keys and of
/// the channel's history keys. A rotation moves the epoch.
type KeysUsed = [Option<(collab_types::ChannelId, u64)>; 2];

/// What was kept opened of a record, when it was opened with exactly the keys `used` now: a
/// record opened under other keys is forgotten.
fn kept_under<K: Default>(kept: Option<(KeysUsed, K)>, used: &KeysUsed) -> K {
    match kept {
        Some((opened_with, kept)) if opened_with == *used => kept,
        _ => K::default(),
    }
}

/// Keys of records, by workspace and record.
type RouteKeys =
    BTreeMap<(WorkspaceId, collab_types::ChannelRoute), channel_crypto::ChannelContentKeys>;

/// Opens one sealed write of a record into its plaintext.
type OpenSealed<'a> = &'a mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>;

/// How many sealed records [`Engine::read_record`] keeps opened between reads.
const KEPT_RECORDS: usize = 64;

/// How much of the workspaces' sealed history this device keeps to hand their other devices: every
/// frame that passes through it, oldest retired first once it holds this much.
#[cfg(not(target_arch = "wasm32"))]
const FRAMES_KEPT: tilo_peer_state::store::Appetite = tilo_peer_state::store::Appetite::Bounded {
    max_bytes: 512 * 1024 * 1024,
};

/// A record's bytes, with the heads they were last saved at: what [`Engine::record_since`] answers
/// when the record changed since the caller last read it.
type SavedRecord = Option<(Vec<u8>, [u8; 32])>;

/// The writes this device made that its replica has not taken in yet, by record, as document
/// frames in the clear: they wait for their record's keys (nothing goes out unsealed) or for room
/// in custody. The session that writes them keeps them here, and its readers read them over the
/// replica ([`Engine::read_record`]); nobody else sees them until they land.
#[derive(Clone, Default)]
pub struct HeldWrites(Arc<std::sync::Mutex<BTreeMap<DocumentRoute, Vec<Vec<u8>>>>>);

impl HeldWrites {
    /// The held writes, to read or change.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<DocumentRoute, Vec<Vec<u8>>>> {
        self.0.lock_recover()
    }
}

/// What a read of one of a workspace's records found ([`Engine::read_record`]).
pub enum RecordReading {
    /// The record as the workspace holds it, sealed writes opened with the record's channel keys.
    Captured {
        /// Names exactly these heads: equal revisions are equal documents.
        revision: [u8; 32],
        /// The document's heads, sorted.
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    /// Only the changes since the heads the caller holds, which they complete to `heads`.
    Changes {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    /// The record stands where the caller last read it.
    Unchanged([u8; 32]),
    /// The record is not here yet: the workspace's host was asked for it, or the record's keys are
    /// on their way, and it reads once they are.
    Pending,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("workspaces", &self.workspaces())
            .finish_non_exhaustive()
    }
}

/// This device's channel keys, as its owner compacts the sealed histories of its records with
/// them wherever it writes, linked or not
/// ([`NativeWorkspaceOwner::keep_sealed_histories`]).
pub struct HistoryKeys(pub ChannelKeyProvider);

impl tilo_peer_state::owner::SealedHistoryKeys for HistoryKeys {
    fn open(
        &self,
        workspace: WorkspaceId,
        write: &collab_types::SignedOp,
    ) -> Result<Vec<u8>, String> {
        self.0
            .get()
            .ok_or("this device's channel service is not running")?
            .open_history_write(workspace, write)
    }

    fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        snapshot: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        self.0
            .get()
            .ok_or("this device's channel service is not running")?
            .seal_checkpoint(workspace, app, record, snapshot)
    }
}

impl Engine {
    /// This device's workspaces as `owner` holds them, with the identity they are held under: what
    /// a host supplies whichever file, store or record it keeps them in.
    #[must_use]
    pub fn holding(
        owner: NativeWorkspaceOwner,
        slots: Arc<Slots>,
        identity: HostIdentity,
        custody: Arc<dyn Custody>,
    ) -> Self {
        let keys = ChannelKeyProvider::default();
        owner.keep_sealed_histories(Arc::new(HistoryKeys(keys.clone())));
        Self {
            owner,
            slots,
            identity,
            keys,
            custody,
            link: std::sync::Mutex::new(None),
            unfounded: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            #[cfg(not(target_arch = "wasm32"))]
            directed: None,
            arrivals: Arc::new(crate::arrivals::Arrivals::default()),
            handed: std::sync::Mutex::default(),
            channel_keys: std::sync::Mutex::default(),
            held: HeldWrites::default(),
            kept: std::sync::Mutex::default(),
        }
    }

    /// Open this device's workspaces under `state`, with the workspaces the desk
    /// already holds. A session that keeps no state (a screenshot, a test without a
    /// state directory) holds no workspace.
    /// # Errors
    /// The record holds no valid state, or the file cannot be written.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(
        identity: HostIdentity,
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = (WorkspaceSlot, WorkspaceId)>,
        custody: Arc<dyn Custody>,
    ) -> Result<Self, String> {
        let state = state.ok_or("this session keeps no state, so it holds no workspace")?;
        let path = record_path(state);
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        let slots = Arc::new(Slots::of(workspaces));
        let owner = open_file_with(
            &owner_file_of(&path),
            Arc::clone(&slots) as Arc<dyn FileHost>,
        )?;
        let mut engine = Self::holding(owner.clone(), slots, identity, custody);
        // What the workspaces' other devices reach this one at, keeping the frames each brings: a
        // device that holds a workspace is a store device of it.
        engine.directed = match tilo_peer_state::store::Store::open(frames_path(state), FRAMES_KEPT)
        {
            Ok(store) => Some(tilo_peer_state::direct::Owning::new(
                owner,
                engine.identity.signing_key()?,
                Arc::new(store),
            )),
            Err(error) => {
                tracing::warn!(%error, "this device serves no workspace to its other devices");
                None
            }
        };
        Ok(engine)
    }

    /// This device's engine, when this computer has an identity and a state directory to keep its
    /// workspaces in.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn for_this_device(
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = (WorkspaceSlot, WorkspaceId)>,
        custody: Arc<dyn Custody>,
    ) -> Result<Self, String> {
        let identity = identity::device_identity()
            .ok_or("this computer has no identity yet, so it holds no workspace")?;
        Self::open(identity, state, workspaces, custody)
    }

    /// The workspace ids this device holds.
    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        self.owner.workspaces()
    }

    /// Whether this device holds this workspace.
    #[must_use]
    pub fn holds(&self, workspace: WorkspaceId) -> bool {
        self.owner.contains_workspace(workspace)
    }

    /// The host `workspace` waits on to take this device in, by the name its person gave it,
    /// while this device holds the workspace but is not its member yet: the workspace's record
    /// names another device (the one this machine was copied from) until its host admits this
    /// one, and only the host can. `None` for a workspace this device is a member of, or does not
    /// hold.
    #[must_use]
    pub fn awaited_host(&self, workspace: WorkspaceId) -> Option<String> {
        if !self.holds(workspace) {
            return None;
        }
        let own = self.identity.device_public_key();
        self.owner
            .with_current(workspace, |view| {
                Ok((view.device().device_public_key != own).then(|| {
                    view.transport_hint()
                        .map(|hint| hint.host.device_label.clone())
                        .unwrap_or_default()
                }))
            })
            .ok()
            .flatten()
    }

    /// What the workspace `workspace` holds of the app `app`'s `record`: the document
    /// as the workspace last admitted it, also what it held before this device joined.
    /// A sealed record opens with this device's channel keys, which reach back past the
    /// membership it joined in; a device that holds none yet asks for them
    /// ([`Self::seal_keys`]) and reads the record once they are here. A record the
    /// hosts send only once asked is asked for on its first read.
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record
    /// is sealed and this device holds no keys of it, or the document cannot be read.
    pub fn record(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        self.record_since(workspace, app, record, None)
            .map(|read| read.map(|(bytes, _)| bytes))
    }

    /// What `workspace` holds of `app`'s `record` for a reader that merged it last at the sorted
    /// `heads` an earlier read handed out, or holds nothing of it: only the changes since those
    /// heads where the record holds them ([`RecordReading::Changes`]), the record whole where it
    /// does not, so a reader that follows a record merges what changed rather than all of it.
    /// # Errors
    /// As [`Self::record`].
    pub fn record_update(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
        heads: Option<&[[u8; 32]]>,
    ) -> Result<RecordReading, String> {
        let heads = heads.unwrap_or_default();
        self.read_record(
            workspace,
            (app, record),
            Holding {
                revision: (!heads.is_empty())
                    .then(|| tilo_peer_state::owner::revision_of_heads(heads)),
                heads,
            },
            MAX_DOCUMENT_BYTES,
        )
    }

    /// [`Self::record`], with the revision the bytes were saved at, and told the revision
    /// `known` this caller already holds: a record whose admitted heads still are those
    /// is answered as unchanged without capturing or saving the document at all.
    /// # Errors
    /// As [`Self::record`].
    pub fn record_since(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
        known: Option<[u8; 32]>,
    ) -> Result<SavedRecord, String> {
        Ok(
            match self.read_record(
                workspace,
                (app, record),
                Holding {
                    revision: known,
                    heads: &[],
                },
                MAX_DOCUMENT_BYTES,
            )? {
                RecordReading::Captured {
                    revision, bytes, ..
                } => Some((bytes, revision)),
                RecordReading::Changes { .. }
                | RecordReading::Unchanged(_)
                | RecordReading::Pending => None,
            },
        )
    }

    /// What `workspace` holds of `app`'s `record` now, for the reader this device is: at most
    /// `maximum` bytes of it, and the heads it stands at; for a reader `holding` heads an earlier
    /// read handed it, only the changes since.
    ///
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record is sealed
    /// and this device holds no keys of it, or the document cannot be read.
    pub fn read_record(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
    ) -> Result<RecordReading, String> {
        let Some(held) = self.own_writes(workspace, app, record) else {
            return self.read_replica(workspace, (app, record), holding, maximum);
        };
        // This device's own writes the replica has not taken in yet read over what it holds.
        let base = match self.read_replica(workspace, (app, record), Holding::default(), maximum) {
            Ok(RecordReading::Captured { bytes, .. }) => Some(bytes),
            _ => None,
        };
        let author = self.identity.device_cert.device_public_key;
        let tilo_peer_state::document_frame::OwnRead {
            bytes,
            heads,
            revision,
        } = tilo_peer_state::document_frame::with_own_writes(base.as_deref(), author, &held)?;
        if holding.revision == Some(revision) {
            return Ok(RecordReading::Unchanged(revision));
        }
        Ok(RecordReading::Captured {
            revision,
            heads,
            bytes,
        })
    }

    /// The revision `workspace`'s `app`'s `record` stands at for the reader this device is,
    /// decided as [`Self::read_record`] decides it but reading nothing out: `None` while the
    /// record is not here yet, or only a read decides it (this device's own writes the replica
    /// has not taken in yet).
    ///
    /// # Errors
    /// As [`Self::read_record`].
    pub fn record_revision(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<[u8; 32]>, String> {
        if self.own_writes(workspace, app, record).is_some() {
            return Ok(None);
        }
        let reader = self.identity.device_cert.device_public_key;
        Ok(self
            .opened(workspace, (app, record), &mut |workspace, open, kept| {
                self.owner.with_current(workspace, |view| {
                    view.document_revision(
                        reader,
                        (app, record),
                        tilo_clock::wall(),
                        History { open, kept },
                    )
                })
            })?
            .flatten())
    }

    /// The first second after `now` at which a read of `workspace`'s records can be decided
    /// differently with nothing written ([`crate::channels::next_policy_change`]).
    ///
    /// # Errors
    /// `workspace` names no workspace.
    pub fn next_policy_change(&self, workspace: &str, now: u64) -> Result<Option<u64>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        Ok(crate::channels::next_policy_change(
            &self.owner,
            workspace,
            now,
        ))
    }

    /// This device's own writes to the record its replica has not taken in yet.
    fn own_writes(&self, workspace: &str, app: &str, record: &str) -> Option<Vec<Vec<u8>>> {
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            record: record.to_owned(),
        };
        self.held
            .lock()
            .get(&route)
            .filter(|frames| !frames.is_empty())
            .cloned()
    }

    /// What this device's replica holds of the record, its own held writes left out.
    fn read_replica(
        &self,
        workspace: &str,
        route: (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
    ) -> Result<RecordReading, String> {
        Ok(self
            .opened(workspace, route, &mut |workspace, open, kept| {
                self.read_in(workspace, route, holding, maximum, History { open, kept })
            })?
            .unwrap_or(RecordReading::Pending))
    }

    /// `read` of the record of `workspace` at `route`, with how its sealed writes open — the
    /// record's keys, then the channel's own history keys — and what this device kept opened of
    /// it: `None` while the keys of a sealed record are not here.
    fn opened<T>(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        read: &mut dyn FnMut(WorkspaceId, OpenSealed<'_>, &mut KeptRecord) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let route = collab_types::ChannelRoute::new(app, record);
        let sealed = self.owner.with_current(workspace, |view| {
            Ok(view.peer().publication().is_sealed_record(app, record))
        })?;
        let keys: Option<tilo_channels::HistoryKeys> = if sealed {
            match self.route_access(workspace, &route) {
                None => None,
                Some(ChannelAccess::Ready(keys)) => Some(keys.into()),
                Some(ChannelAccess::Waiting | ChannelAccess::Removed) => {
                    // Nothing opened under keys this device no longer holds answers a read
                    // until the record's channel gives it keys again.
                    self.kept.lock_recover().remove(&(
                        workspace,
                        app.to_owned(),
                        record.to_owned(),
                    ));
                    return Ok(None);
                }
                Some(ChannelAccess::NoSecretStore) => {
                    return Err(
                        "this device has no secret store, so it holds no channel keys".to_owned(),
                    );
                }
                Some(ChannelAccess::Unavailable) => {
                    return Err("this device takes part in no channel of the record".to_owned());
                }
            }
        } else {
            None
        };
        // Taken before the workspace's view is held: the channel service's pump settles the groups
        // under its own lock and reads the workspace's governance there, so a read that asked it
        // for keys while holding the view would wait on it as it waits on the view.
        let history = self
            .keys
            .get()
            .and_then(|keys| keys.history_keys(workspace, &route));
        // A sealed write the keys this device holds do not open yet — its epoch's key, or the key
        // of the branch it was sealed on, has not reached this device — is a record still
        // arriving here: it reads whole once the keys have, as the app's own channel holds such a
        // write until they do. The record's keys open what they carry; the channel's own history
        // keys open the rest.
        let mut unopened = false;
        let key = (workspace, app.to_owned(), record.to_owned());
        // Taken out while it is read: a read of the same record meanwhile opens it whole. What was
        // opened under other keys than these — the channel rotated, as removing a member rotates
        // it — is let go, and the record opens whole under the keys held now.
        let used: KeysUsed = [
            keys.as_ref().map(tilo_channels::HistoryKeys::generation),
            history
                .as_ref()
                .and_then(|history| history.as_ref().ok())
                .map(tilo_channels::HistoryKeys::generation),
        ];
        let mut kept = kept_under(self.kept.lock_recover().remove(&key), &used);
        let read = read(
            workspace,
            &mut |write| {
                let opened = match (&keys, &history) {
                    (Some(keys), Some(Ok(history))) => {
                        keys.open(write).or_else(|_| history.open(write))
                    }
                    (Some(keys), _) => keys.open(write),
                    (None, Some(history)) => history.as_ref().map_err(Clone::clone)?.open(write),
                    (None, None) => Err("this session's channel service is not running".into()),
                };
                unopened |= opened.is_err();
                opened
            },
            &mut kept,
        );
        if sealed && kept.is_open() {
            let mut records = self.kept.lock_recover();
            if records.len() >= KEPT_RECORDS {
                records.pop_first();
            }
            records.insert(key, (used, kept));
        }
        match read {
            Err(_) if unopened => Ok(None),
            read => read.map(Some),
        }
    }

    /// [`Self::read_record`] under keys the caller holds itself rather than this device's channel
    /// service: what a caller that sealed a record's writes reads them back with. Nothing of it
    /// is kept: what these keys open is the caller's alone.
    fn read_in(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
        history: History<'_>,
    ) -> Result<RecordReading, String> {
        let reader = self.identity.device_cert.device_public_key;
        self.owner
            .with_current(workspace, |view| {
                view.read_document_with_history(
                    reader,
                    (app, record),
                    holding,
                    maximum,
                    tilo_clock::wall(),
                    history,
                )
            })
            .map(|read| match read {
                DocumentRead::Captured(document) => RecordReading::Captured {
                    revision: document.revision,
                    heads: document.heads,
                    bytes: document.bytes,
                },
                DocumentRead::Changes(changes) => RecordReading::Changes {
                    revision: changes.revision,
                    heads: changes.heads,
                    bytes: changes.bytes,
                },
                DocumentRead::Unchanged(revision) => RecordReading::Unchanged(revision),
                DocumentRead::CheckpointPending => RecordReading::Pending,
            })
    }

    /// Whether `workspace` holds `app`'s `record` here now: what a read of it opens, or a record
    /// this device holds nothing of that may still be on its way.
    #[must_use]
    pub fn holds_record(&self, workspace: &str, app: &str, record: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|workspace| {
            self.owner.with_current(workspace, |view| {
                Ok(view.peer().document(app, record).is_some())
            }) == Ok(true)
        })
    }

    /// Whether `workspace`'s host finished replaying to this device what it retains: every record
    /// comes with it, so one that is not here then is one the workspace has none of — unless the
    /// host sends records only when they are opened
    /// ([`Self::records_when_opened`]).
    #[must_use]
    pub fn host_replayed(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        self.link
            .lock_recover()
            .as_ref()
            .is_some_and(|link| link.has_replayed(workspace))
    }

    /// Whether `workspace`'s host sends this device only the workspace's governance, its shared
    /// desktop and the heads of its chained records, and every other record once this device opens
    /// it ([`Self::open_record`]): a record that is not here is not new until the host answered it
    /// holds none of it ([`Self::record_answer`]).
    #[must_use]
    pub fn records_when_opened(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        self.link
            .lock_recover()
            .as_ref()
            .is_some_and(|link| link.records_when_opened(workspace))
    }

    /// Ask `workspace`'s hosts for `app`'s `record`
    /// (`neighbour_wire::app_document_channel`): what they hold of it, and their changes to it from
    /// then on. Asked once per record while the plane runs; `false` when there is no plane to ask
    /// on, so nothing was asked.
    pub fn open_record(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let Some(link) = self.link.lock_recover().clone() else {
            return false;
        };
        link.open_record(workspace, neighbour_wire::app_document_channel(app, record))
    }

    /// What `workspace`'s host answered about `app`'s `record`: `None` while the answer to
    /// [`Self::open_record`] is owed, and the frames it sent once it came — none when the workspace
    /// has none of the record, which is then new here.
    #[must_use]
    pub fn record_answer(&self, workspace: &str, app: &str, record: &str) -> Option<u32> {
        let workspace = WorkspaceId::parse(workspace)?;
        let channel = neighbour_wire::app_document_channel(app, record);
        self.link
            .lock_recover()
            .as_ref()?
            .record_answer(workspace, &channel)
    }

    /// What ticks whenever a host answered for a record this device opened; `None` with no plane.
    #[must_use]
    pub fn record_answers(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.link.lock_recover().as_ref().map(|link| link.answers())
    }

    /// What ticks whenever a host finished its replay; `None` with no plane.
    #[must_use]
    pub fn replays(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.link.lock_recover().as_ref().map(|link| link.replays())
    }

    /// Whether this device is `workspace`'s own host: everything the workspace retains is here,
    /// and no other device replays any of it.
    #[must_use]
    pub fn hosts_itself(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|workspace| {
            self.host_of(workspace) == Some(self.identity.device_cert.device_public_key)
        })
    }

    /// Whether this device can ask its workspaces' hosts for what they retain: a session whose
    /// collaboration plane is not running reads only what it holds.
    #[must_use]
    pub fn reaches_host(&self) -> bool {
        self.link.lock_recover().is_some()
    }

    /// What lands here from the workspaces' other devices: the event a read waiting for one waits
    /// on.
    #[must_use]
    pub fn arrivals(&self) -> Arc<crate::arrivals::Arrivals> {
        Arc::clone(&self.arrivals)
    }

    /// Whether `certificate`'s holder reads `app`'s `record` in `workspace` here now: the workspace
    /// is held with this device's custody of it, its people certify the caller, and the caller's
    /// device is one the workspace's policy lets read the record.
    /// # Errors
    /// The workspace is not held here, this device has no custody of it, or the caller may not
    /// read the record now.
    pub fn authorizes_read(
        &self,
        workspace: &str,
        certificate: &tilo_user_identity::DeviceCert,
        app: &str,
        record: &str,
    ) -> Result<(), String> {
        use tilo_broker_core::custody::OwnerCustody as _;
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.check_custody(&self.owner, workspace)?;
        let reader = self.identity.device_cert.device_public_key;
        self.owner.with_current(workspace, |view| {
            let certified = view.peer().certificates().iter().any(|held| {
                held.device_public_key == certificate.device_public_key
                    && held.user_public_key == certificate.user_public_key
            });
            if !certified {
                return Err("the reader is no device the workspace certifies".to_owned());
            }
            view.peer()
                .check_document_read(reader, app, record, tilo_clock::wall())
        })
    }

    /// Every record `workspace` holds here, by its app and id. A workspace this device
    /// does not hold lists nothing.
    /// # Errors
    /// The workspace is not held here, or its documents cannot be listed now.
    pub fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let mut records = self
            .owner
            .with_current(id, |view| Ok(view.peer().records()))?;
        // A record this device wrote that its replica holds nothing of yet is one of them too.
        for route in self.held.lock().keys() {
            let named = (route.app_id.clone(), route.record.clone());
            if route.cluster_id == workspace && !records.contains(&named) {
                records.push(named);
            }
        }
        Ok(records)
    }

    /// Whether this device's person reads everything `workspace` holds here, so that it may be
    /// branched: this device holds it, and its governance names the person as its owner or as a
    /// member it has not removed.
    #[must_use]
    pub fn reads_whole(&self, workspace: WorkspaceId) -> bool {
        let person = hex::encode(self.identity.device_cert.user_public_key);
        let id = workspace.to_string();
        self.holds(workspace)
            && (self.owner_of(&id).is_ok_and(|owner| owner == person)
                || self.people(&id).is_ok_and(|people| {
                    people
                        .iter()
                        .any(|member| member.person == person && !member.removed)
                }))
    }

    /// Write `changes` to `record` of `app` in `workspace` as this device's own write, and publish
    /// it to the workspace's other devices: the record's founding write when `founding` (the whole
    /// document, which the workspace must not hold yet), a change of it otherwise. Sealed under
    /// `sealed` , the keys of the record's channel, where the workspace's records are its members'
    /// sealed channels — a founding write as a checkpoint, so the record is sealed from the start —
    /// and in the clear where they are documents this device commits.
    /// Answers the write as this device's apps take a member's: their copies of the record follow
    /// it only once it is handed to them.
    /// # Errors
    /// The workspace is not held here, a founding write finds the record held already, or the
    /// workspace refused the write.
    pub async fn write_record(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        changes: &[u8],
        founding: bool,
        sealed: Option<&channel_crypto::ChannelContentKeys>,
    ) -> Result<tilo_peer_state::owner::AppWrite, String> {
        use tilo_broker_core::custody::OwnerCustody as _;
        let device = self.identity.device_cert.device_public_key;
        let now = tilo_clock::wall();
        let artifact = collab_types::document_artifact_id(app, &workspace.to_string(), record);
        let frame = if founding {
            collab_types::DocumentMutation::Snapshot(changes.to_vec())
        } else {
            collab_types::DocumentMutation::Incremental(changes.to_vec())
        }
        .encode()
        .map_err(|error| error.to_string())?;
        let mut author = None;
        let draft = |view: &tilo_peer_state::owner::NativeCommittedView| {
            if founding && view.peer().document(app, record).is_some() {
                return Err(format!("{record} of {app} is in the workspace already"));
            }
            author = Some(
                view.peer()
                    .permissions(device, now)
                    .map(tilo_peer_state::MemberAuthority::standing)
                    .map_err(|error| error.to_string())?,
            );
            let mut seal = sealed.map(|keys| {
                move |plaintext: &[u8]| {
                    keys.seal_content(
                        artifact,
                        collab_types::ContentRetention::of_frame(plaintext),
                        plaintext,
                    )
                    .map_err(|error| error.to_string())
                }
            });
            let op = view.peer().draft_write(
                (app, record),
                &frame,
                seal.as_mut()
                    .map(|seal| seal as &mut tilo_peer_state::SealCheckpoint<'_>),
                device,
                0,
                now,
            )?;
            Ok(vec![tilo_peer_state::owner_commit::OwnerCommitRequest {
                workspace: workspace.to_string(),
                operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                    app: app.to_owned(),
                    record: record.to_owned(),
                    envelope: Box::new(op),
                },
            }])
        };
        let committed = self
            .owner
            .write(workspace, draft, self.identity.signer.as_ref(), true, now)
            .await?
            .pop()
            .ok_or("the workspace has no room for the write now")?;
        self.schedule_publication(workspace);
        let tilo_peer_state::owner_commit::CommitOperation::Document { envelope, .. } =
            committed.0.operation
        else {
            return Err("a record's write is not a document write".to_owned());
        };
        Ok(tilo_peer_state::owner::AppWrite {
            peer: neighbour_wire::peer_handle(&device),
            app_id: app.to_owned(),
            cluster_id: workspace.to_string(),
            record_id: record.to_owned(),
            author: author.ok_or("the write was not made")?,
            replayed: false,
            envelope: *envelope,
        })
    }

    /// [`Self::record`] of a sealed record, opened with `keys` rather than this device's channel
    /// service: what a caller that holds a channel's keys itself reads back.
    pub fn record_opened_with(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        keys: &channel_crypto::ChannelContentKeys,
    ) -> Result<Option<Vec<u8>>, String> {
        let read = self.read_in(
            workspace,
            (app, record),
            Holding::default(),
            MAX_DOCUMENT_BYTES,
            History {
                open: &mut |write| {
                    let metadata = write.encryption.ok_or("the write is not sealed")?;
                    let ciphertext = collab_types::sealed_ciphertext(write)
                        .map_err(|error| error.to_string())?;
                    keys.open_content(write.artifact, &metadata, &ciphertext)
                        .map(|plaintext| plaintext.to_vec())
                        .map_err(|error| error.to_string())
                },
                kept: &mut KeptRecord::default(),
            },
        )?;
        Ok(match read {
            RecordReading::Captured { bytes, .. } => Some(bytes),
            RecordReading::Changes { .. }
            | RecordReading::Unchanged(_)
            | RecordReading::Pending => None,
        })
    }

    /// The keys of `route`'s record in `workspace` here: those its host handed this device, else
    /// its channel's — taking part in it asks the workspace's devices for access the first time,
    /// and its governance answers with the group — and while the channel asks again, the ones it
    /// last gave. `None` without a channel service.
    fn route_access(
        &self,
        workspace: WorkspaceId,
        route: &collab_types::ChannelRoute,
    ) -> Option<ChannelAccess> {
        let at = (workspace, route.clone());
        if let Some(handed) = self.handed.lock_recover().get(&at) {
            return Some(ChannelAccess::Ready(handed.clone()));
        }
        let access = self.keys.get()?.access_record(workspace, route).0;
        let mut kept = self.channel_keys.lock_recover();
        Some(match access {
            ChannelAccess::Ready(keys) => {
                kept.insert(at, keys.clone());
                ChannelAccess::Ready(keys)
            }
            ChannelAccess::Waiting => kept.get(&at).map_or(ChannelAccess::Waiting, |keys| {
                ChannelAccess::Ready(keys.clone())
            }),
            gone => {
                kept.remove(&at);
                gone
            }
        })
    }

    /// The keys this device seals writes of `record` of `app` with in `workspace`.
    /// [`SealKeys::Waiting`] says they are not here yet and will arrive as the
    /// workspace's other messages do, which includes a channel the governance does not
    /// name this device in yet. [`SealKeys::NoSecretStore`] is this device's own lack,
    /// not any workspace's verdict.
    /// # Errors
    /// This session has no channel service, or the workspace removed this device from
    /// the record's channel: it holds no keys, and none are coming.
    pub fn seal_keys(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<SealKeys, String> {
        Ok(
            match self.route_access(workspace, &collab_types::ChannelRoute::new(app, record)) {
                None => return Err("this session's channel service is not running".to_owned()),
                Some(ChannelAccess::Ready(keys)) => SealKeys::Here(keys),
                // Asked and not admitted yet, or admitted nowhere the governance here can name:
                // either way the keys are not here yet.
                Some(ChannelAccess::Waiting | ChannelAccess::Unavailable) => SealKeys::Waiting,
                Some(ChannelAccess::Removed) => {
                    return Err(format!(
                        "the workspace removed this device from {record}'s channel"
                    ));
                }
                Some(ChannelAccess::NoSecretStore) => SealKeys::NoSecretStore,
            },
        )
    }

    /// The devices the workspace `workspace` admits, by the publisher each of their
    /// sessions is known by ([`channels::roster_of`]).
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn roster(&self, workspace: &str) -> Result<Vec<String>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        channels::roster_of(self, workspace).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| device.to_string())
                .collect()
        })
    }

    /// The devices the workspace admits, as a call's roster names them: whoever its governance
    /// certified and has not removed may reach a call in it.
    pub fn roster_devices(&self, workspace: &str) -> Result<Vec<[u8; 32]>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        channels::roster_of(self, workspace).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| *device.as_bytes())
                .collect()
        })
    }

    /// Who the workspace has in it, as its governance tells it: every person it was shared with, by
    /// the name it knows them by and the part they take in it. A joined workspace brings its people
    /// with it, so this is how the desk knows them before any of them is here.
    pub fn people(&self, workspace: &str) -> Result<Vec<Person>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
            let members = view
                .peer()
                .governance()
                .governed_members(tilo_clock::wall())?;
            Ok(members
                .into_iter()
                .map(|member| Person {
                    person: hex::encode(member.user_public_key),
                    name: member.display_name.unwrap_or_default(),
                    role: member.role,
                    removed: member.removed,
                })
                .collect())
        })
    }

    /// The people of `workspace` and the grants it makes, as its governance records
    /// them: the one source of who shares it, in what part, and what each may do.
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner
            .with_current(workspace, |view| view.peer().governance().governed_policy())
    }

    /// Whether this device's person may administer `workspace` here: the governance gives their
    /// person the grant action. A workspace decided by another device's governance says no,
    /// whatever this device holds of it.
    #[must_use]
    pub fn administers(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let device = self.identity.device_cert.device_public_key;
        self.owner
            .with_current(workspace, |view| {
                Ok(view
                    .peer()
                    .workspace()
                    .check_workspace_administration(device, tilo_clock::wall())
                    .is_ok())
            })
            .unwrap_or(false)
    }

    /// The name `workspace` knows `person` by, when it knows one: what an invitation made for them
    /// records as its invitee.
    pub fn name_of(&self, workspace: &str, person: [u8; 32]) -> Option<String> {
        let person = hex::encode(person);
        self.people(workspace)
            .ok()?
            .into_iter()
            .find(|member| member.person == person)
            .map(|member| member.name)
            .filter(|name| !name.is_empty())
    }

    /// The invitations into `workspace` this device's person issued that are still
    /// redeemable: a link is answered by the device that signed it.
    /// # Errors
    /// The workspace is not held here, or its join ledger cannot be read now.
    #[cfg(target_os = "linux")]
    pub fn invitations(
        &self,
        workspace: &str,
        now: SystemTime,
    ) -> Result<Vec<PendingInvitation>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.pending_invitations(workspace, now)
    }

    /// Cancel the invitation `invitation` into `workspace`, as the person who issued it:
    /// the link admits nobody from now on.
    /// # Errors
    /// The workspace is not held here, the invitation was not issued from this device's
    /// person, or the workspace cannot be published.
    #[cfg(target_os = "linux")]
    pub fn cancel_invitation(
        &self,
        workspace: &str,
        invitation: &str,
        now: SystemTime,
    ) -> Result<bool, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let invite_id = invitation_id(invitation)?;
        let identity = self.identity();
        self.owner.cancel_invitation(
            workspace,
            &CancelInvitationRequest {
                request_id: cancellation_id(invite_id),
                invite_id,
            },
            identity.signer.as_ref(),
            now,
        )
    }

    /// Whether this device's person may invite into `workspace` , as its governance answers it.
    /// [`Self::issue_invitation`] asks again, with the part its link hands out.
    #[must_use]
    pub fn may_invite(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let key = self.identity().signer.as_ref();
        self.owner
            .with_current(workspace, |view| {
                Ok(view.peer().governance().may_invite(key, tilo_clock::wall()))
            })
            .unwrap_or(false)
    }

    /// Whether this device's person may delete `workspace` : they own it, no one else is in it, and
    /// this device is the only one holding it — so a workspace taken off this device is a workspace
    /// gone everywhere, and not one another of their devices would list again.
    #[must_use]
    pub fn may_delete(&self, workspace: WorkspaceId) -> bool {
        self.not_deletable(workspace).is_none()
    }

    /// Why `workspace` is not this device's person's to delete here, when it is not: what the
    /// surfaces offering deleting say beside a delete they cannot offer.
    #[must_use]
    pub fn not_deletable(&self, workspace: WorkspaceId) -> Option<NotDeletable> {
        if !self.holds(workspace) {
            return Some(NotDeletable::NotHeld);
        }
        let identity = self.identity();
        let person = hex::encode(identity.device_cert.user_public_key);
        let device = identity.device_cert.device_public_key;
        let workspace_id = workspace.to_string();
        let alone = self.people(&workspace_id).is_ok_and(|people| {
            people
                .iter()
                .all(|member| member.removed || member.person == person)
        });
        if !alone {
            return Some(NotDeletable::OthersInIt);
        }
        let only_here = self
            .roster_devices(&workspace_id)
            .is_ok_and(|devices| devices == [device]);
        if !only_here {
            return Some(NotDeletable::OtherDevice);
        }
        if !self
            .owner_of(&workspace_id)
            .is_ok_and(|owner| owner == person)
        {
            return Some(NotDeletable::NotOwner);
        }
        None
    }

    /// Take `workspace` out of this device's record, with everything of it this device
    /// holds. Answers whether the record held it.
    /// # Errors
    /// The record cannot be published.
    pub fn delete(&self, workspace: WorkspaceId) -> Result<bool, String> {
        self.kept
            .lock_recover()
            .retain(|(held, _, _), _| *held != workspace);
        self.owner.release_workspace(workspace)
    }

    /// The parts this device's person may hand out in an invitation into `workspace` , from the
    /// least to the most: what the surfaces offering a part to grant list. Empty when the person
    /// may not invite at all, since no part is theirs to hand out then.
    #[must_use]
    pub fn invitable_roles(&self, workspace: &str) -> Vec<MemberRole> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        let key = self.identity().signer.as_ref();
        self.owner
            .with_current(workspace, |view| {
                let now = tilo_clock::wall();
                Ok([
                    MemberRole::Viewer,
                    MemberRole::Commenter,
                    MemberRole::Editor,
                    MemberRole::Moderator,
                ]
                .into_iter()
                .filter(|role| {
                    view.peer()
                        .governance()
                        .may_invite_role(key, (*role).into(), now)
                })
                .collect())
            })
            .unwrap_or_default()
    }

    /// An invitation into `workspace` this device's person makes, at the part `role`
    /// names, redeemable until `expires_in_seconds` from `now`: the link to hand to
    /// whoever is invited. A link is worth as much as the device that signed it being
    /// reachable; when `recipient_user_public_key` names someone, only a device
    /// certified for that person may redeem it.
    /// # Errors
    /// The workspace is not held here, this device's person may not invite into it or
    /// hand out that part, or the workspace cannot be published.
    #[cfg(target_os = "linux")]
    pub fn issue_invitation(
        &self,
        workspace: &str,
        role: MemberRole,
        invitee_name: Option<String>,
        recipient_user_public_key: Option<[u8; 32]>,
        expires_in_seconds: u64,
        endpoint: &iroh_base::EndpointAddr,
        now: SystemTime,
    ) -> Result<Issued, String> {
        let room = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let identity = self.identity();
        // What the joiner dials: this device's endpoint, signed by the device the grant names, as a
        // host of its own workspace. A link is handed to someone who may be anywhere, so it names
        // the addresses anyone can dial and the relay this device is reached through, never the
        // ones only this device's own network knows.
        let reachable = tilo_delivery_protocol::workspace_join::reachable(endpoint);
        let hint = self.own_host_hint(workspace, &reachable)?;
        let now_ns: u64 = now
            .duration_since(tilo_clock::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
            .try_into()
            .map_err(|_| "this clock is past what an invitation records".to_owned())?;
        let expires_at_ns = now_ns.saturating_add(expires_in_seconds.saturating_mul(1_000_000_000));
        let mut request_id = [0; 32];
        getrandom::fill(&mut request_id).map_err(|error| error.to_string())?;
        let issued = collab_types::signer::at_once(self.owner.issue_invitation(
            room,
            &identity.device_cert,
            &IssueInvitationRequest {
                request_id,
                role,
                recipient_user_public_key,
                invitee_name,
                expires_at_ns,
                inviter_name: None,
            },
            &[hint],
            identity.signer.as_ref(),
            now,
        ))
        .map_err(|error| error.to_string())??;
        Ok(Issued {
            invite: hex::encode(issued.invite_id),
            link: tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation),
            role,
            expires_at_ns,
        })
    }

    /// The revocations the workspace's governance records, as it last admitted them. `None` when
    /// this device holds no such workspace. What is checked against this is whether a shortcut's
    /// saver may still start it here.
    #[must_use]
    pub fn revocations(&self, workspace: &str) -> Option<RevocationList> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.owner
            .with_current(workspace, |view| {
                let mut list = RevocationList::new();
                for entry in view.peer().revocations() {
                    // The workspace verified every entry when it admitted it: an entry that does
                    // not verify now leaves its refusals unknown rather than trusting the rest.
                    list.insert(entry).map_err(|error| error.to_string())?;
                }
                Ok(list)
            })
            .ok()
    }

    /// Who governs the workspace, as the id its governance names. A joined workspace
    /// keeps the owner its host's governance has, never the person who joined.
    /// # Errors
    /// The workspace is not held here yet, or its governance cannot be read now.
    pub fn owner_of(&self, workspace: &str) -> Result<String, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
            let owner = view.peer().governance().owner_user_public_key();
            if owner == [0; 32] {
                // A workspace whose governance names nobody has no owner to carry: a claim made
                // here would be this device's, not the workspace's.
                return Err("the workspace's governance names no owner".to_owned());
            }
            Ok(hex::encode(owner))
        })
    }

    /// The verdicts `workspace` holds on writes: this device's own, and the ones other devices
    /// recorded on its writes. Each names the changes the refused write would have brought, so a
    /// history row reads where it is not.
    #[must_use]
    pub fn write_refusals(&self, workspace: &str) -> Vec<neighbour_wire::WriteRefusal> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        self.owner.write_refusals(workspace)
    }

    /// The verdicts other devices recorded on this device's writes, with the workspace each was
    /// filed in, where waiting changes nothing: what the person hears once per verdict, because
    /// their writes stop arriving.
    #[must_use]
    pub fn lasting_refusals(&self) -> Vec<(String, neighbour_wire::WriteRefusal)> {
        let mine = self.identity.device_cert.device_public_key;
        self.workspaces()
            .into_iter()
            .flat_map(|workspace| {
                let name = workspace.to_string();
                self.owner
                    .write_refusals(workspace)
                    .into_iter()
                    .filter(|refusal| refusal.author == mine && refusal.reason.permanent())
                    .map(move |refusal| (name.clone(), refusal))
            })
            .collect()
    }

    /// What each device of `workspace` calls itself, by its key.
    #[must_use]
    pub fn device_labels(&self, workspace: &str) -> std::collections::BTreeMap<[u8; 32], String> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return std::collections::BTreeMap::new();
        };
        self.owner.device_labels(workspace)
    }

    /// The engine's owner, for what holds the workspace's session.
    pub fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
    }

    /// What this device answers for the workspaces it holds, beside the planes its endpoint already
    /// serves: the direct link their other devices sync on, and catch up from the frames this one
    /// kept.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn direct_protocols(&self) -> Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)> {
        self.directed.as_ref().map_or_else(Vec::new, |directed| {
            vec![(
                tilo_peer_state::direct::DIRECT_ALPN.to_vec(),
                Box::<dyn iroh::protocol::DynProtocolHandler>::from(directed.clone()),
            )]
        })
    }

    /// The devices that take part in this device's workspaces through it, as their workspace's host
    /// ([`Self::direct_protocols`]): what they write reaches this device's apps and the workspace's
    /// own host through this device, and what the workspace writes elsewhere reaches them through
    /// it too.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn members(&self) -> Option<tilo_peer_state::direct::Owning> {
        self.directed.clone()
    }

    /// Where `workspace` 's host is reached and the device its enrollment pins: the address its
    /// live plane is dialled at.
    #[cfg(target_os = "linux")]
    pub fn workspace_host(
        &self,
        workspace: WorkspaceId,
    ) -> Option<([u8; 32], iroh_base::EndpointAddr)> {
        self.owner.host_addr(workspace)
    }

    /// The device hosting `workspace`: the one whose host orders its commits,
    /// which is where the person's other devices enter it.
    pub fn host_of(&self, workspace: WorkspaceId) -> Option<[u8; 32]> {
        self.owner
            .with_current(workspace, |view| {
                Ok(view.workspace_host().device_public_key)
            })
            .ok()
    }

    /// Where the hosts of the workspaces this device holds are reached, by workspace id, postcard
    /// encoded: what the person's other devices need to enter a workspace listed here at the device
    /// that hosts it.
    pub fn host_hints(&self) -> Vec<(String, Vec<u8>)> {
        self.owner
            .workspaces()
            .into_iter()
            .filter_map(|workspace| {
                // The hint itself, as the device that reads it decodes it: not the answer
                // whether this device has one, whose tag would lead the bytes.
                let hint = self
                    .owner
                    .with_current(workspace, |view| view.transport_hint().cloned())
                    .ok()?;
                let bytes = postcard::to_stdvec(&hint).ok()?;
                Some((workspace.to_string(), bytes))
            })
            .collect()
    }

    /// Where a read asks the workspaces' hosts for a record they send only once asked: the plane
    /// this session's channels travel on.
    pub fn attach_link(&self, link: Arc<channels::ChannelLink>) {
        // The devices that take part in a workspace through this one hear its channels' governance
        // through this one alone, and what they say reaches the channels here and the workspace's
        // own host through it.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(directed) = &self.directed {
            link.serve_members(Arc::new(directed.clone()));
            let members = Arc::downgrade(&link);
            directed.hear(Arc::new(move |_, heard| {
                if let Some(link) = members.upgrade() {
                    link.heard_from_member(&heard.message);
                }
            }));
        }
        *self.link.lock_recover() = Some(link);
    }

    /// Where this device's channels hear its workspaces' governance and tell which devices each
    /// admits: the plane's side of them, `None` when this session runs no channel service.
    pub fn link(&self) -> Option<Arc<channels::ChannelLink>> {
        self.link.lock_recover().clone()
    }

    /// Where this session's channel service installs this device's keys: the broker, the room host
    /// and every sealed read meet here.
    /// This device's own writes its replica has not taken in yet.
    pub fn held_writes(&self) -> &HeldWrites {
        &self.held
    }

    /// Read `workspace`'s record of `route` with `keys` from now on: the record's host handed them
    /// to this device, which takes no part in the record's channel.
    pub fn hand_keys(
        &self,
        workspace: WorkspaceId,
        route: collab_types::ChannelRoute,
        keys: channel_crypto::ChannelContentKeys,
    ) {
        self.handed.lock_recover().insert((workspace, route), keys);
    }

    pub fn keys(&self) -> ChannelKeyProvider {
        self.keys.clone()
    }

    /// This device's collaboration identity: the key its endpoint and its channels sign with, and
    /// the certificate that says whose it is.
    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    /// Where this device's own host of `workspace` is reached at `endpoint`,
    /// signed by this device: what an invitation names for the joiner, and
    /// what a workspace founded here records of its own host.
    #[cfg(target_os = "linux")]
    fn own_host_hint(
        &self,
        workspace: &str,
        endpoint: &iroh_base::EndpointAddr,
    ) -> Result<tilo_peer_state::join::TransportHint, String> {
        let workspace_id = tilo_peer_state::join::room_workspace_id(workspace)
            .map_err(|error| error.to_string())?;
        let identity = self.identity();
        let packet_relay_url = endpoint
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
            version: 1,
            mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
            room: workspace.to_owned(),
            workspace_id,
            endpoint_addr: endpoint.clone(),
            host_user_public_key: identity.device_cert.user_public_key,
            host_device_certificate: identity.device_cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&identity.signing_key()?)
        .map_err(|error| error.to_string())
    }

    /// Found `workspace` — made on this desk in `slot` , under `name` — when this device made it
    /// and nothing here holds it yet: who is in it, who may invite and what its apps record become
    /// the workspace's own, hosted here. `endpoint` is where other devices reach this one, which
    /// the workspace's record keeps as its host's address. A workspace this device may not own, or
    /// one that could not be published, stays the desk's own.
    #[cfg(target_os = "linux")]
    pub fn found(
        &self,
        slot: WorkspaceSlot,
        workspace: WorkspaceId,
        name: &str,
        endpoint: Option<&iroh_base::EndpointAddr>,
    ) {
        if self.holds(workspace) || self.unfounded.lock_recover().contains(&workspace) {
            return;
        }
        let identity = self.identity();
        if !identity.device_cert.capabilities.can_own_workspaces {
            return;
        }
        // A session with nowhere other devices reach this one founds the workspace all the same:
        // its records and its governance are held here, and its own host's address is named in the
        // record once this device has one ([`Self::name_own_hosts`]).
        let host_hint = match endpoint {
            Some(endpoint) => match self.own_host_hint(&workspace.to_string(), endpoint) {
                Ok(hint) => Some(hint),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        %workspace,
                        "this device's own host of the workspace could not be named"
                    );
                    return;
                }
            },
            None => None,
        };
        let founded = self.owner.create_workspace(
            workspace,
            name,
            &identity.device_cert,
            identity.signer.as_ref(),
            host_hint.as_ref(),
            tilo_clock::wall(),
        );
        match collab_types::signer::at_once(founded).unwrap_or_else(|error| Err(error.to_string()))
        {
            Ok(_) => self.bind(slot, workspace),
            Err(error) => {
                tracing::warn!(%error, %workspace, "the workspace was not founded here");
                self.unfounded.lock_recover().insert(workspace);
            }
        }
    }

    /// Name where this device's own host of each workspace it hosts is reached at `endpoint`:
    /// where members and anyone a link reaches dial it. A record that says nothing gets the
    /// address, and one naming another relay gets it again ([`NativeWorkspaceOwner::name_host`]),
    /// so what a workspace says keeps up with the relays this device answers through.
    #[cfg(target_os = "linux")]
    pub fn name_own_hosts(&self, endpoint: &iroh_base::EndpointAddr) {
        for workspace in self.owner.hosted() {
            let hint = match self.own_host_hint(&workspace.to_string(), endpoint) {
                Ok(hint) => hint,
                Err(error) => {
                    tracing::warn!(%error, %workspace, "this device's own host could not be named");
                    continue;
                }
            };
            match self.owner.name_host(workspace, hint) {
                Ok(true) => {
                    tracing::info!(%workspace, "the workspace's own host is named in its record");
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, %workspace, "the workspace's own host was not named");
                }
            }
        }
    }

    /// Whether this device holds a link into a workspace that still admits someone: a person who
    /// redeems it dials this device, so its relays stay up until the link is taken back or lapses.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn invitations_open(&self, now: SystemTime) -> bool {
        self.workspaces().into_iter().any(|workspace| {
            self.owner
                .pending_invitations(workspace, now)
                .is_ok_and(|pending| !pending.is_empty())
        })
    }

    /// Name the workspace a join is installing into by the workspace id it holds: an enrollment
    /// only installs into the workspace made for it, and says so before the host is asked.
    pub fn bind(&self, workspace: WorkspaceSlot, workspace_id: WorkspaceId) {
        self.slots.0.lock_recover().insert(workspace, workspace_id);
    }

    /// Let go of a workspace a join did not install into, so this device keeps nothing it does not
    /// hold.
    pub fn unbind(&self, workspace: WorkspaceSlot) {
        self.slots.0.lock_recover().remove(&workspace);
    }
}

/// What the broker's owner commits ask of this device's workspaces: the engine's owner is the
/// canonical copy of every workspace it holds, and what it commits leaves on the plane's session
/// with the workspace's host.
impl tilo_broker_core::custody::OwnerCustody for Engine {
    /// A held workspace whose host takes part in no channels: its records are documents the owner
    /// commits, never sealed channels.
    fn commits_documents(&self, workspace_id: WorkspaceId) -> bool {
        self.holds(workspace_id)
            && self
                .owner
                .with_current(workspace_id, |view| {
                    Ok(!view
                        .workspace_host()
                        .capabilities
                        .takes_part_in_room_channels())
                })
                .unwrap_or(false)
    }

    fn projects_room(&self, workspace_id: WorkspaceId) -> bool {
        self.holds(workspace_id)
    }

    /// The engine opened its owner before the session started.
    fn opened_owner(&self, _unavailable: &str) -> Result<NativeWorkspaceOwner, String> {
        Ok(self.owner.clone())
    }

    fn resolve_owner(&self) -> Result<NativeWorkspaceOwner, String> {
        Ok(self.owner.clone())
    }

    fn check_custody(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace_id: WorkspaceId,
    ) -> Result<(), String> {
        self.custody.check(owner, workspace_id, &self.identity)
    }

    fn signer(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace_id: WorkspaceId,
    ) -> Result<Arc<dyn collab_types::signer::DeviceSigner>, String> {
        self.check_custody(owner, workspace_id)?;
        Ok(Arc::clone(&self.identity.signer))
    }

    /// Send the owner's outgoing custody of `workspace` to its peers soon: what was just committed
    /// leaves this device. Queued, never awaited.
    fn schedule_publication(&self, workspace_id: WorkspaceId) {
        if let Some(link) = self.link() {
            link.publish_custody(workspace_id);
        }
    }
}

/// The id a listing reports for an invitation, as its 32 bytes: 64 hex characters.
fn invitation_id(text: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(text).map_err(|error| error.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| "an invitation's id is 64 hex characters".to_owned())
}

/// The id cancelling `invite_id` asks under: the link is what is cancelled, so asking again about
/// the same link is the same request.
fn cancellation_id(invite_id: [u8; 32]) -> [u8; 32] {
    *blake3::Hasher::new_derive_key("Tilo invitation cancellation v1")
        .update(&invite_id)
        .finalize()
        .as_bytes()
}

/// A workspace's revocations, as the threads beside the session's loop ask for them: the broker
/// decides on its own thread and the automation host arms shortcuts on the loop, and both refuse a
/// device the workspace revoked.
pub type Revocations = Arc<dyn RevocationsOf>;

/// What a workspace of this device's engine revokes, asked from a thread of the session's own or
/// from the session's loop: `Send + Sync` where the host has threads.
pub trait RevocationsOf: collab_types::signer::MaybeSendSync {
    fn of(&self, workspace: &str) -> Option<RevocationList>;
}

/// What a workspace of this device's engine revokes, for the callers that ask from another thread.
/// A session that holds no workspace knows of none.
#[cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::arc_with_non_send_sync,
        reason = "a page has one thread, and the engine it shares is `MaybeSendSync`, never `Send` there"
    )
)]
pub fn revocations_of(engine: Option<&Arc<Engine>>) -> Revocations {
    struct Held(Option<Arc<Engine>>);

    impl RevocationsOf for Held {
        fn of(&self, workspace: &str) -> Option<RevocationList> {
            self.0
                .as_ref()
                .and_then(|engine| engine.revocations(workspace))
        }
    }

    Arc::new(Held(engine.cloned()))
}

/// Where this device's workspaces are kept under `state` .
fn record_path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// Where this device keeps the frames of the workspaces it holds, for the devices of theirs that
/// catch up from it: beside the record that holds the workspaces themselves.
fn frames_path(state: &Path) -> PathBuf {
    state.join("tilo").join("frames")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    tilo_automerge_isolate::test_worker!();

    /// A record opened under keys a rotation replaced, or under keys handed over that changed, is
    /// opened whole again under the keys held now; the same keys read on what was kept.
    #[test]
    fn a_record_kept_under_keys_a_rotation_replaced_is_forgotten() {
        let channel = collab_types::ChannelId(uuid::Uuid::from_u128(7));
        let held: super::KeysUsed = [None, Some((channel, 1))];
        let rotated: super::KeysUsed = [None, Some((channel, 2))];
        let handed: super::KeysUsed = [Some((channel, 1)), Some((channel, 1))];
        // Whatever a read kept stands in for the opened record.
        let opened = || (held, Some("opened"));
        assert_eq!(super::kept_under(Some(opened()), &held), Some("opened"));
        assert_eq!(super::kept_under(Some(opened()), &rotated), None);
        assert_eq!(super::kept_under(Some(opened()), &handed), None);
        assert_eq!(super::kept_under::<Option<&str>>(None, &held), None);
    }

    use super::*;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    /// A device of a person, certified for their root, with `capabilities`,
    /// and the person's root key.
    fn identity(capabilities: DeviceCapabilities) -> (HostIdentity, [u8; 32]) {
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&tilo_test_env::node_key());
        let root = person.verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key: root,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".to_owned(),
            capabilities,
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        let identity = HostIdentity::kept(cert, device);
        (identity, root)
    }

    /// A host whose own proof of custody this device's engine asks: these tests read what it asks
    /// with, not the proof a desktop makes over the plane.
    struct Kept;

    impl Custody for Kept {
        fn check(
            &self,
            _owner: &NativeWorkspaceOwner,
            _workspace: WorkspaceId,
            _identity: &HostIdentity,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    /// This device's engine, kept in `state` .
    fn engine(state: &Path, identity: HostIdentity) -> Engine {
        Engine::open(identity, Some(state), Vec::new(), Arc::new(Kept)).expect("an engine")
    }

    /// Where a desk's own host is reached: its device's endpoint.
    fn endpoint(identity: &HostIdentity) -> iroh_base::EndpointAddr {
        iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&identity.device_cert.device_public_key)
                .expect("a device endpoint"),
        )
    }

    #[test]
    fn a_desk_workspace_is_founded_under_the_identity_the_desk_made() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, person) = identity(DeviceCapabilities::member());
        let at = endpoint(&identity);
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        let slot = WorkspaceSlot(7);

        engine.found(slot, workspace, "Desk", Some(&at));
        // Again, as every pass over the desk does: one workspace, still the one the desk made.
        engine.found(slot, workspace, "Desk", Some(&at));

        assert!(
            engine.holds(workspace),
            "a workspace made on the desk is held here"
        );
        assert_eq!(engine.workspaces(), vec![workspace]);
        assert_eq!(
            engine.owner_of(&workspace.to_string()),
            Ok(hex::encode(person)),
            "the workspace is the person's at this device"
        );
        assert!(
            engine.may_invite(&workspace.to_string()),
            "its owner may invite into it"
        );
    }

    #[test]
    fn a_workspace_this_device_hosts_names_its_own_host_and_commits_here() {
        use tilo_broker_core::custody::OwnerCustody as _;

        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let at = endpoint(&identity);
        let host = identity.device_cert.device_public_key;
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(7), workspace, "Desk", Some(&at));

        let hint = engine
            .owner
            .with_current(workspace, |view| view.transport_hint().cloned())
            .expect("a workspace this device founded names where its host is reached");
        assert_eq!(
            hint.host.device_public_key, host,
            "the host it names is this device, whose host is in this process"
        );
        engine
            .check_custody(&engine.owner, workspace)
            .expect("its own member acts there, its host being in this process");
    }

    /// What this device tells its siblings of where its workspaces are hosted reads back as the
    /// hint itself: a sibling decodes a `TransportHint` from each, and enters the workspace at the
    /// host it names.
    #[test]
    fn the_host_hints_a_device_tells_decode_as_hints() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let at = endpoint(&identity);
        let host = identity.device_cert.device_public_key;
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(7), workspace, "Desk", Some(&at));

        let hints = engine.host_hints();
        assert_eq!(hints.len(), 1, "one hint for the one workspace held");
        let (named, bytes) = &hints[0];
        assert_eq!(named, &workspace.to_string());
        let hint = postcard::from_bytes::<tilo_peer_state::join::TransportHint>(bytes)
            .expect("the hint decodes as the device that reads it decodes it");
        assert_eq!(hint.host.device_public_key, host);
    }

    #[test]
    fn a_workspace_this_device_hosts_without_an_address_commits_here() {
        use tilo_broker_core::custody::OwnerCustody as _;

        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(7), workspace, "Desk", None);

        assert!(
            engine.holds(workspace),
            "a session with nowhere other devices reach it founds its desk's workspace"
        );
        assert!(
            engine
                .owner
                .with_current(workspace, |view| Ok(view.transport_hint().is_err()))
                .expect("the workspace is held here"),
            "its record names no host to reach until this device has one"
        );
        engine
            .check_custody(&engine.owner, workspace)
            .expect("its own member acts there, its host being in this process");
    }

    #[test]
    fn a_workspace_made_here_and_held_nowhere_else_may_be_deleted() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let at = endpoint(&identity);
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(7), workspace, "Desk", Some(&at));

        assert!(
            engine.may_delete(workspace),
            "its owner, in it alone, on its only device, may delete it"
        );
        assert_eq!(
            engine.delete(workspace),
            Ok(true),
            "the record it was held in is taken out"
        );
        assert!(!engine.holds(workspace));
        assert!(!engine.may_delete(workspace));
        assert_eq!(
            engine.delete(workspace),
            Ok(false),
            "a record holding nothing of it has nothing to take out"
        );
    }

    #[test]
    fn a_workspace_this_device_holds_no_record_of_may_not_be_deleted() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let engine = engine(state.path(), identity);

        assert!(!engine.may_delete(WorkspaceId::new()));
    }

    #[test]
    fn a_device_that_may_not_own_workspaces_founds_none() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let mut identity = identity;
        identity.device_cert.capabilities.can_own_workspaces = false;
        let at = endpoint(&identity);
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();

        engine.found(WorkspaceSlot(7), workspace, "Desk", Some(&at));

        assert!(!engine.holds(workspace));
        assert!(!engine.may_invite(&workspace.to_string()));
    }
}
