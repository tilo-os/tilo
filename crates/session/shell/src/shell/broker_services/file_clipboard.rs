//! Local, descriptor-bound Files offers. No offer is a sharing grant.
//!
//! # Pasting: group commit
//!
//! A paste copies (Copy) or moves (Cut) the offer's entries into the
//! receiving folder. Every step that matters after a crash is in the
//! operation's durable receipt ([`journal`]) before the step is taken, and
//! recovery rolls an interrupted paste back from that receipt. The receipt
//! is written per *batch* of entries ([`BATCH_ITEMS`], [`BATCH_BYTES`]), not
//! per entry, so its cost does not grow with the paste's entry count: a
//! paste of many small files used to pay four durable writes per file, one
//! after another.
//!
//! One batch goes through five phases:
//!
//! 1. **Stage.** Each entry is copied into the batch's private staging
//!    folder in the receiving folder (`.tilo-transfer-<id>`, one leaf per
//!    entry), its live admission checked as it is read. Nothing is synced
//!    and nothing is journaled between entries. A staged copy is hidden and
//!    never presented as a pasted object.
//! 2. **Sync.** The staged copies are synced in parallel, one `fsync` per
//!    file on a few threads so the filesystem commits them together, then
//!    the staging folder and the receiving folder.
//! 3. **Intent.** One journal entry names the whole batch: for every entry,
//!    its staged copy and that copy's identity, and for a Cut the hidden
//!    name its source will be moved to before it is removed. The entry is
//!    durable before any source or destination is touched.
//! 4. **Publish.** Entry by entry, its admission is checked again; a Cut's
//!    source is moved to its recorded hidden name and removed; a Replace
//!    sends the clashing item to Trash; the staged copy is renamed into
//!    place under its own name (or the first free numbered one). Nothing is
//!    journaled between entries.
//! 5. **Completion.** The receiving folder (and a Cut's source folders) are
//!    synced, then one journal entry records where each entry was
//!    published, and drops the staged record of an entry that failed before
//!    publication and whose copy is gone.
//!
//! Recovery replays per batch. A batch whose completion is missing is
//! checked entry by entry against its intent: a staged copy is found at its
//! staged path or, once published, by its identity in the receiving
//! folder, and is moved back to a Cut's source name or to Trash; a hidden
//! Cut source is put back. An entry staged but not yet in an intent is a
//! hidden copy whose source was never touched.
//!
//! The guarantees are the per-entry protocol's: a half-written file is never
//! presented as complete, because only synced copies named in a durable
//! intent are renamed into place; and a moved source is never lost, because
//! it is removed only after its synced copy is named in a durable intent,
//! so recovery always holds either the source or the copy.
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use super::super::nearby_transfer::{SecureNearbyDestination, SecureNearbySource};
use super::super::state::{Capability, FsMode, WorkspaceTreeHandle};
use runtime_collab::file_clipboard::{
    Command, Conflict, Offer as Offered, Reply, SelectedDescriptor,
};
use tilo_action_broker::services::{FileClipboardContext, FileClipboardServices};
use tilo_actions::ActionKind as Permission;
use tilo_lock::MutexRecover as _;

const OFFER_LIFETIME: Duration = Duration::from_secs(10 * 60);
const MAX_OPERATIONS: usize = 256;
const MAX_WORKERS: usize = 4;
const MAX_RETAINED_DESCRIPTORS: usize = 4096;
/// The most entries one batch of a paste stages before it is made durable.
const BATCH_ITEMS: usize = 64;
/// The most bytes one batch stages before it is made durable, so large
/// files are published as they arrive rather than a batch at a time.
const BATCH_BYTES: u64 = 256 * 1024 * 1024;
/// Threads that sync one batch's staged copies.
const SYNC_THREADS: usize = 8;
/// The most clashing names a [`Reply::Conflicts`] lists (it also counts).
const MAX_CONFLICT_NAMES: usize = 8;

/// The names of `offer`'s entries still to paste that `destination` already
/// has, in offer order.
fn clashing_names(offer: &Offer, destination: &Destination) -> Result<Vec<String>, String> {
    let consumed = offer
        .consumed
        .lock()
        .map_err(|_| "clipboard consumption receipt unavailable")?
        .clone();
    Ok(offer
        .entries
        .iter()
        .zip(consumed)
        .filter(|(source, consumed)| !consumed && destination.directory.entry(&source.name).is_ok())
        .map(|(source, _)| source.name.to_string_lossy().into_owned())
        .collect())
}

mod journal;
mod restore;
#[cfg(test)]
pub(crate) mod test_support;
mod trash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

struct Grant {
    capability: Capability,
    root: PathBuf,
    entry: File,
    identity: Identity,
}
impl Grant {
    fn open(
        tree: &WorkspaceTreeHandle,
        workspace: &str,
        path: &Path,
        write: bool,
        owner: &str,
    ) -> Result<Self, String> {
        super::nearby_transfer::files_grants(tree, workspace)
            .into_iter()
            .find_map(|capability| {
                let Capability::Filesystem {
                    path: root, mode, ..
                } = &capability
                else {
                    return None;
                };
                if !path.is_absolute()
                    || !path.starts_with(root)
                    || (write && *mode != FsMode::ReadWrite)
                {
                    return None;
                }
                if !current_files_grant(tree, workspace, &capability, owner) {
                    return None;
                }
                let opened = SecureNearbySource::open_beneath(root, root).ok()?;
                let identity = Identity::of(&opened.metadata().ok()?);
                let root = root.clone();
                Some(Self {
                    capability,
                    root,
                    entry: opened.into_file(),
                    identity,
                })
            })
            .ok_or_else(|| "path is outside this Files window's live grants".into())
    }
    fn root(&self) -> &Path {
        &self.root
    }
    fn check(
        &self,
        tree: &WorkspaceTreeHandle,
        workspace: &str,
        owner: &str,
    ) -> Result<(), String> {
        if Identity::of(&self.entry.metadata().map_err(|error| error.to_string())?) != self.identity
        {
            return Err("retained Files grant identity changed".into());
        }
        if !current_files_grant(tree, workspace, &self.capability, owner)
            || !super::nearby_transfer::files_grants(tree, workspace).contains(&self.capability)
        {
            return Err("Files access was revoked".into());
        }
        let opened = SecureNearbySource::open_beneath(self.root(), self.root())?;
        if Identity::of(&opened.metadata()?) != self.identity {
            return Err("Files grant identity changed".into());
        }
        Ok(())
    }
}

fn current_files_grant(
    tree: &WorkspaceTreeHandle,
    room: &str,
    capability: &Capability,
    actor: &str,
) -> bool {
    let tree = tree.read();
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter()
        .find(|ws| ws.room.to_string() == room)
    else {
        return false;
    };
    let scopes = capability.requested_scopes();
    let current = workspace
        .app_capabilities_at(
            super::nearby_transfer::FILES_APP_ID,
            tilo_clock::unix_now_ns() / 1_000_000_000,
        )
        .into_iter()
        .any(|row| {
            scopes.contains(&(row.scope.clone(), row.access))
                && tree
                    .native_runtime
                    .files_home_consent_current(workspace, row, actor)
        });
    drop(tree);
    current
}

/// How a captured entry is held. A single selected object keeps its own
/// descriptor; the entries of a folder captured by name share the folder's
/// descriptor and are pinned by a seal (identity, birth or change time), so
/// thousands of them cost the host no descriptor each.
enum Pin {
    Held(Arc<File>),
    Sealed(journal::Seal),
}

struct Source {
    grant: Arc<Grant>,
    path: PathBuf,
    parent: Arc<SecureNearbyDestination>,
    pin: Pin,
    identity: Identity,
    name: OsString,
    link: Option<PathBuf>,
}
impl Source {
    fn open(
        tree: &WorkspaceTreeHandle,
        workspace: &str,
        path: PathBuf,
        cut: bool,
        owner: &str,
    ) -> Result<Self, String> {
        let grant = Grant::open(tree, workspace, &path, cut, owner)?;
        let parent_path = path.parent().ok_or("source has no parent")?;
        let parent = if parent_path.starts_with(grant.root()) {
            SecureNearbyDestination::open_beneath(grant.root(), parent_path)?
        } else {
            if cut {
                return Err("Cut requires an existing writable source-directory grant".into());
            }
            SecureNearbyDestination::open_grant(parent_path)?
        };
        let name = path.file_name().ok_or("source has no name")?.to_os_string();
        let entry = parent.entry(&name)?;
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() && !metadata.is_dir() && !metadata.file_type().is_symlink() {
            return Err("source is not a file, folder or symbolic link".into());
        }
        let identity = Identity::of(&metadata);
        if !parent_path.starts_with(grant.root()) && identity != grant.identity {
            return Err("source differs from its exact file grant".into());
        }
        let link = if metadata.file_type().is_symlink() {
            Some(read_link(&entry)?)
        } else {
            None
        };
        Ok(Self {
            grant: Arc::new(grant),
            path,
            parent: Arc::new(parent),
            pin: Pin::Held(Arc::new(entry)),
            identity,
            name,
            link,
        })
    }

    /// The entry `name` of a folder already held (its grant and descriptor
    /// shared with the folder's other captured entries), pinned by its seal.
    fn open_named(
        grant: &Arc<Grant>,
        parent: &Arc<SecureNearbyDestination>,
        folder: &Path,
        name: OsString,
    ) -> Result<Self, String> {
        let entry = parent.entry(&name)?;
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() && !metadata.is_dir() && !metadata.file_type().is_symlink() {
            return Err("source is not a file, folder or symbolic link".into());
        }
        let link = if metadata.file_type().is_symlink() {
            Some(read_link(&entry)?)
        } else {
            None
        };
        Ok(Self {
            grant: grant.clone(),
            path: folder.join(&name),
            parent: parent.clone(),
            pin: Pin::Sealed(journal::Seal::of(&metadata)),
            identity: Identity::of(&metadata),
            name,
            link,
        })
    }

    /// The captured entry itself: its held descriptor, or the folder's entry
    /// of that name when it is still the very object that was captured.
    fn entry(&self) -> Result<Arc<File>, String> {
        match &self.pin {
            Pin::Held(entry) => Ok(entry.clone()),
            Pin::Sealed(seal) => {
                let entry = self.parent.entry(&self.name)?;
                let metadata = entry.metadata().map_err(|error| error.to_string())?;
                if !seal.matches(&metadata) || Identity::of(&metadata) != self.identity {
                    return Err("selected source was replaced or moved".into());
                }
                Ok(Arc::new(entry))
            }
        }
    }

    /// The descriptors this entry keeps open by itself (shared folder and
    /// grant descriptors are counted once per folder).
    fn own_descriptors(&self) -> usize {
        usize::from(matches!(self.pin, Pin::Held(_)))
    }

    fn anchored(&self) -> PathBuf {
        self.parent.anchored_root().join(&self.name)
    }
    fn check_parent(
        &self,
        tree: &WorkspaceTreeHandle,
        workspace: &str,
        owner: &str,
    ) -> Result<(), String> {
        self.grant.check(tree, workspace, owner)?;
        let parent = self.path.parent().ok_or("source parent unavailable")?;
        let current = if parent.starts_with(self.grant.root()) {
            SecureNearbyDestination::open_beneath(self.grant.root(), parent)?.metadata()?
        } else {
            fs::metadata(parent).map_err(|error| error.to_string())?
        };
        if Identity::of(&current) != Identity::of(&self.parent.metadata()?) {
            return Err("original source folder was replaced or moved".into());
        }
        Ok(())
    }

    fn check(
        &self,
        tree: &WorkspaceTreeHandle,
        workspace: &str,
        owner: &str,
    ) -> Result<(), String> {
        self.check_parent(tree, workspace, owner)?;
        if Identity::of(
            &self
                .entry()?
                .metadata()
                .map_err(|error| error.to_string())?,
        ) != self.identity
        {
            return Err("retained source identity changed".into());
        }
        let current = fs::symlink_metadata(&self.path).map_err(|error| error.to_string())?;
        if Identity::of(&current) != self.identity {
            return Err("selected source was replaced or moved".into());
        }
        let current = self
            .parent
            .entry(&self.name)?
            .metadata()
            .map_err(|error| error.to_string())?;
        if Identity::of(&current) != self.identity {
            return Err("selected source identity changed".into());
        }
        Ok(())
    }
}

fn verify_selected(
    context: &FileClipboardContext,
    selected: &SelectedDescriptor,
    authorized: Identity,
) -> Result<(), String> {
    let expected = Identity {
        device: selected.expected.device,
        inode: selected.expected.inode,
    };
    let retained = context.duplicate_selected(selected.descriptor)?;
    let actual = Identity::of(&retained.metadata().map_err(|error| error.to_string())?);
    if actual != expected || actual != authorized {
        return Err("shown object differs from its retained selection".into());
    }
    Ok(())
}

fn read_link(entry: &File) -> Result<PathBuf, String> {
    let mut bytes = vec![0_u8; 4096];
    // SAFETY: owned O_PATH|O_NOFOLLOW symlink descriptor, empty NUL-terminated
    // name and writable buffer. readlinkat reads this exact leaf, not its target.
    let length = unsafe {
        libc::readlinkat(
            entry.as_raw_fd(),
            c"".as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    let length = usize::try_from(length).map_err(|_| io::Error::last_os_error().to_string())?;
    if length == bytes.len() {
        return Err("symbolic link exceeds bound".into());
    }
    bytes.truncate(length);
    Ok(OsString::from_vec(bytes).into())
}

struct Destination {
    grant: Grant,
    path: PathBuf,
    directory: Arc<SecureNearbyDestination>,
    identity: Identity,
}
impl Destination {
    fn open(
        tree: &WorkspaceTreeHandle,
        workspace: &str,
        path: PathBuf,
        owner: &str,
    ) -> Result<Self, String> {
        let grant = Grant::open(tree, workspace, &path, true, owner)?;
        let directory = SecureNearbyDestination::open_beneath(grant.root(), &path)?;
        let identity = Identity::of(&directory.metadata()?);
        Ok(Self {
            grant,
            path,
            directory: Arc::new(directory),
            identity,
        })
    }
    fn check(
        &self,
        tree: &WorkspaceTreeHandle,
        workspace: &str,
        owner: &str,
    ) -> Result<(), String> {
        self.grant.check(tree, workspace, owner)?;
        let current = SecureNearbyDestination::open_beneath(self.grant.root(), &self.path)?;
        if Identity::of(&current.metadata()?) != self.identity {
            return Err("destination folder was replaced or moved".into());
        }
        Ok(())
    }
}

struct Offer {
    public: Offered,
    source: FileClipboardContext,
    expires: Instant,
    entries: Vec<Arc<Source>>,
    consumed: Mutex<Vec<bool>>,
    cut_busy: AtomicBool,
}
impl Offer {
    fn check(&self, now: Instant) -> Result<(), String> {
        if self.expires <= now {
            return Err("Files clipboard offer expired".into());
        }
        self.source.authorize(if self.public.cut {
            Permission::Undoable
        } else {
            Permission::Read
        })
    }

    fn check_visible_sources(&self, tree: &WorkspaceTreeHandle) -> Result<(), String> {
        let consumed = self
            .consumed
            .lock()
            .map_err(|_| "clipboard consumption receipt unavailable")?
            .clone();
        for (source, consumed) in self.entries.iter().zip(consumed) {
            if !consumed {
                source.check(tree, self.source.workspace(), self.source.owner())?;
            }
        }
        Ok(())
    }
}

/// What one pasted entry became. It is pinned by its seal, not a held
/// descriptor, so a large paste keeps no descriptor per entry for its undo.
struct Record {
    source: Arc<Source>,
    destination: PathBuf,
    seal: journal::Seal,
    identity: Identity,
    /// The item of the same name this paste replaced, in Trash, to put back
    /// when the paste is undone.
    replaced: Option<PathBuf>,
    undone: bool,
}
struct Operation {
    context: FileClipboardContext,
    offer: Arc<Offer>,
    destination: Arc<Destination>,
    destination_ticket: String,
    destination_custody: Arc<Ticket>,
    id: String,
    cancelled: AtomicBool,
    records: Mutex<Vec<Record>>,
    reply: Mutex<Option<Reply>>,
    journal: Arc<journal::Journal>,
    receipt: std::sync::OnceLock<Arc<journal::PendingReceipt>>,
    destination_name: Option<OsString>,
    restore_info: Option<restore::Info>,
    /// What to do about names the destination already has.
    conflict: Conflict,
}
enum SelectedObject {
    Source(Arc<Source>),
    /// The entries of one folder captured by name: one ticket for all.
    Sources(Vec<Arc<Source>>),
    Destination(Arc<Destination>),
}

/// One entry of a batch, staged and not yet published.
struct Staged {
    source: Arc<Source>,
    transfer: file_explorer::fileops::StagedTransfer,
    /// The staged copy, held while it is synced and published.
    entry: Arc<File>,
    identity: Identity,
    seal: journal::Seal,
    bytes: u64,
    /// The staged copy by its stable path, as the intent records it.
    stable: PathBuf,
    /// For a Cut: the hidden name its source moves to, anchored and stable.
    quarantine: Option<(PathBuf, PathBuf)>,
}

/// Writable grants and folders opened for Cut, one per source folder.
type WritableFolders = HashMap<PathBuf, (Arc<Grant>, Arc<SecureNearbyDestination>)>;

/// The descriptors `sources` keep open: each held entry, and each shared
/// grant and folder once. `seen` carries what was already counted.
fn source_descriptors<'a>(
    sources: impl IntoIterator<Item = &'a Arc<Source>>,
    seen: &mut HashSet<usize>,
) -> usize {
    sources
        .into_iter()
        .map(|source| {
            let grant = usize::from(seen.insert(Arc::as_ptr(&source.grant) as usize));
            let parent = usize::from(seen.insert(Arc::as_ptr(&source.parent) as usize));
            source
                .own_descriptors()
                .saturating_add(grant)
                .saturating_add(parent)
        })
        .sum()
}

impl SelectedObject {
    fn descriptors(&self, seen: &mut HashSet<usize>) -> usize {
        match self {
            Self::Source(source) => source_descriptors([source], seen),
            Self::Sources(sources) => source_descriptors(sources, seen),
            Self::Destination(_) => 2,
        }
    }
}
struct Ticket {
    context: FileClipboardContext,
    expires: Instant,
    object: SelectedObject,
}
impl Ticket {
    fn check(&self, context: &FileClipboardContext, now: Instant) -> Result<(), String> {
        if self.context.peer() != context.peer()
            || self.context.window() != context.window()
            || self.context.workspace() != context.workspace()
        {
            return Err("selected object belongs to another Files window".into());
        }
        if self.expires <= now {
            return Err("selected object ticket expired".into());
        }
        self.context.authorize(Permission::Read)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompensationKind {
    Undo,
    Redo,
    Recover,
}

struct Compensation {
    context: FileClipboardContext,
    kind: CompensationKind,
    operation: String,
    answer: Mutex<Option<Result<Reply, String>>>,
}
struct CompensationInput {
    state: Arc<Mutex<State>>,
    tree: WorkspaceTreeHandle,
    journal: Arc<journal::Journal>,
    retained: Option<Arc<Operation>>,
    pending: Option<Arc<journal::PendingReceipt>>,
}
impl Compensation {
    fn run(&self, input: CompensationInput) {
        let permit = WorkerPermit {
            state: input.state.clone(),
            offer: None,
        };
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.context.authorize(Permission::Undoable)?;
            match self.kind {
                CompensationKind::Recover => input.journal.recover(&input.tree, &self.context),
                CompensationKind::Undo => match input.retained {
                    Some(operation) => operation.undo(&input.tree, &self.context),
                    None => input
                        .pending
                        .as_ref()
                        .ok_or("clipboard receipt unavailable")?
                        .undo(&input.tree, &self.context),
                },
                CompensationKind::Redo => input
                    .pending
                    .as_ref()
                    .ok_or("clipboard receipt unavailable")?
                    .redo(&input.tree, &self.context),
            }
        }))
        .unwrap_or_else(|_| {
            Err("clipboard compensation interrupted; recover its durable receipt".into())
        });
        if self.kind == CompensationKind::Redo
            && matches!(
                &answer,
                Ok(Reply::Settled {
                    error: None,
                    undone: false,
                    ..
                })
            )
            && let Ok(mut state) = input.state.lock()
        {
            state.operations.retain(|(workspace, _, id), operation| {
                !(workspace == self.context.workspace()
                    && id == &self.operation
                    && operation.context.window() == self.context.window())
            });
            drop(state);
        }
        if let Ok(Reply::Recovered { operations, .. }) = &answer
            && let Ok(mut state) = input.state.lock()
        {
            state.operations.retain(|(workspace, _, id), operation| {
                !(workspace == self.context.workspace()
                    && operation.context.window() == self.context.window()
                    && operations.contains(id))
            });
            drop(state);
        }
        drop(permit);
        *self.answer.lock_recover() = Some(answer);
    }
    fn answer(&self) -> Result<Reply, String> {
        self.answer
            .lock()
            .map_err(|_| "clipboard compensation unavailable")?
            .clone()
            .unwrap_or_else(|| {
                Ok(Reply::Running {
                    operation: self.operation.clone(),
                })
            })
    }
}

#[derive(Default)]
struct State {
    tickets: HashMap<String, Arc<Ticket>>,
    offers: HashMap<String, Arc<Offer>>,
    operations: HashMap<(String, String, String), Arc<Operation>>,
    workers: usize,
    compensations: HashMap<(String, String, String), Arc<Compensation>>,
}

impl State {
    fn check_paste_capacity(&self, operation: &Operation) -> Result<(), String> {
        if self
            .compensations
            .values()
            .any(|job| matches!(job.answer(), Ok(Reply::Running { .. })))
        {
            return Err("wait for clipboard compensation before pasting".into());
        }
        if self.workers >= MAX_WORKERS
            || self.operations.len() >= MAX_OPERATIONS
            || self
                .reserved_descriptors()
                .saturating_add(if operation.restore_info.is_some() {
                    3
                } else {
                    0
                })
                .saturating_add(3)
                > MAX_RETAINED_DESCRIPTORS
        {
            return Err("clipboard transfer capacity reached".into());
        }
        Ok(())
    }
    fn prune_offers(&mut self, now: Instant) {
        self.offers.retain(|_, offer| offer.expires > now);
        self.tickets.retain(|_, ticket| ticket.expires > now);
    }
    fn reserved_descriptors(&self) -> usize {
        let mut offers = HashSet::new();
        let mut seen = HashSet::new();
        let mut total = self
            .tickets
            .values()
            .map(|ticket| ticket.object.descriptors(&mut seen))
            .sum::<usize>();
        for offer in self
            .offers
            .values()
            .chain(self.operations.values().map(|op| &op.offer))
        {
            if offers.insert(&offer.public.id) {
                total = total.saturating_add(source_descriptors(&offer.entries, &mut seen));
            }
        }
        // A running operation holds its destination and the entry it is on;
        // what it pasted is pinned by seals.
        total.saturating_add(self.operations.len().saturating_mul(3))
    }
}

struct WorkerPermit {
    state: Arc<Mutex<State>>,
    offer: Option<Arc<Offer>>,
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        if let Some(offer) = &self.offer {
            offer.cut_busy.store(false, Ordering::Release);
        }
        if let Ok(mut state) = self.state.lock() {
            state.workers = state.workers.saturating_sub(1);
        }
    }
}

pub(super) struct NativeFileClipboard {
    tree: WorkspaceTreeHandle,
    journal: Arc<journal::Journal>,
    state: Arc<Mutex<State>>,
}
impl NativeFileClipboard {
    pub(super) fn new(tree: WorkspaceTreeHandle) -> Self {
        Self {
            journal: Arc::new(journal::Journal::new(&tree)),
            tree,
            state: Arc::default(),
        }
    }
    fn capture(
        &self,
        context: &FileClipboardContext,
        sources: Vec<SelectedDescriptor>,
        destination: Option<SelectedDescriptor>,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Read)?;
        let now = Instant::now();
        let mut captured = Vec::with_capacity(
            sources
                .len()
                .saturating_add(usize::from(destination.is_some())),
        );
        let mut ids = Vec::with_capacity(sources.len());
        for selected in sources {
            let source = Arc::new(Source::open(
                &self.tree,
                context.workspace(),
                PathBuf::from(OsString::from_vec(selected.path.clone())),
                false,
                context.owner(),
            )?);
            verify_selected(context, &selected, source.identity)?;
            source.check(&self.tree, context.workspace(), context.owner())?;
            let id = uuid::Uuid::new_v4().to_string();
            captured.push((id.clone(), SelectedObject::Source(source)));
            ids.push(id);
        }
        let destination = destination
            .map(|selected| {
                let destination = Arc::new(Destination::open(
                    &self.tree,
                    context.workspace(),
                    PathBuf::from(OsString::from_vec(selected.path.clone())),
                    context.owner(),
                )?);
                verify_selected(context, &selected, destination.identity)?;
                destination.check(&self.tree, context.workspace(), context.owner())?;
                let id = uuid::Uuid::new_v4().to_string();
                captured.push((id.clone(), SelectedObject::Destination(destination)));
                Ok::<_, String>(id)
            })
            .transpose()?;
        context.authorize(Permission::Read)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        state.prune_offers(now);
        let mut seen = HashSet::new();
        let descriptors = captured
            .iter()
            .map(|(_, object)| object.descriptors(&mut seen))
            .sum::<usize>();
        if state.reserved_descriptors().saturating_add(descriptors) > MAX_RETAINED_DESCRIPTORS {
            return Err("clipboard descriptor capacity reached".into());
        }
        for (id, object) in captured {
            state.tickets.insert(
                id,
                Arc::new(Ticket {
                    context: context.clone(),
                    expires: now.checked_add(OFFER_LIFETIME).unwrap_or(now),
                    object,
                }),
            );
        }
        drop(state);
        Ok(Reply::Captured {
            sources: ids,
            destination,
        })
    }
    /// Capture the entries `names` of the shown folder as one ticket. The
    /// folder travels as one descriptor and is checked against the folder
    /// the window shows; each entry is pinned by its seal, so the host keeps
    /// one descriptor for the folder however many entries there are.
    fn capture_named(
        &self,
        context: &FileClipboardContext,
        folder: &SelectedDescriptor,
        entries: Vec<runtime_collab::file_clipboard::NamedEntry>,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Read)?;
        let path = PathBuf::from(OsString::from_vec(folder.path.clone()));
        let grant = Arc::new(Grant::open(
            &self.tree,
            context.workspace(),
            &path,
            false,
            context.owner(),
        )?);
        let parent = Arc::new(SecureNearbyDestination::open_beneath(grant.root(), &path)?);
        verify_selected(context, folder, Identity::of(&parent.metadata()?))?;
        let mut sources = Vec::with_capacity(entries.len());
        let mut unique = HashSet::with_capacity(entries.len());
        for entry in entries {
            if !unique.insert(entry.name.clone()) {
                return Err("clipboard selection contains a duplicate entry".into());
            }
            let source =
                Source::open_named(&grant, &parent, &path, OsString::from_vec(entry.name))?;
            // The object under the name must still be the one the window
            // showed: a name is never consent to copy its replacement.
            if source.identity
                != (Identity {
                    device: entry.expected.device,
                    inode: entry.expected.inode,
                })
            {
                return Err("shown object differs from its retained selection".into());
            }
            sources.push(Arc::new(source));
        }
        let count = sources.len();
        let object = SelectedObject::Sources(sources);
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        state.prune_offers(now);
        let mut seen = HashSet::new();
        if state
            .reserved_descriptors()
            .saturating_add(object.descriptors(&mut seen))
            > MAX_RETAINED_DESCRIPTORS
        {
            return Err("clipboard descriptor capacity reached".into());
        }
        let ticket = uuid::Uuid::new_v4().to_string();
        state.tickets.insert(
            ticket.clone(),
            Arc::new(Ticket {
                context: context.clone(),
                expires: now.checked_add(OFFER_LIFETIME).unwrap_or(now),
                object,
            }),
        );
        drop(state);
        Ok(Reply::CapturedNamed { ticket, count })
    }

    fn ticket(&self, context: &FileClipboardContext, id: &str) -> Result<Arc<Ticket>, String> {
        let ticket = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?
            .tickets
            .get(id)
            .cloned()
            .ok_or("selected object ticket unavailable")?;
        ticket.check(context, Instant::now())?;
        Ok(ticket)
    }
    fn release(&self, context: &FileClipboardContext, ids: &[String]) -> Result<Reply, String> {
        context.authorize(Permission::Read)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        // Validate the entire request before removing any ticket. Missing tickets
        // are already released; foreign-window tickets are never discarded.
        for id in ids {
            if let Some(ticket) = state.tickets.get(id)
                && (ticket.context.peer() != context.peer()
                    || ticket.context.window() != context.window()
                    || ticket.context.workspace() != context.workspace())
            {
                return Err("selected object belongs to another Files window".into());
            }
        }
        for id in ids {
            state.tickets.remove(id);
        }
        drop(state);
        Ok(Reply::Released)
    }
    fn publish(
        &self,
        context: &FileClipboardContext,
        sources: Vec<String>,
        cut: bool,
    ) -> Result<Reply, String> {
        context.authorize(if cut {
            Permission::Undoable
        } else {
            Permission::Read
        })?;
        let mut entries = Vec::with_capacity(sources.len());
        let mut paths = HashSet::new();
        // Cut needs write access to each source's folder: opened once per
        // folder and shared by the entries in it.
        let mut writable_folders = WritableFolders::new();
        for id in sources {
            let ticket = self.ticket(context, &id)?;
            let captured: Vec<Arc<Source>> = match &ticket.object {
                SelectedObject::Source(source) => vec![source.clone()],
                SelectedObject::Sources(sources) => sources.clone(),
                SelectedObject::Destination(_) => {
                    return Err("clipboard source requires a selected entry ticket".into());
                }
            };
            for source in captured {
                source.check(&self.tree, context.workspace(), context.owner())?;
                let source = if cut {
                    self.writable(context, &source, &mut writable_folders)?
                } else {
                    source
                };
                if !paths.insert(source.path.clone()) {
                    return Err("clipboard selection contains a duplicate entry".into());
                }
                entries.push(source);
            }
        }
        let public = Offered {
            id: uuid::Uuid::new_v4().to_string(),
            count: entries.len(),
            cut,
        };
        let now = Instant::now();
        let offer = Arc::new(Offer {
            public: public.clone(),
            source: context.clone(),
            expires: now.checked_add(OFFER_LIFETIME).unwrap_or(now),
            consumed: Mutex::new(vec![false; entries.len()]),
            cut_busy: AtomicBool::new(false),
            entries,
        });
        let mut state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        state.prune_offers(now);
        if state
            .reserved_descriptors()
            .saturating_add(source_descriptors(&offer.entries, &mut HashSet::new()))
            > MAX_RETAINED_DESCRIPTORS
        {
            return Err("clipboard descriptor capacity reached".into());
        }
        state.offers.insert(context.workspace().into(), offer);
        drop(state);
        Ok(Reply::Published { offer: public })
    }
    /// `source` held with write access to its folder, for Cut. Entries
    /// captured by name share one writable folder per folder.
    fn writable(
        &self,
        context: &FileClipboardContext,
        source: &Arc<Source>,
        folders: &mut WritableFolders,
    ) -> Result<Arc<Source>, String> {
        let writable = if matches!(source.pin, Pin::Sealed(_)) {
            let folder = source.path.parent().ok_or("source has no parent")?;
            let (grant, parent) = if let Some(held) = folders.get(folder) {
                held.clone()
            } else {
                let grant = Arc::new(Grant::open(
                    &self.tree,
                    context.workspace(),
                    folder,
                    true,
                    context.owner(),
                )?);
                let parent = Arc::new(SecureNearbyDestination::open_beneath(grant.root(), folder)?);
                folders.insert(folder.to_path_buf(), (grant.clone(), parent.clone()));
                (grant, parent)
            };
            Source::open_named(&grant, &parent, folder, source.name.clone())?
        } else {
            Source::open(
                &self.tree,
                context.workspace(),
                source.path.clone(),
                true,
                context.owner(),
            )?
        };
        if writable.identity != source.identity
            || Identity::of(&writable.parent.metadata()?)
                != Identity::of(&source.parent.metadata()?)
        {
            return Err("selected source was replaced or moved".into());
        }
        Ok(Arc::new(writable))
    }

    fn snapshot(&self, context: &FileClipboardContext) -> Result<Reply, String> {
        context.authorize(Permission::Read)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        state.prune_offers(Instant::now());
        let offer = state.offers.get(context.workspace()).cloned();
        drop(state);
        let public = offer
            .filter(|offer| {
                offer.check(Instant::now()).is_ok()
                    && offer.check_visible_sources(&self.tree).is_ok()
            })
            .and_then(|offer| {
                let count = offer
                    .consumed
                    .lock()
                    .ok()?
                    .iter()
                    .filter(|done| !**done)
                    .count();
                (count > 0).then(|| Offered {
                    count,
                    ..offer.public.clone()
                })
            });
        Ok(Reply::Snapshot { offer: public })
    }
    fn operation(
        &self,
        context: &FileClipboardContext,
        id: &str,
    ) -> Result<Arc<Operation>, String> {
        self.state
            .lock()
            .map_err(|_| "clipboard state unavailable")?
            .operations
            .get(&(
                context.workspace().into(),
                context.owner().into(),
                id.into(),
            ))
            .filter(|operation| operation.context.window() == context.window())
            .cloned()
            .ok_or_else(|| "clipboard receipt unavailable".into())
    }
    fn compensation_operation(
        &self,
        context: &FileClipboardContext,
        id: &str,
    ) -> Result<Arc<Operation>, String> {
        if !context.is_history_compensation() {
            return self.operation(context, id);
        }
        context.authorize(Permission::Undoable)?;
        let state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        let mut matching =
            state
                .operations
                .iter()
                .filter(|((workspace, _, operation), receipt)| {
                    workspace == context.workspace()
                        && operation == id
                        && receipt.context.window() == context.window()
                });
        let receipt = matching.next().map(|(_, receipt)| receipt.clone());
        let ambiguous = matching.next().is_some();
        drop(state);
        if ambiguous {
            return Err("clipboard compensation receipt ambiguous".into());
        }
        receipt.ok_or_else(|| "clipboard receipt unavailable".into())
    }
    fn compensation(
        &self,
        context: &FileClipboardContext,
        id: String,
        kind: CompensationKind,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Undoable)?;
        // Resolve the original receipt before admission. Saved paths confer no
        // authority; each destructive phase rechecks this invocation's grants.
        let retained = if kind == CompensationKind::Undo {
            self.compensation_operation(context, &id).ok()
        } else {
            None
        };
        let pending = if kind == CompensationKind::Recover || retained.is_some() {
            None
        } else {
            Some(self.journal.find(context, &id)?)
        };
        let key = (
            context.workspace().to_owned(),
            context.window().to_owned(),
            id.clone(),
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        if let Some(prior) = state.compensations.get(&key)
            && matches!(prior.answer(), Ok(Reply::Running { .. }))
        {
            if prior.kind != kind
                || prior.context.owner() != context.owner()
                || prior.context.peer() != context.peer()
            {
                return Err("another clipboard compensation is still running".into());
            }
            return prior.answer();
        }
        if state.workers != 0 {
            return Err("wait for running clipboard transfers before compensating".into());
        }
        state
            .compensations
            .retain(|_, job| matches!(job.answer(), Ok(Reply::Running { .. })));
        let job = Arc::new(Compensation {
            context: context.clone(),
            kind,
            operation: id.clone(),
            answer: Mutex::new(None),
        });
        state.compensations.insert(key.clone(), job.clone());
        state.workers = state.workers.saturating_add(1);
        drop(state);
        let input = CompensationInput {
            tree: self.tree.clone(),
            journal: self.journal.clone(),
            state: self.state.clone(),
            retained,
            pending,
        };
        let result = std::thread::Builder::new()
            .name("files-compensation".into())
            .spawn(move || job.run(input));
        if let Err(error) = result {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "clipboard state unavailable")?;
            state.workers = state.workers.saturating_sub(1);
            state.compensations.remove(&key);
            drop(state);
            return Err(error.to_string());
        }
        Ok(Reply::Running { operation: id })
    }

    fn compensation_answer(
        &self,
        context: &FileClipboardContext,
        id: &str,
    ) -> Result<Option<Reply>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        let job = state
            .compensations
            .get(&(
                context.workspace().into(),
                context.window().into(),
                id.into(),
            ))
            .cloned();
        drop(state);
        let Some(job) = job else { return Ok(None) };
        if job.context.peer() != context.peer() || job.context.owner() != context.owner() {
            return Err("clipboard compensation belongs to another invocation owner".into());
        }
        job.answer().map(Some)
    }

    fn paste(
        &self,
        context: &FileClipboardContext,
        offer: &str,
        destination_ticket: String,
        id: String,
        conflict: Conflict,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Undoable)?;
        let prior = self.operation(context, &id).ok();
        if let Some(prior) = prior {
            if !prior.matches_paste(context, offer, &destination_ticket) {
                return Err("operation id already names another transfer".into());
            }
            return Ok(prior.answer());
        }
        let offered = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?
            .offers
            .get(context.workspace())
            .cloned()
            .filter(|value| value.public.id == offer)
            .ok_or("clipboard offer replaced or unavailable")?;
        offered.check(Instant::now())?;
        let ticket = self.ticket(context, &destination_ticket)?;
        let SelectedObject::Destination(destination) = &ticket.object else {
            return Err("clipboard destination requires a shown folder ticket".into());
        };
        destination.check(&self.tree, context.workspace(), context.owner())?;
        if conflict == Conflict::Ask {
            let clashing = clashing_names(&offered, destination)?;
            if !clashing.is_empty() {
                // Nothing starts: the window asks once and pastes again
                // with the answer.
                let count = clashing.len();
                return Ok(Reply::Conflicts {
                    names: clashing.into_iter().take(MAX_CONFLICT_NAMES).collect(),
                    count,
                });
            }
        }
        let destination = destination.clone();
        let operation = Arc::new(Operation {
            context: context.clone(),
            offer: offered,
            destination,
            destination_ticket,
            destination_custody: ticket,
            id,
            cancelled: AtomicBool::new(false),
            records: Mutex::default(),
            reply: Mutex::default(),
            journal: self.journal.clone(),
            receipt: std::sync::OnceLock::new(),
            destination_name: None,
            restore_info: None,
            conflict,
        });
        self.enqueue(&operation)
    }

    fn enqueue(&self, operation: &Arc<Operation>) -> Result<Reply, String> {
        let context = &operation.context;
        let id = operation.id.clone();
        let mut state = self
            .state
            .lock()
            .map_err(|_| "clipboard state unavailable")?;
        if let Some(prior) = state.operations.get(&(
            context.workspace().into(),
            context.owner().into(),
            id.clone(),
        )) {
            if !prior.matches_paste(
                context,
                &operation.offer.public.id,
                &operation.destination_ticket,
            ) || prior.restore_info.is_some() != operation.restore_info.is_some()
                || prior.destination.path != operation.destination.path
                || prior.destination_name != operation.destination_name
                || (operation.restore_info.is_some()
                    && prior
                        .offer
                        .entries
                        .iter()
                        .zip(&operation.offer.entries)
                        .any(|(a, b)| a.path != b.path || a.identity != b.identity))
            {
                return Err("operation id already names another transfer".into());
            }
            return Ok(prior.answer());
        }
        state.check_paste_capacity(operation)?;
        if operation.offer.public.cut && operation.offer.cut_busy.swap(true, Ordering::AcqRel) {
            return Err("another Cut paste is still running".into());
        }
        state.operations.insert(
            (
                context.workspace().into(),
                context.owner().into(),
                id.clone(),
            ),
            operation.clone(),
        );
        state.workers = state.workers.saturating_add(1);
        drop(state);
        let state = self.state.clone();
        let tree = self.tree.clone();
        let executing = operation.clone();
        match std::thread::Builder::new()
            .name("files-clipboard".into())
            .spawn(move || executing.run_worker(&tree, state))
        {
            Ok(_) => Ok(Reply::Running { operation: id }),
            Err(error) => {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "clipboard state unavailable")?;
                operation.offer.cut_busy.store(false, Ordering::Release);
                state.workers = state.workers.saturating_sub(1);
                state
                    .operations
                    .remove(&(context.workspace().into(), context.owner().into(), id));
                drop(state);
                Err(error.to_string())
            }
        }
    }
}

impl Operation {
    fn matches_paste(
        &self,
        context: &FileClipboardContext,
        offer: &str,
        destination: &str,
    ) -> bool {
        self.offer.public.id == offer
            && self.destination_ticket == destination
            && self.context.peer() == context.peer()
            && self.context.window() == context.window()
    }
    fn run_worker(&self, tree: &WorkspaceTreeHandle, state: Arc<Mutex<State>>) {
        let permit = WorkerPermit {
            state,
            offer: Some(self.offer.clone()),
        };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(tree))).is_err() {
            let completed = self.records.lock_recover().len();
            *self.reply.lock_recover() = Some(Reply::Settled {
                operation: self.id.clone(),
                completed,
                error: Some(
                    "clipboard worker stopped; inspect recorded changes before retrying".into(),
                ),
                undone: false,
            });
        }
        drop(permit);
        #[cfg(test)]
        test_support::finished(&self.destination.path);
    }
    fn answer(&self) -> Reply {
        self.reply
            .lock()
            .ok()
            .and_then(|reply| reply.clone())
            .unwrap_or_else(|| Reply::Running {
                operation: self.id.clone(),
            })
    }
    fn check(&self, tree: &WorkspaceTreeHandle, source: &Source) -> Result<(), String> {
        #[cfg(test)]
        if self.restore_info.is_some() {
            test_support::pause(&self.destination.path, "restore-before-transfer")
                .map_err(|error| error.to_string())?;
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err("clipboard transfer cancelled".into());
        }
        self.context.authorize(Permission::Undoable)?;
        self.context.authorize(Permission::Read)?;
        self.offer.check(Instant::now())?;
        source.check(tree, self.context.workspace(), self.context.owner())?;
        if let Some(info) = &self.restore_info {
            info.check(tree, &self.context)?;
        }
        self.check_destination(tree)
    }
    fn check_destination(&self, tree: &WorkspaceTreeHandle) -> Result<(), String> {
        self.destination_custody
            .check(&self.context, Instant::now())?;
        self.destination
            .check(tree, self.context.workspace(), self.context.owner())
    }
    fn run(&self, tree: &WorkspaceTreeHandle) {
        if let Err(error) = self.journal.begin(self).and_then(|receipt| {
            self.receipt
                .set(receipt)
                .map_err(|_| "clipboard receipt already initialized".into())
        }) {
            if let Ok(mut reply) = self.reply.lock() {
                *reply = Some(Reply::Settled {
                    operation: self.id.clone(),
                    completed: 0,
                    error: Some(error),
                    undone: false,
                });
            }
            return;
        }
        let mut error = None;
        let mut next = 0;
        while error.is_none() && next < self.offer.entries.len() {
            if let Err(reason) = self.run_batch(tree, &mut next) {
                error = Some(reason);
            }
        }
        let completed = self.records.lock().map_or(0, |records| records.len());
        if let Err(reason) = self
            .receipt()
            .and_then(|receipt| receipt.settled(error.clone()))
        {
            error =
                Some(error.map_or_else(|| reason.clone(), |error| format!("{error}; {reason}")));
        }
        if let Ok(mut reply) = self.reply.lock() {
            *reply = Some(Reply::Settled {
                operation: self.id.clone(),
                completed,
                error,
                undone: false,
            });
        }
    }
    /// Paste the next batch of the offer's entries from `next` on, through
    /// the five phases of the module docs. Entries published before an
    /// error stay recorded and undoable.
    fn run_batch(&self, tree: &WorkspaceTreeHandle, next: &mut usize) -> Result<(), String> {
        let mut batch =
            file_explorer::fileops::TransferBatch::new(&self.destination.directory.anchored_root())
                .map_err(|error| error.to_string())?;
        // 1. Stage, with nothing synced or journaled between entries.
        let (staged, mut failure) = self.stage_batch(tree, &mut batch, next);
        if staged.is_empty() {
            return failure.map_or(Ok(()), Err);
        }
        // 2. Sync the copies together, then their folders.
        self.sync_batch(tree, &batch, &staged)?;
        #[cfg(test)]
        interrupt_test_process("batch-staged");
        // 3. One intent for the batch.
        let intent: Vec<_> = staged
            .iter()
            .map(|entry| journal::BatchIntent {
                source: &entry.source,
                staged: &entry.stable,
                seal: entry.seal.clone(),
                quarantine: entry
                    .quarantine
                    .as_ref()
                    .map(|(_, stable)| stable.as_path()),
            })
            .collect();
        self.receipt()?.batch_intent(&intent)?;
        drop(intent);
        #[cfg(test)]
        interrupt_test_process("batch-intent");
        // 4. Publish entry by entry.
        let mut published: Vec<Record> = Vec::new();
        let mut discarded: Vec<Arc<Source>> = Vec::new();
        for entry in staged {
            if failure.is_some() {
                // Unpublished and its source untouched: its copy goes.
                let source = entry.source.clone();
                drop(entry);
                discarded.push(source);
                continue;
            }
            match self.publish(tree, entry) {
                Ok(record) => {
                    published.push(record);
                    #[cfg(test)]
                    if published.len() == 1 {
                        interrupt_test_process("batch-midway");
                    }
                    #[cfg(test)]
                    if let Err(reason) = test_support::pause(&self.destination.path, "after-item") {
                        failure = Some(reason.to_string());
                    }
                }
                Err((reason, source, copy_gone)) => {
                    if copy_gone {
                        discarded.push(source);
                    }
                    failure = Some(reason);
                }
            }
        }
        drop(batch);
        // 5. Sync what moved, then one completion for the batch.
        self.sync_published(&published)?;
        #[cfg(test)]
        interrupt_test_process("batch-published");
        self.complete_batch(tree, published, &discarded)?;
        failure.map_or(Ok(()), Err)
    }

    /// Phase 1: copy entries from `next` on into `batch`, unsynced, until
    /// the batch is full. What was staged, and why staging stopped early.
    fn stage_batch(
        &self,
        tree: &WorkspaceTreeHandle,
        batch: &mut file_explorer::fileops::TransferBatch,
        next: &mut usize,
    ) -> (Vec<Staged>, Option<String>) {
        let mut staged: Vec<Staged> = Vec::new();
        let mut bytes = 0_u64;
        let mut failure = None;
        while *next < self.offer.entries.len() && staged.len() < BATCH_ITEMS && bytes < BATCH_BYTES
        {
            let index = *next;
            *next = index.saturating_add(1);
            let Some(source) = self.offer.entries.get(index) else {
                break;
            };
            let consumed = self
                .offer
                .consumed
                .lock()
                .map_or(true, |items| items.get(index).copied().unwrap_or(true));
            if consumed || self.skips(source) {
                continue;
            }
            match self.stage(tree, batch, source) {
                Ok(entry) => {
                    bytes = bytes.saturating_add(entry.bytes);
                    staged.push(entry);
                }
                Err(reason) => {
                    failure = Some(reason);
                    break;
                }
            }
        }
        (staged, failure)
    }

    /// Phase 5's journal: one completion for the batch, then the published
    /// entries become this paste's records.
    fn complete_batch(
        &self,
        tree: &WorkspaceTreeHandle,
        published: Vec<Record>,
        discarded: &[Arc<Source>],
    ) -> Result<(), String> {
        let completed: Vec<(&Source, &Path)> = published
            .iter()
            .map(|record| (record.source.as_ref(), record.destination.as_path()))
            .collect();
        let gone: Vec<&Source> = discarded.iter().map(AsRef::as_ref).collect();
        let recorded = self.receipt()?.batch_complete(&completed, &gone);
        drop(completed);
        for record in published {
            if self.offer.public.cut
                && let Some(index) = self
                    .offer
                    .entries
                    .iter()
                    .position(|source| Arc::ptr_eq(source, &record.source))
                && let Ok(mut items) = self.offer.consumed.lock()
                && let Some(consumed) = items.get_mut(index)
            {
                *consumed = true;
            }
            let source = record.source.clone();
            if let Ok(mut records) = self.records.lock() {
                records.push(record);
            }
            if recorded.is_ok() {
                self.receipt()?
                    .consume_restore_info(&source, tree, &self.context)?;
            }
        }
        recorded
    }

    /// Whether `source` is skipped: its name is taken and the person chose
    /// Skip.
    fn skips(&self, source: &Source) -> bool {
        matches!(self.conflict, Conflict::Skip)
            && self.destination_name.is_none()
            && self.destination.directory.entry(&source.name).is_ok()
    }

    /// Phase 1 for one entry: copy it into the batch, unsynced.
    fn stage(
        &self,
        tree: &WorkspaceTreeHandle,
        batch: &mut file_explorer::fileops::TransferBatch,
        source: &Arc<Source>,
    ) -> Result<Staged, String> {
        self.check(tree, source)?;
        let mut admission = Admission {
            operation: self,
            source,
            tree,
            staged: None,
        };
        let transfer = batch.stage(
            &source.anchored(),
            self.destination_name.as_deref().unwrap_or(&source.name),
            self.offer.public.cut,
            &mut admission,
        )?;
        let (entry, identity) = admission
            .staged
            .ok_or("staged transfer receipt unavailable")?;
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        let relative = transfer
            .path()
            .strip_prefix(self.destination.directory.anchored_root())
            .map_err(|error| error.to_string())?;
        let stable = self.destination.path.join(relative);
        let quarantine = if self.offer.public.cut {
            let leaf = format!(".tilo-transfer-source-{}", uuid::Uuid::new_v4());
            let parent = source.path.parent().ok_or("source parent unavailable")?;
            Some((
                source.parent.anchored_root().join(&leaf),
                parent.join(&leaf),
            ))
        } else {
            None
        };
        Ok(Staged {
            source: source.clone(),
            transfer,
            entry,
            identity,
            seal: journal::Seal::of(&metadata),
            bytes: metadata.len(),
            stable,
            quarantine,
        })
    }

    /// Phase 2: sync the batch's staged copies on a few threads, so the
    /// filesystem commits them together, then their folders; then check
    /// each entry's admission again before its intent is written.
    fn sync_batch(
        &self,
        tree: &WorkspaceTreeHandle,
        batch: &file_explorer::fileops::TransferBatch,
        staged: &[Staged],
    ) -> Result<(), String> {
        let entries: Vec<Arc<File>> = staged.iter().map(|entry| entry.entry.clone()).collect();
        let claimed = std::sync::atomic::AtomicUsize::new(0);
        let failed: Mutex<Option<String>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for _ in 0..SYNC_THREADS.min(entries.len()) {
                scope.spawn(|| {
                    while let Some(entry) = entries.get(claimed.fetch_add(1, Ordering::Relaxed)) {
                        let synced = journal::sync_staged(entry, &mut || {
                            if self.cancelled.load(Ordering::Acquire) {
                                Err(io::Error::other("clipboard transfer cancelled"))
                            } else {
                                Ok(())
                            }
                        });
                        if let Err(error) = synced {
                            *failed.lock_recover() = Some(error.to_string());
                            break;
                        }
                    }
                });
            }
        });
        if let Some(error) = failed
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(error);
        }
        File::open(batch.container())
            .and_then(|folder| folder.sync_all())
            .and_then(|()| File::open(self.destination.directory.anchored_root()))
            .and_then(|folder| folder.sync_all())
            .map_err(|error| error.to_string())?;
        for entry in staged {
            self.check(tree, &entry.source)?;
        }
        Ok(())
    }

    /// Phase 4 for one entry. On failure: why, its source, and whether its
    /// copy is gone (so nothing of it is left to recover).
    fn publish(
        &self,
        tree: &WorkspaceTreeHandle,
        entry: Staged,
    ) -> Result<Record, (String, Arc<Source>, bool)> {
        let Staged {
            source,
            transfer,
            entry: staged_entry,
            identity,
            quarantine,
            ..
        } = entry;
        let staged_path = transfer.path().to_path_buf();
        let replaced = match self.make_room(tree, &source) {
            Ok(replaced) => replaced,
            Err(reason) => {
                drop(transfer);
                return Err((reason, source, true));
            }
        };
        let mut admission = Admission {
            operation: self,
            source: &source,
            tree,
            staged: Some((staged_entry.clone(), identity)),
        };
        let finished = transfer.finish(
            quarantine.as_ref().map(|(anchored, _)| anchored.as_path()),
            &mut admission,
        );
        match finished {
            Ok(destination) => {
                let seal = staged_entry
                    .metadata()
                    .map(|metadata| journal::Seal::of(&metadata))
                    .map_err(|error| (error.to_string(), source.clone(), false))?;
                let name = destination
                    .file_name()
                    .ok_or_else(|| ("destination has no name".to_owned(), source.clone(), false))?;
                Ok(Record {
                    destination: self.destination.path.join(name),
                    source,
                    seal,
                    identity,
                    replaced,
                    undone: false,
                })
            }
            Err(reason) => {
                // The replacement didn't arrive: put the original back.
                let reason = match replaced {
                    Some(slot) => match self.restore_replaced(&slot, &source.name) {
                        Ok(()) => reason,
                        Err(restore) => format!("{reason}; {restore}"),
                    },
                    None => reason,
                };
                let gone = fs::symlink_metadata(&staged_path)
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
                Err((reason, source, gone))
            }
        }
    }

    /// Phase 5's sync: the receiving folder, and each Cut source folder once.
    fn sync_published(&self, published: &[Record]) -> Result<(), String> {
        if published.is_empty() {
            return Ok(());
        }
        File::open(self.destination.directory.anchored_root())
            .and_then(|folder| folder.sync_all())
            .map_err(|error| error.to_string())?;
        if self.offer.public.cut {
            let mut synced = HashSet::new();
            for record in published {
                let folder = record.source.parent.anchored_root();
                if synced.insert(folder.clone()) {
                    File::open(&folder)
                        .and_then(|folder| folder.sync_all())
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// What the destination's entry `name` is now.
    fn destination_metadata(&self, name: &std::ffi::OsStr) -> Result<fs::Metadata, String> {
        self.destination
            .directory
            .entry(name)?
            .metadata()
            .map_err(|error| error.to_string())
    }

    /// Make room for `source` just before it is published: when its name is
    /// taken and the person chose Replace, the item there goes to Trash.
    /// Where it went, to put it back if the paste fails.
    fn make_room(
        &self,
        tree: &WorkspaceTreeHandle,
        source: &Source,
    ) -> Result<Option<PathBuf>, String> {
        let taken = self.destination_name.is_none()
            && self.destination.directory.entry(&source.name).is_ok();
        if taken && matches!(self.conflict, Conflict::Replace) {
            self.check(tree, source)?;
            return Ok(Some(self.trash_existing(&source.name)?));
        }
        Ok(None)
    }

    /// Move the destination's own item called `name` to Trash, so a Replace
    /// never destroys it; where it went, to put it back on undo.
    fn trash_existing(&self, name: &std::ffi::OsStr) -> Result<PathBuf, String> {
        let slot = trash::TrashSlot::prepare(&self.destination.path.join(name))?;
        let files = trash::files_directory()?;
        let leaf = slot
            .path()
            .file_name()
            .ok_or("Trash slot has no name")?
            .to_os_string();
        rename_entry(&self.destination.directory, name, &files, &leaf).map_err(|reason| {
            format!(
                "couldn't move the existing “{}” to Trash: {reason}",
                name.to_string_lossy()
            )
        })?;
        slot.publish_with(&files, &leaf, || Ok(()))
    }

    /// Put back the item a Replace sent to Trash, once its replacement is
    /// gone.
    fn restore_replaced(&self, slot: &Path, name: &std::ffi::OsStr) -> Result<(), String> {
        let files = trash::files_directory()?;
        let leaf = slot.file_name().ok_or("Trash slot has no name")?;
        rename_entry(&files, leaf, &self.destination.directory, name).map_err(|reason| {
            format!(
                "the replaced “{}” stays in Trash: {reason}",
                name.to_string_lossy()
            )
        })?;
        trash::consume_info(slot)
    }

    fn receipt(&self) -> Result<&journal::PendingReceipt, String> {
        self.receipt
            .get()
            .map(Arc::as_ref)
            .ok_or_else(|| "clipboard durable receipt unavailable".into())
    }

    fn undo(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Undoable)?;
        if self.receipt()?.needs_resume()? {
            let answer = self.receipt()?.undo(tree, context)?;
            if matches!(
                &answer,
                Reply::Settled {
                    error: None,
                    undone: true,
                    ..
                }
            ) {
                for record in self
                    .records
                    .lock()
                    .map_err(|_| "clipboard receipts unavailable")?
                    .iter_mut()
                {
                    record.undone = true;
                }
            }
            *self
                .reply
                .lock()
                .map_err(|_| "clipboard reply unavailable")? = Some(answer.clone());
            return Ok(answer);
        }
        let mut reply = self
            .reply
            .lock()
            .map_err(|_| "clipboard reply unavailable")?;
        let Some(Reply::Settled { completed, .. }) = reply.as_ref() else {
            return Err("clipboard transfer has not settled".into());
        };
        let completed = *completed;
        let mut records = self
            .records
            .lock()
            .map_err(|_| "clipboard receipts unavailable")?;
        let mut error = None;
        for record in records.iter_mut().rev().filter(|record| !record.undone) {
            if let Err(reason) = self.undo_one(tree, context, record) {
                error = Some(reason);
                break;
            }
            if let Some(slot) = record.replaced.take()
                && let Some(name) = record.destination.file_name()
                && let Err(reason) = self.restore_replaced(&slot, name)
            {
                error = Some(reason);
                break;
            }
            self.receipt()?
                .restore_source_info(&record.source, tree, context)?;
            self.receipt()?.undone(&record.source)?;
            record.undone = true;
        }
        let undone = records.iter().all(|record| record.undone);
        drop(records);
        let answer = Reply::Settled {
            operation: self.id.clone(),
            completed,
            error,
            undone,
        };
        *reply = Some(answer.clone());
        drop(reply);
        Ok(answer)
    }

    fn verify_undo<'a>(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
        record: &'a Record,
    ) -> Result<&'a std::ffi::OsStr, String> {
        context.authorize(Permission::Undoable)?;
        self.destination
            .check(tree, context.workspace(), context.owner())?;
        let name = record
            .destination
            .file_name()
            .ok_or("receipt destination has no name")?;
        let metadata = self.destination_metadata(name)?;
        if Identity::of(&metadata) != record.identity || !record.seal.same_object(&metadata) {
            return Err("receipt destination was replaced or removed".into());
        }
        if self.offer.public.cut {
            record
                .source
                .grant
                .check(tree, context.workspace(), context.owner())?;
            let parent = record
                .source
                .path
                .parent()
                .ok_or("source parent unavailable")?;
            let current =
                SecureNearbyDestination::open_beneath(record.source.grant.root(), parent)?;
            if Identity::of(&current.metadata()?) != Identity::of(&record.source.parent.metadata()?)
            {
                return Err("original source folder was replaced or moved".into());
            }
        }
        Ok(name)
    }

    fn undo_one(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
        record: &Record,
    ) -> Result<(), String> {
        let name = self.verify_undo(tree, context, record)?;
        let recovery_name =
            OsString::from(format!(".tilo-clipboard-undo-{}", uuid::Uuid::new_v4()));
        let recovery_path = self
            .destination
            .directory
            .anchored_root()
            .join(&recovery_name);
        let recovery = self.destination.directory.private_child(&recovery_name)?;
        let retained = OsString::from("payload");
        self.receipt()?.undo_intent(
            &record.source,
            &self.destination.path.join(&recovery_name).join(&retained),
            &self.destination_metadata(name)?,
        )?;
        if let Err(reason) = rename_entry(&self.destination.directory, name, &recovery, &retained) {
            let _ = fs::remove_dir(&recovery_path);
            return Err(reason);
        }
        let identity = recovery
            .entry(&retained)
            .and_then(|entry| entry.metadata().map_err(|error| error.to_string()))
            .map(|metadata| Identity::of(&metadata));
        let admitted = context.authorize(Permission::Undoable).and_then(|()| {
            self.destination
                .check(tree, context.workspace(), context.owner())
        });
        let result = if identity != Ok(record.identity) {
            Err("receipt destination changed before undo".into())
        } else if let Err(reason) = admitted {
            Err(reason)
        } else if self.offer.public.cut {
            record
                .source
                .check_parent(tree, context.workspace(), context.owner())
                .and_then(|()| {
                    self.receipt()?.relocate_source(
                        &record.source,
                        (
                            &self.destination.path.join(&recovery_name).join(&retained),
                            &record.source.path,
                        ),
                        tree,
                        context,
                    )
                })
        } else {
            // The standard Trash owns recovery, including edits made after
            // Paste. Its existing typed Restore action returns this exact item.
            trash::TrashSlot::prepare(&record.destination)
                .and_then(|slot| {
                    self.receipt()?.trash_intent(&record.source, slot.path())?;
                    let target = slot.path().to_path_buf();
                    slot.publish_with(&recovery, &retained, || {
                        self.receipt()?.relocate_source(
                            &record.source,
                            (
                                &self.destination.path.join(&recovery_name).join(&retained),
                                &target,
                            ),
                            tree,
                            context,
                        )
                    })
                })
                .map(|_| ())
        };
        if let Err(reason) = result {
            if self.receipt()?.has_active_move(&record.source)? {
                return Err(format!(
                    "{reason}; recover the recorded clipboard transfer before retrying"
                ));
            }
            return match rename_entry(&recovery, &retained, &self.destination.directory, name) {
                Ok(()) => {
                    let _ = fs::remove_dir(&recovery_path);
                    Err(reason)
                }
                Err(restore) => Err(format!(
                    "{reason}; undo recovery retained at {}: {restore}",
                    self.destination
                        .path
                        .join(&recovery_name)
                        .join(&retained)
                        .display()
                )),
            };
        }
        let _ = fs::remove_dir(&recovery_path);
        if self.offer.public.cut {
            File::open(record.source.parent.anchored_root())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
        }
        File::open(self.destination.directory.anchored_root())
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn rename_entry(
    source: &SecureNearbyDestination,
    name: &std::ffi::OsStr,
    destination: &SecureNearbyDestination,
    target: &std::ffi::OsStr,
) -> Result<(), String> {
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|error| error.to_string())?;
    let target = std::ffi::CString::new(target.as_bytes()).map_err(|error| error.to_string())?;
    // SAFETY: retained directory descriptors and single-component terminated
    // leaf names. RENAME_NOREPLACE never overwrites a concurrently created item.
    let result = unsafe {
        libc::renameat2(
            source.directory_fd(),
            name.as_ptr(),
            destination.directory_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().to_string())
    }
}

struct Admission<'a> {
    operation: &'a Operation,
    source: &'a Source,
    tree: &'a WorkspaceTreeHandle,
    staged: Option<(Arc<File>, Identity)>,
}
impl Admission<'_> {
    fn check(&self) -> io::Result<()> {
        self.operation
            .check(self.tree, self.source)
            .map_err(io::Error::other)
    }
    fn exact(&self, metadata: &fs::Metadata) -> io::Result<()> {
        if Identity::of(metadata) != self.source.identity {
            return Err(io::Error::other("source identity changed"));
        }
        Ok(())
    }
}
impl file_explorer::fileops::TransferObserver for Admission<'_> {
    fn open_source(
        &mut self,
        parent: &file_explorer::fileops::SourceDirectory,
        name: &std::ffi::OsStr,
        _options: &file_explorer::fileops::SourceOpenOptions,
    ) -> io::Result<File> {
        let name = std::ffi::CString::new(name.as_bytes())?;
        // SAFETY: retained directory descriptor and one terminated leaf name.
        // Every source is read-only, never followed, and nonblocking so a FIFO
        // substitution cannot hang before opened_source validates its identity.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned this newly owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn staged_source(&mut self, path: &Path) -> io::Result<()> {
        self.check()?;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: terminated name and an owned O_PATH descriptor; the staged
        // leaf can itself be a symlink, whose identity must remain unfollowed.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful open returned this newly owned descriptor.
        let entry = unsafe { File::from_raw_fd(fd) };
        let identity = Identity::of(&entry.metadata()?);
        // Its batch syncs it with the others and names it in one intent.
        self.staged = Some((Arc::new(entry), identity));
        Ok(())
    }
    fn before_source_quarantine(&mut self, _: &Path, _: &Path) -> io::Result<()> {
        // The hidden name is in the batch's durable intent already.
        self.check()
    }
    fn allow_direct_rename(&self) -> bool {
        false
    }
    fn before_copy_source(&mut self, _: &Path) -> io::Result<()> {
        self.check()
    }
    fn opened_source(&mut self, path: &Path, source: &File) -> io::Result<()> {
        self.check()?;
        if path == self.source.anchored() {
            self.exact(&source.metadata()?)?;
        }
        Ok(())
    }
    fn symlink_target(&mut self, path: &Path) -> io::Result<Option<PathBuf>> {
        self.check()?;
        Ok((path == self.source.anchored())
            .then(|| self.source.link.clone())
            .flatten())
    }
    fn before_publish(&mut self, path: &Path) -> io::Result<()> {
        if let Some(name) = &self.operation.destination_name
            && path.file_name() != Some(name.as_os_str())
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "original Restore location is occupied; Trash data is retained",
            ));
        }
        if let Some(info) = &self.operation.restore_info {
            info.check(self.tree, &self.operation.context)
                .map_err(io::Error::other)?;
        }
        if self.operation.offer.public.cut {
            // The source has already been sequestered/deleted; revalidate
            // authority and destination, without reopening the old leaf name.
            self.source
                .check_parent(
                    self.tree,
                    self.operation.context.workspace(),
                    self.operation.context.owner(),
                )
                .map_err(io::Error::other)?;
            self.operation
                .context
                .authorize(Permission::Undoable)
                .map_err(io::Error::other)?;
            self.operation
                .offer
                .check(Instant::now())
                .map_err(io::Error::other)?;
            self.operation
                .check_destination(self.tree)
                .map_err(io::Error::other)?;
            if self.operation.cancelled.load(Ordering::Acquire) {
                return Err(io::Error::other("clipboard transfer cancelled"));
            }
            #[cfg(test)]
            interrupt_test_process("cut-before-publish");
            Ok(())
        } else {
            self.check()
        }
    }
    fn after_source_quarantine(&mut self, path: &Path) -> io::Result<()> {
        self.exact(&fs::symlink_metadata(path)?)?;
        self.operation
            .context
            .authorize(Permission::Undoable)
            .map_err(io::Error::other)?;
        self.operation
            .offer
            .check(Instant::now())
            .map_err(io::Error::other)?;
        self.source
            .check_parent(
                self.tree,
                self.operation.context.workspace(),
                self.operation.context.owner(),
            )
            .map_err(io::Error::other)?;
        self.operation
            .check_destination(self.tree)
            .map_err(io::Error::other)?;
        if self.operation.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::other("clipboard transfer cancelled"));
        }
        Ok(())
    }
    fn before_source_restore(&mut self, _: &Path, material: &Path) -> io::Result<()> {
        self.operation
            .context
            .authorize(Permission::Undoable)
            .map_err(io::Error::other)?;
        self.operation
            .offer
            .check(Instant::now())
            .map_err(io::Error::other)?;
        self.source
            .check_parent(
                self.tree,
                self.operation.context.workspace(),
                self.operation.context.owner(),
            )
            .map_err(io::Error::other)?;
        self.operation
            .check_destination(self.tree)
            .map_err(io::Error::other)?;
        let identity = Identity::of(&fs::symlink_metadata(material)?);
        if identity == self.source.identity {
            return self.exact(&self.source.entry().map_err(io::Error::other)?.metadata()?);
        }
        if let Some((entry, expected)) = &self.staged
            && identity == *expected
            && Identity::of(&entry.metadata()?) == *expected
        {
            return Ok(());
        }
        Err(io::Error::other("Cut recovery object identity changed"))
    }
    fn before_source_remove(&mut self, path: &Path) -> io::Result<()> {
        self.after_source_quarantine(path)?;
        #[cfg(test)]
        interrupt_test_process("cut-before-remove");
        #[cfg(test)]
        test_support::pause(&self.operation.destination.path, "before-source-remove")?;
        Ok(())
    }
    fn copy_bytes(&mut self, source: &mut File, destination: &mut File) -> io::Result<u64> {
        let mut buffer = vec![0_u8; 256 * 1024];
        let mut total = 0_u64;
        let mut next_check = 0_u64;
        loop {
            if total >= next_check {
                self.check()?;
                next_check = total.saturating_add(8 * 1024 * 1024);
            }
            if self.operation.cancelled.load(Ordering::Acquire) {
                return Err(io::Error::other("clipboard transfer cancelled"));
            }
            let count = source.read(&mut buffer)?;
            if count == 0 {
                return Ok(total);
            }
            destination.write_all(
                buffer
                    .get(..count)
                    .ok_or_else(|| io::Error::other("invalid read length"))?,
            )?;
            total = total.saturating_add(u64::try_from(count).map_err(io::Error::other)?);
        }
    }
}

impl FileClipboardServices for NativeFileClipboard {
    fn discover_selected(
        &self,
        context: tilo_action_broker::services::SelectedDiscoveryContext,
        request: runtime_collab::open_selected::discovery::Request,
    ) -> tilo_action_broker::services::SelectedDiscoveryFuture {
        super::super::launcher::retained_home::discovery::begin(self.tree.clone(), context, request)
    }

    fn open_selected_v2(
        &self,
        context: tilo_action_broker::services::OpenSelectedContext,
        request: runtime_collab::open_selected::v2::Request,
    ) -> tilo_action_broker::services::OpenSelectedV2Future {
        super::super::launcher::retained_home::selected_target::begin_v2(
            self.tree.clone(),
            context,
            request,
        )
    }
    fn open_selected(
        &self,
        context: tilo_action_broker::services::OpenSelectedContext,
        request: runtime_collab::open_selected::Request,
    ) -> tilo_action_broker::services::OpenSelectedFuture {
        super::super::launcher::retained_home::selected_target::begin(
            self.tree.clone(),
            context,
            request,
        )
    }

    fn command(&self, context: &FileClipboardContext, command: Command) -> Result<Reply, String> {
        if !command.valid() {
            return Err("invalid clipboard command".into());
        }
        match command {
            Command::Snapshot => self.snapshot(context),
            Command::Recover { operation } => {
                self.compensation(context, operation, CompensationKind::Recover)
            }
            Command::Capture {
                sources,
                destination,
            } => self.capture(context, sources, destination),
            Command::CaptureNamed { folder, entries } => {
                self.capture_named(context, &folder, entries)
            }
            Command::Release { tickets } => self.release(context, &tickets),
            Command::Publish { sources, cut } => self.publish(context, sources, cut),
            Command::Paste {
                offer,
                destination,
                operation,
                conflict,
            } => self.paste(context, &offer, destination, operation, conflict),
            Command::RestoreTrash { source, operation } => self
                .restore_trash(context, &source, operation)
                .or_else(|reason| Ok(Reply::Rejected { reason })),
            Command::Observe { operation } => {
                context.authorize(Permission::Read)?;
                if let Some(answer) = self.compensation_answer(context, &operation)? {
                    return Ok(answer);
                }
                match self.compensation_operation(context, &operation) {
                    Ok(operation) => Ok(operation.answer()),
                    Err(_) => self.journal.find(context, &operation)?.answer(),
                }
            }
            Command::Cancel { operation } => {
                context.authorize(Permission::Undoable)?;
                let operation = self.operation(context, &operation)?;
                operation.cancelled.store(true, Ordering::Release);
                Ok(operation.answer())
            }
            Command::Redo { operation } => {
                self.compensation(context, operation, CompensationKind::Redo)
            }
            Command::Undo { operation } => {
                self.compensation(context, operation, CompensationKind::Undo)
            }
        }
    }
    fn disconnect(&self, peer: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.offers.retain(|_, offer| offer.source.peer() != peer);
            state
                .tickets
                .retain(|_, ticket| ticket.context.peer() != peer);
            for operation in state.operations.values() {
                if operation.context.peer() == peer || operation.offer.source.peer() == peer {
                    operation.cancelled.store(true, Ordering::Release);
                }
            }
        }
    }
}

// The real destructive phases are exercised in a separate test process. No
// production build contains this switch, and SIGKILL runs no cleanup handlers.
#[cfg(test)]
fn interrupt_test_process(phase: &str) {
    if std::env::var("TILO_CLIPBOARD_TEST_KILL_AT").ok().as_deref() == Some(phase) {
        // SAFETY: kill targets only this test process, with a fixed signal.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        unreachable!("the isolated clipboard test process must be killed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

    fn granted(directory: &Path) -> (WorkspaceTreeHandle, String) {
        let mut workspace = Workspace::new(WorkspaceId(1), "Clipboard descriptor test");
        let room = workspace.room.to_string();
        workspace.grant_app(
            super::super::nearby_transfer::FILES_APP_ID,
            &Capability::Filesystem {
                path: directory.to_path_buf(),
                mode: FsMode::ReadWrite,
                scope: crate::shell::state::GRANT_SESSION,
            },
        );
        let mut tree = WorkspaceTree {
            persist_path: Some(directory.join("state.json")),
            ..Default::default()
        };
        tree.workspaces.push(workspace);
        (tree.shared(), room)
    }

    #[test]
    fn clipboard_source_replacement_never_retargets_its_retained_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        fs::write(&path, b"original").unwrap();
        let (tree, room) = granted(directory.path());
        let source = Source::open(&tree, &room, path.clone(), false, "").unwrap();
        source.check(&tree, &room, "").unwrap();
        fs::rename(&path, directory.path().join("retained.txt")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(
            source
                .check(&tree, &room, "")
                .unwrap_err()
                .contains("replaced")
        );
        assert_eq!(
            Identity::of(&source.entry().unwrap().metadata().unwrap()),
            source.identity
        );
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(
            fs::read(directory.path().join("retained.txt")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn clipboard_destination_replacement_never_retargets_its_retained_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("destination");
        fs::create_dir(&path).unwrap();
        let (tree, room) = granted(directory.path());
        let destination = Destination::open(&tree, &room, path.clone(), "").unwrap();
        fs::rename(&path, directory.path().join("retained")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            destination
                .check(&tree, &room, "")
                .unwrap_err()
                .contains("replaced")
        );
        fs::write(
            destination.directory.anchored_root().join("proof.txt"),
            b"anchored",
        )
        .unwrap();
        assert!(!path.join("proof.txt").exists());
        assert_eq!(
            fs::read(directory.path().join("retained/proof.txt")).unwrap(),
            b"anchored"
        );
    }

    #[test]
    fn clipboard_source_parent_move_outside_grant_refuses_cut_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let grant = directory.path().join("grant");
        let parent = grant.join("selected");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join("report.txt");
        fs::write(&path, b"original").unwrap();
        let (tree, room) = granted(&grant);
        let source = Source::open(&tree, &room, path, true, "").unwrap();
        source.check_parent(&tree, &room, "").unwrap();
        let moved = directory.path().join("outside");
        fs::rename(&parent, &moved).unwrap();
        fs::create_dir(&parent).unwrap();
        assert!(
            source
                .check_parent(&tree, &room, "")
                .unwrap_err()
                .contains("replaced or moved")
        );
        assert_eq!(fs::read(moved.join("report.txt")).unwrap(), b"original");
        assert!(!parent.join("report.txt").exists());
    }

    #[test]
    fn clipboard_read_only_grant_allows_copy_but_not_cut_or_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        fs::write(&path, b"original").unwrap();
        let mut workspace = Workspace::new(WorkspaceId(1), "Read-only clipboard test");
        let room = workspace.room.to_string();
        workspace.grant_app(
            super::super::nearby_transfer::FILES_APP_ID,
            &Capability::Filesystem {
                path: directory.path().to_path_buf(),
                mode: FsMode::Read,
                scope: crate::shell::state::GRANT_SESSION,
            },
        );
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let tree = tree.shared();
        let source = Source::open(&tree, &room, path.clone(), false, "").unwrap();
        source.check(&tree, &room, "").unwrap();
        assert!(Source::open(&tree, &room, path.clone(), true, "").is_err());
        assert!(Destination::open(&tree, &room, directory.path().to_path_buf(), "").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn clipboard_symlink_is_retained_as_data_without_opening_its_target() {
        let directory = tempfile::tempdir().unwrap();
        let grant = directory.path().join("grant");
        fs::create_dir(&grant).unwrap();
        let outside = directory.path().join("outside.txt");
        fs::write(&outside, b"not granted").unwrap();
        let path = grant.join("link");
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        let (tree, room) = granted(&grant);
        let source = Source::open(&tree, &room, path, false, "").unwrap();
        source.check(&tree, &room, "").unwrap();
        assert!(
            source
                .entry()
                .unwrap()
                .metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(source.link.as_deref(), Some(outside.as_path()));
        assert!(Grant::open(&tree, &room, &outside, false, "").is_err());
        assert_eq!(fs::read(outside).unwrap(), b"not granted");
    }

    #[test]
    fn clipboard_live_grant_revocation_and_root_replacement_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grant");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("report.txt"), b"original").unwrap();
        let (tree, room) = granted(&path);
        let source = Source::open(&tree, &room, path.join("report.txt"), true, "").unwrap();
        let original_identity = source.grant.identity;
        fs::rename(&path, directory.path().join("retained")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            source
                .grant
                .check(&tree, &room, "")
                .unwrap_err()
                .contains("identity changed")
        );
        assert_eq!(
            Identity::of(&source.grant.entry.metadata().unwrap()),
            original_identity
        );
        {
            let mut tree = tree.write();
            let workspace = tree.workspaces.first_mut().expect("test workspace");
            let grants = workspace.app_grant_ids_at(
                super::super::nearby_transfer::FILES_APP_ID,
                tilo_clock::unix_now(),
            );
            assert!(!grants.is_empty());
            for grant in grants {
                assert!(workspace.revoke_sandbox(&grant));
            }
            drop(tree);
        }
        assert!(
            source
                .grant
                .check(&tree, &room, "")
                .unwrap_err()
                .contains("revoked")
        );
    }
}

#[cfg(test)]
mod bridge_tests;
