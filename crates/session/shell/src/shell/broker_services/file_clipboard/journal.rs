//! Durable write-ahead clipboard receipts. Saved paths never grant authority.
use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::OpenOptionsExt as _;

mod relocate;

const MAX_RECEIPT_BYTES: u64 = 2 * 1024 * 1024;
/// A receipt is written whole at most every this many item steps; the steps
/// between are appended to its log, so a paste of many entries costs one
/// short append per step instead of rewriting every entry each time.
const LOG_COMPACT: usize = 64;
/// The most a receipt's step log holds before it is folded in.
const MAX_LOG_BYTES: u64 = MAX_RECEIPT_BYTES;

/// How many items one chunk file of a receipt holds. A receipt of any size
/// is a header plus chunk files, so no single file grows with the paste.
const CHUNK_ITEMS: usize = 256;
/// The receipt format written as a header plus chunk files.
const CHUNKED_VERSION: u8 = 3;

/// The chunk file holding chunk `index` as written in `generation`.
fn chunk_name(name: &str, index: usize, generation: u64) -> Option<String> {
    name.strip_suffix(".json")
        .map(|stem| format!("{stem}.c{index}.g{generation}.json"))
}

/// One item step appended to a receipt's log: the item's whole new state.
/// It applies only to the receipt generation it was written against.
#[derive(Serialize, Deserialize)]
struct LogLine {
    generation: u64,
    index: usize,
    item: Item,
}

/// Write `bytes` as the new private file `name` in `directory`, durably.
fn write_private(directory: &SecureNearbyDestination, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = format!(".pending-{}", uuid::Uuid::new_v4());
    let path = directory.anchored_root().join(&temporary);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&path, directory.anchored_root().join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result
}

/// Whether a receipt still has work to resume or undo (its pending marker).
fn pending_active(receipt: &Receipt) -> bool {
    (!receipt.settled || receipt.error.is_some()) && receipt.items.iter().any(|item| !item.undone)
        || receipt.items.iter().any(|item| {
            !item.undone && (item.undo_recovery.is_some() || item.redoing || item.moving.is_some())
        })
}

/// The log file beside receipt `name` (`<hash>.json` → `<hash>.log`).
fn log_name(name: &str) -> Option<String> {
    name.strip_suffix(".json").map(|stem| format!("{stem}.log"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Seal {
    identity: (u64, u64),
    born: Option<(u64, u32)>,
    changed: (i64, i64),
}
impl Seal {
    pub(super) fn of(metadata: &fs::Metadata) -> Self {
        Self {
            identity: (metadata.dev(), metadata.ino()),
            born: metadata.created().ok().and_then(|time| {
                time.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|time| (time.as_secs(), time.subsec_nanos()))
            }),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    pub(super) fn unchanged(&self, metadata: &fs::Metadata) -> bool {
        self.matches(metadata) && self.changed == (metadata.ctime(), metadata.ctime_nsec())
    }

    /// Whether `metadata` is the same object, edits allowed: its identity,
    /// and its birth time where the filesystem keeps one. For a pasted copy
    /// a person may have edited since; never for a source about to be read.
    pub(super) fn same_object(&self, metadata: &fs::Metadata) -> bool {
        let current = Self::of(metadata);
        self.identity == current.identity
            && match (self.born, current.born) {
                (Some(saved), Some(now)) => saved == now,
                _ => true,
            }
    }

    pub(super) fn matches(&self, metadata: &fs::Metadata) -> bool {
        let current = Self::of(metadata);
        self.identity == current.identity
            && match self.born {
                Some(born) => current.born == Some(born),
                // Without birth time, fail closed when edits or inode reuse make
                // the saved object ambiguous. Never substitute a new path occupant.
                None => current.changed == self.changed,
            }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Folder {
    pub(super) path: Vec<u8>,
    root: Vec<u8>,
    root_identity: Seal,
    identity: Seal,
}
impl Folder {
    pub(super) fn new(
        path: &Path,
        grant: &Grant,
        folder: &SecureNearbyDestination,
    ) -> Result<Self, String> {
        let metadata = folder.metadata()?;
        Ok(Self {
            path: path.as_os_str().as_bytes().into(),
            root: grant.root.as_os_str().as_bytes().into(),
            root_identity: Seal::of(&grant.entry.metadata().map_err(|error| error.to_string())?),
            identity: Seal::of(&metadata),
        })
    }
    pub(super) fn open(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<SecureNearbyDestination, String> {
        context.authorize(Permission::Undoable)?;
        let path = decode_path(&self.path)?;
        let grant = Grant::open(tree, context.workspace(), &path, true, context.owner())?;
        grant.check(tree, context.workspace(), context.owner())?;
        if grant.root.as_os_str().as_bytes() != self.root
            || !self
                .root_identity
                .matches(&grant.entry.metadata().map_err(|error| error.to_string())?)
        {
            return Err("saved clipboard grant was replaced".into());
        }
        let folder = SecureNearbyDestination::open_beneath(grant.root(), &path)?;
        let metadata = folder.metadata()?;
        if !self.identity.matches(&metadata) {
            return Err("saved clipboard folder was replaced".into());
        }
        Ok(folder)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Item {
    original: Vec<u8>,
    parent: Folder,
    original_seal: Seal,
    staged: Option<(Vec<u8>, Seal)>,
    quarantine: Option<Vec<u8>>,
    #[serde(default)]
    quarantine_trash: Option<Vec<u8>>,
    destination: Option<Vec<u8>>,
    published: bool,
    undo_recovery: Option<Vec<u8>>,
    trash: Option<Vec<u8>>,
    undone: bool,
    #[serde(default)]
    redoing: bool,
    #[serde(default)]
    moving: Option<relocate::MoveIntent>,
    #[serde(default)]
    restore_info: Option<super::restore::Info>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    workspace: String,
    owner: String,
    window: String,
    operation: String,
    cut: bool,
    destination: Folder,
    items: Vec<Item>,
    error: Option<String>,
    settled: bool,
    /// Which whole write this is; log lines name it.
    #[serde(default)]
    generation: u64,
    /// For a chunked receipt (version 3): the generation of the file
    /// holding each chunk of [`Self::items`], which the header then omits.
    #[serde(default)]
    chunks: Vec<u64>,
}
impl Receipt {
    fn filename(&self) -> Result<String, String> {
        receipt_name(&self.workspace, &self.window, &self.operation)
    }
}

pub(super) struct Journal {
    root: Option<PathBuf>,
}
impl Journal {
    pub(super) fn new(tree: &WorkspaceTreeHandle) -> Self {
        Self {
            root: tree
                .read()
                .persist_path
                .as_ref()
                .and_then(|path| path.parent().map(|parent| parent.join("files-clipboard"))),
        }
    }
    fn directory(&self) -> Result<Option<SecureNearbyDestination>, String> {
        let Some(root) = &self.root else {
            return Err("durable Files clipboard storage is unavailable".into());
        };
        let parent = root.parent().ok_or("clipboard journal has no parent")?;
        let directory = SecureNearbyDestination::open_beneath(parent, parent)?;
        super::trash::private_directory(&directory, "files-clipboard").map(Some)
    }
    pub(super) fn begin(&self, operation: &Operation) -> Result<Arc<PendingReceipt>, String> {
        let mut items = Vec::with_capacity(operation.offer.entries.len());
        for source in &operation.offer.entries {
            items.push(Item {
                original: source.path.as_os_str().as_bytes().into(),
                parent: Folder::new(
                    source.path.parent().ok_or("source folder unavailable")?,
                    &source.grant,
                    &source.parent,
                )?,
                original_seal: Seal::of(
                    &source
                        .entry()?
                        .metadata()
                        .map_err(|error| error.to_string())?,
                ),
                staged: None,
                quarantine: None,
                quarantine_trash: None,
                destination: None,
                published: false,
                undo_recovery: None,
                trash: None,
                undone: false,
                redoing: false,
                moving: None,
                restore_info: operation.restore_info.clone(),
            });
        }
        let receipt = Receipt {
            // Older hosts must refuse Restore receipts rather than dropping
            // their metadata compensation and treating them as ordinary Cut.
            version: if operation.restore_info.is_some() {
                2
            } else {
                CHUNKED_VERSION
            },
            workspace: operation.context.workspace().into(),
            owner: operation.context.owner().into(),
            window: operation.context.window().into(),
            operation: operation.id.clone(),
            cut: operation.offer.public.cut,
            destination: Folder::new(
                &operation.destination.path,
                &operation.destination.grant,
                &operation.destination.directory,
            )?,
            items,
            error: None,
            settled: false,
            generation: 0,
            chunks: Vec::new(),
        };
        let pending = Arc::new(PendingReceipt {
            directory: self.directory()?,
            receipt: Mutex::new(receipt),
            published: AtomicBool::new(false),
            logged: std::sync::atomic::AtomicUsize::new(0),
            dirty: Mutex::default(),
            positions: Mutex::default(),
        });
        pending.change(|_| Ok(()))?;
        Ok(pending)
    }
    pub(super) fn find(
        &self,
        context: &FileClipboardContext,
        id: &str,
    ) -> Result<Arc<PendingReceipt>, String> {
        context.authorize(if context.is_history_compensation() {
            Permission::Undoable
        } else {
            Permission::Read
        })?;
        let directory = self
            .directory()?
            .ok_or("durable clipboard receipt unavailable")?;
        let name = receipt_name(context.workspace(), context.window(), id)?;
        let receipt = Self::read(&directory, std::ffi::OsStr::new(&name))?;
        if receipt.workspace != context.workspace()
            || receipt.window != context.window()
            || receipt.operation != id
            || (!context.is_history_compensation() && receipt.owner != context.owner())
        {
            return Err("clipboard receipt belongs to another admitted namespace".into());
        }
        Ok(Arc::new(PendingReceipt {
            directory: Some(directory),
            receipt: Mutex::new(receipt),
            published: AtomicBool::new(true),
            logged: std::sync::atomic::AtomicUsize::new(0),
            dirty: Mutex::default(),
            positions: Mutex::default(),
        }))
    }
    fn read(
        directory: &SecureNearbyDestination,
        name: &std::ffi::OsStr,
    ) -> Result<Receipt, String> {
        if name.as_bytes().len() != 69
            || !name.as_bytes().ends_with(b".json")
            || !name
                .as_bytes()
                .get(..64)
                .is_some_and(|prefix| prefix.iter().all(u8::is_ascii_hexdigit))
        {
            return Err("invalid clipboard receipt name".into());
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(directory.anchored_root().join(name))
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let owner = rustix::process::geteuid().as_raw();
        if !metadata.is_file()
            || metadata.len() > MAX_RECEIPT_BYTES
            || metadata.uid() != owner
            || metadata.mode() & 0o077 != 0
        {
            return Err("invalid clipboard journal file".into());
        }
        let mut receipt: Receipt = serde_json::from_reader(file.take(MAX_RECEIPT_BYTES + 1))
            .map_err(|error| error.to_string())?;
        if receipt.version == CHUNKED_VERSION {
            Self::read_chunks(directory, name, &mut receipt)?;
        }
        Self::replay_log(directory, name, &mut receipt)?;
        let restore_shape = receipt
            .items
            .iter()
            .filter(|item| item.restore_info.is_some())
            .count();
        if !matches!(receipt.version, 1 | 2 | CHUNKED_VERSION)
            || (receipt.version != 2 && restore_shape != 0)
            || (receipt.version != CHUNKED_VERSION && !receipt.chunks.is_empty())
            || (receipt.version == 2
                && (receipt.items.len() != 1 || restore_shape != 1 || !receipt.cut))
            || name != std::ffi::OsStr::new(&receipt.filename()?)
        {
            return Err("invalid clipboard journal receipt".into());
        }
        Ok(receipt)
    }
    /// Read a chunked receipt's items from its chunk files, in order.
    fn read_chunks(
        directory: &SecureNearbyDestination,
        name: &std::ffi::OsStr,
        receipt: &mut Receipt,
    ) -> Result<(), String> {
        if !receipt.items.is_empty() {
            return Err("invalid clipboard journal receipt".into());
        }
        let name = name.to_str().ok_or("invalid clipboard receipt name")?;
        for (index, generation) in receipt.chunks.iter().enumerate() {
            let chunk =
                chunk_name(name, index, *generation).ok_or("invalid clipboard receipt name")?;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(directory.anchored_root().join(chunk))
                .map_err(|error| error.to_string())?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file()
                || metadata.len() > MAX_RECEIPT_BYTES
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err("invalid clipboard journal chunk".into());
            }
            let items: Vec<Item> = serde_json::from_reader(file.take(MAX_RECEIPT_BYTES + 1))
                .map_err(|error| error.to_string())?;
            if items.is_empty() || items.len() > CHUNK_ITEMS {
                return Err("invalid clipboard journal chunk".into());
            }
            receipt.items.extend(items);
        }
        Ok(())
    }

    /// Apply the item steps logged since `receipt` was written whole. A torn
    /// last line (an append cut short) ends the replay; lines of an older
    /// generation are already folded in and are skipped.
    fn replay_log(
        directory: &SecureNearbyDestination,
        name: &std::ffi::OsStr,
        receipt: &mut Receipt,
    ) -> Result<(), String> {
        let Some(log) = name.to_str().and_then(log_name) else {
            return Ok(());
        };
        let file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(directory.anchored_root().join(&log))
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.len() > MAX_LOG_BYTES
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err("invalid clipboard journal log".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_LOG_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            let Some(line) = line.strip_suffix(b"\n") else {
                break;
            };
            let Ok(step) = serde_json::from_slice::<LogLine>(line) else {
                break;
            };
            if step.generation == receipt.generation
                && let Some(item) = receipt.items.get_mut(step.index)
            {
                *item = step.item;
            }
        }
        Ok(())
    }

    fn load(directory: &SecureNearbyDestination) -> Result<Vec<Receipt>, String> {
        let pending = super::trash::private_directory(directory, "pending")?;
        let mut receipts = Vec::new();
        for (index, entry) in fs::read_dir(pending.anchored_root())
            .map_err(|error| error.to_string())?
            .enumerate()
        {
            if index >= MAX_OPERATIONS {
                return Err("clipboard recovery exceeds its pending-intent bound".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            receipts.push(Self::read(directory, &entry.file_name())?);
        }
        Ok(receipts)
    }

    pub(super) fn recover(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Undoable)?;
        let directory = self
            .directory()?
            .ok_or("durable clipboard receipt unavailable")?;
        let receipts = Self::load(&directory)?;
        let mut operations = Vec::new();
        let mut completed = 0usize;
        let mut error = None;
        for receipt in receipts {
            if receipt.workspace == context.workspace()
                && receipt.window == context.window()
                && receipt.owner == context.owner()
                && receipt
                    .items
                    .iter()
                    .all(|item| item.undone || item.staged.is_none())
            {
                let pending = PendingReceipt {
                    directory: Some(
                        self.directory()?
                            .ok_or("durable clipboard receipt unavailable")?,
                    ),
                    receipt: Mutex::new(receipt),
                    published: AtomicBool::new(true),
                    logged: std::sync::atomic::AtomicUsize::new(0),
                    dirty: Mutex::default(),
                    positions: Mutex::default(),
                };
                pending.change(|receipt| {
                    receipt.settled = true;
                    receipt.error = None;
                    Ok(())
                })?;
                continue;
            }
            if receipt.workspace != context.workspace()
                || receipt.window != context.window()
                || receipt.owner != context.owner()
                || receipt
                    .items
                    .iter()
                    .all(|item| item.undone || item.staged.is_none())
                || (receipt.settled
                    && receipt.error.is_none()
                    && !receipt.items.iter().any(|item| {
                        item.undo_recovery.is_some() || item.redoing || item.moving.is_some()
                    }))
            {
                continue;
            }
            let operation = receipt.operation.clone();
            let pending = PendingReceipt {
                directory: Some(
                    self.directory()?
                        .ok_or("durable clipboard receipt unavailable")?,
                ),
                receipt: Mutex::new(receipt),
                published: AtomicBool::new(true),
                logged: std::sync::atomic::AtomicUsize::new(0),
                dirty: Mutex::default(),
                positions: Mutex::default(),
            };
            match pending.undo(tree, context)? {
                Reply::Settled {
                    error: None,
                    undone: true,
                    ..
                } => {
                    completed = completed.saturating_add(1);
                    operations.push(operation);
                }
                Reply::Settled { error: reason, .. } => {
                    error =
                        Some(reason.unwrap_or_else(|| "clipboard recovery did not settle".into()));
                    break;
                }
                _ => return Err("clipboard recovery returned an unrelated receipt".into()),
            }
        }
        Ok(Reply::Recovered {
            operations,
            completed,
            error,
        })
    }
}

pub(super) struct PendingReceipt {
    directory: Option<SecureNearbyDestination>,
    receipt: Mutex<Receipt>,
    published: AtomicBool,
    /// Item steps appended to the log since the receipt was written whole.
    logged: std::sync::atomic::AtomicUsize,
    /// The chunks those steps changed: the next whole write rewrites only
    /// these (and the header).
    dirty: Mutex<std::collections::BTreeSet<usize>>,
    /// Where each item is, by its original path, once looked up.
    positions: Mutex<Option<HashMap<Vec<u8>, usize>>>,
}
impl PendingReceipt {
    fn change(
        &self,
        change: impl FnOnce(&mut Receipt) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut receipt = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?;
        change(&mut receipt)?;
        // Any item may have changed: every chunk is rewritten.
        self.persist(&mut receipt, true)
    }

    /// Write the receipt whole in a new generation: its header, and its
    /// chunks (all of them, or those changed since the last write). The
    /// header's rename is the commit point; what it supersedes goes after.
    fn persist(&self, receipt: &mut Receipt, all: bool) -> Result<(), String> {
        let Some(directory) = &self.directory else {
            return Err("durable clipboard storage unavailable".into());
        };
        let name = receipt.filename()?;
        // A new generation: log lines written against the last one are
        // folded into this write and never replayed over it.
        receipt.generation = receipt.generation.wrapping_add(1);
        let mut superseded = Vec::new();
        if receipt.version == CHUNKED_VERSION {
            let count = receipt.items.len().div_ceil(CHUNK_ITEMS);
            let previous = std::mem::take(&mut receipt.chunks);
            let dirty = std::mem::take(
                &mut *self
                    .dirty
                    .lock()
                    .map_err(|_| "clipboard journal unavailable")?,
            );
            for index in 0..count {
                let kept = previous
                    .get(index)
                    .copied()
                    .filter(|_| !all && !dirty.contains(&index));
                if let Some(generation) = kept {
                    receipt.chunks.push(generation);
                    continue;
                }
                let items = receipt
                    .items
                    .chunks(CHUNK_ITEMS)
                    .nth(index)
                    .ok_or("clipboard journal chunk unavailable")?;
                let bytes = serde_json::to_vec(items).map_err(|error| error.to_string())?;
                let chunk = chunk_name(&name, index, receipt.generation)
                    .ok_or("invalid clipboard receipt name")?;
                write_private(directory, &chunk, &bytes).map_err(|error| error.to_string())?;
                receipt.chunks.push(receipt.generation);
                if let Some(old) = previous.get(index) {
                    superseded.extend(chunk_name(&name, index, *old));
                }
            }
        }
        let items = if receipt.version == CHUNKED_VERSION {
            std::mem::take(&mut receipt.items)
        } else {
            Vec::new()
        };
        let encoded = serde_json::to_vec(&*receipt);
        if receipt.version == CHUNKED_VERSION {
            receipt.items = items;
        }
        let bytes = encoded.map_err(|error| error.to_string())?;
        if u64::try_from(bytes.len()).map_err(|error| error.to_string())? > MAX_RECEIPT_BYTES {
            return Err("clipboard journal receipt exceeds its bound".into());
        }
        let temporary = format!(".pending-{}", uuid::Uuid::new_v4());
        let path = directory.anchored_root().join(&temporary);
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            if self.published.load(Ordering::Acquire) {
                fs::rename(&path, directory.anchored_root().join(&name))?;
            } else {
                rename_entry(
                    directory,
                    std::ffi::OsStr::new(&temporary),
                    directory,
                    std::ffi::OsStr::new(&name),
                )
                .map_err(io::Error::other)?;
                self.published.store(true, Ordering::Release);
            }
            fs::File::open(directory.anchored_root())?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result.map_err(|error: io::Error| error.to_string())?;
        // Whether these go or not: nothing names them any more.
        for old in superseded.into_iter().chain(log_name(&name)) {
            let _ = fs::remove_file(directory.anchored_root().join(old));
        }
        self.logged.store(0, Ordering::Release);
        update_pending(directory, &name, receipt)
    }

    /// Where the item for `source` is in `receipt`.
    fn position(&self, receipt: &Receipt, source: &Source) -> Result<usize, String> {
        let mut guard = self
            .positions
            .lock()
            .map_err(|_| "clipboard journal unavailable")?;
        let positions = guard.get_or_insert_with(|| {
            receipt
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| (item.original.clone(), index))
                .collect()
        });
        let found = positions.get(source.path.as_os_str().as_bytes()).copied();
        drop(guard);
        found.ok_or_else(|| "clipboard journal source unavailable".into())
    }

    /// Change one item: appended to the receipt's log as the item's new
    /// state, and folded into a whole write every [`LOG_COMPACT`] steps.
    fn item_change(
        &self,
        source: &Source,
        change: impl FnOnce(&mut Item) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut receipt = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?;
        let index = self.position(&receipt, source)?;
        let active = pending_active(&receipt);
        change(
            receipt
                .items
                .get_mut(index)
                .ok_or("clipboard journal item unavailable")?,
        )?;
        self.dirty
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .insert(index / CHUNK_ITEMS);
        let logged = self.logged.load(Ordering::Acquire);
        let directory = match &self.directory {
            Some(directory) if self.published.load(Ordering::Acquire) && logged < LOG_COMPACT => {
                directory
            }
            _ => return self.persist(&mut receipt, false),
        };
        let name = receipt.filename()?;
        let log = log_name(&name).ok_or("invalid clipboard receipt name")?;
        let mut line = serde_json::to_vec(&LogLine {
            generation: receipt.generation,
            index,
            item: receipt
                .items
                .get(index)
                .cloned()
                .ok_or("clipboard journal item unavailable")?,
        })
        .map_err(|error| error.to_string())?;
        line.push(b'\n');
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.anchored_root().join(&log))?;
            if file.metadata()?.len().saturating_add(line.len() as u64) > MAX_LOG_BYTES {
                return Err(io::Error::other("clipboard journal log full"));
            }
            file.write_all(&line)?;
            file.sync_data()?;
            if logged == 0 {
                fs::File::open(directory.anchored_root())?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            // Fold the step into a whole write instead.
            return self.persist(&mut receipt, false);
        }
        self.logged
            .store(logged.saturating_add(1), Ordering::Release);
        let result = if pending_active(&receipt) == active {
            Ok(())
        } else {
            update_pending(directory, &name, &receipt)
        };
        drop(receipt);
        result
    }
    pub(super) fn consume_restore_info(
        &self,
        source: &Source,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let info = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .items
            .iter()
            .find(|item| item.original == source.path.as_os_str().as_bytes())
            .ok_or("clipboard source unavailable")?
            .restore_info
            .clone();
        if let Some(info) = info {
            info.remove(tree, context)?;
        }
        Ok(())
    }

    pub(super) fn restore_source_info(
        &self,
        source: &Source,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let index = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .items
            .iter()
            .position(|item| item.original == source.path.as_os_str().as_bytes())
            .ok_or("clipboard source unavailable")?;
        self.restore_metadata(index, tree, context)
    }

    fn restore_metadata(
        &self,
        index: usize,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let info = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .items
            .get(index)
            .ok_or("clipboard item unavailable")?
            .restore_info
            .clone();
        if let Some(mut info) = info {
            info.restore(tree, context)?;
            self.change(|receipt| {
                receipt
                    .items
                    .get_mut(index)
                    .ok_or("clipboard item unavailable")?
                    .restore_info = Some(info);
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(super) fn staged(&self, source: &Source, path: &Path, seal: Seal) -> Result<(), String> {
        self.item_change(source, |item| {
            item.staged = Some((path.as_os_str().as_bytes().into(), seal));
            Ok(())
        })
    }
    pub(super) fn quarantine(&self, source: &Source, path: &Path) -> Result<(), String> {
        self.item_change(source, |item| {
            item.quarantine = Some(path.as_os_str().as_bytes().into());
            Ok(())
        })
    }
    pub(super) fn publishing(&self, source: &Source, path: &Path) -> Result<(), String> {
        self.item_change(source, |item| {
            item.destination = Some(path.as_os_str().as_bytes().into());
            Ok(())
        })
    }
    pub(super) fn published(&self, source: &Source, path: &Path) -> Result<(), String> {
        self.item_change(source, |item| {
            item.destination = Some(path.as_os_str().as_bytes().into());
            item.published = true;
            Ok(())
        })
    }
    pub(super) fn settled(&self, error: Option<String>) -> Result<(), String> {
        self.change(|receipt| {
            receipt.error = error;
            receipt.settled = true;
            Ok(())
        })
    }
}

fn decode_path(bytes: &[u8]) -> Result<PathBuf, String> {
    if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
        return Err("invalid saved clipboard path".into());
    }
    let path = PathBuf::from(OsString::from_vec(bytes.into()));
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("saved clipboard path is not absolute".into());
    }
    Ok(path)
}

/// Sync the already copied tree before a Cut is allowed to delete its source.
/// Links remain data; every filesystem entry is reached through a retained fd.
pub(super) fn sync_staged(
    entry: &File,
    check: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut remaining = MAX_RETAINED_DESCRIPTORS;
    sync_entry(entry, check, &mut remaining, 0)
}
fn sync_entry(
    entry: &File,
    check: &mut impl FnMut() -> io::Result<()>,
    remaining: &mut usize,
    depth: usize,
) -> io::Result<()> {
    check()?;
    if *remaining == 0 || depth > 256 {
        return Err(io::Error::other(
            "clipboard durable tree exceeds its entry or depth bound",
        ));
    }
    *remaining = remaining.saturating_sub(1);
    let metadata = entry.metadata()?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(io::Error::other("unsupported clipboard recovery object"));
    }
    // This magic link names our still-owned descriptor, not a caller path.
    let path = PathBuf::from(format!("/proc/self/fd/{}", entry.as_raw_fd()));
    let opened = File::open(&path)?;
    if metadata.is_dir() {
        for child in fs::read_dir(&path)? {
            let child = child?;
            let name = std::ffi::CString::new(child.file_name().as_bytes())?;
            // SAFETY: still-retained directory and a terminated single leaf.
            // O_PATH|O_NOFOLLOW retains a link itself without opening its target.
            let fd = unsafe {
                libc::openat(
                    entry.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat transferred this new descriptor.
            let child = unsafe { File::from_raw_fd(fd) };
            sync_entry(&child, check, remaining, depth.saturating_add(1))?;
        }
    }
    check()?;
    opened.sync_all()
}

impl PendingReceipt {
    pub(super) fn answer(&self) -> Result<Reply, String> {
        let receipt = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?;
        Ok(Reply::Settled {
            operation: receipt.operation.clone(),
            completed: receipt.items.iter().filter(|item| item.published).count(),
            error: receipt.error.clone().or_else(|| {
                (!receipt.settled)
                    .then(|| "transfer interrupted; recover it before pasting again".into())
            }),
            undone: receipt
                .items
                .iter()
                .all(|item| item.undone || item.staged.is_none()),
        })
    }
    pub(super) fn undo_intent(
        &self,
        source: &Source,
        recovery: &Path,
        metadata: &fs::Metadata,
    ) -> Result<(), String> {
        self.item_change(source, |item| {
            if let Some((_, seal)) = &mut item.staged {
                *seal = Seal::of(metadata);
            }
            item.undo_recovery = Some(recovery.as_os_str().as_bytes().into());
            Ok(())
        })
    }
    pub(super) fn trash_intent(&self, source: &Source, trash: &Path) -> Result<(), String> {
        self.item_change(source, |item| {
            item.trash = Some(trash.as_os_str().as_bytes().into());
            Ok(())
        })
    }
    pub(super) fn undone(&self, source: &Source) -> Result<(), String> {
        self.item_change(source, |item| {
            item.undone = true;
            Ok(())
        })
    }
    pub(super) fn needs_resume(&self) -> Result<bool, String> {
        let receipt = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?;
        Ok(receipt.items.iter().any(|item| {
            !item.undone && item.undo_recovery.is_some() || item.redoing || item.moving.is_some()
        }))
    }
    fn resume_moves(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let count = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .items
            .len();
        for index in 0..count {
            relocate::Mover {
                receipt: self,
                index,
                tree,
                context,
            }
            .resume()?;
        }
        Ok(())
    }
    pub(super) fn has_active_move(&self, source: &Source) -> Result<bool, String> {
        let receipt = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?;
        Ok(receipt.items.iter().any(|item| {
            item.original == source.path.as_os_str().as_bytes() && item.moving.is_some()
        }))
    }
    pub(super) fn relocate_source(
        &self,
        source: &Source,
        paths: (&Path, &Path),
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let index = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .items
            .iter()
            .position(|item| item.original == source.path.as_os_str().as_bytes())
            .ok_or("clipboard item unavailable")?;
        relocate::Mover {
            receipt: self,
            index,
            tree,
            context,
        }
        .move_entry(paths.0, paths.1)
    }
    pub(super) fn redo(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Undoable)?;
        self.resume_moves(tree, context)?;
        let receipt = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .clone();
        if receipt.workspace != context.workspace() || receipt.window != context.window() {
            return Err("saved transfer belongs to another receiving record".into());
        }
        let mut error = None;
        for (index, item) in receipt.items.iter().enumerate() {
            if !item.published {
                continue;
            }
            if let Err(reason) = self.redo_item(&receipt, index, tree, context) {
                error = Some(reason);
                break;
            }
        }
        self.change(|receipt| {
            receipt.error.clone_from(&error);
            receipt.settled = true;
            Ok(())
        })?;
        let mut reply = self.answer()?;
        if let Reply::Settled { error: actual, .. } = &mut reply {
            *actual = error;
        }
        Ok(reply)
    }
    fn redo_item(
        &self,
        receipt: &Receipt,
        index: usize,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let item = receipt
            .items
            .get(index)
            .ok_or("clipboard item unavailable")?;
        let destination = receipt.destination.open(tree, context)?;
        let target = decode_path(
            item.destination
                .as_ref()
                .ok_or("saved destination unavailable")?,
        )?;
        if target.parent() != Some(decode_path(&receipt.destination.path)?.as_path()) {
            return Err("saved destination escaped its receiving folder".into());
        }
        let name = target.file_name().ok_or("saved destination has no name")?;
        let (_, seal) = item
            .staged
            .as_ref()
            .ok_or("saved restoration object unavailable")?;
        if let Some(entry) = optional_entry(&destination, name)? {
            if !seal.matches(&entry.metadata().map_err(|error| error.to_string())?) {
                return Err("Redo destination name is occupied by another object".into());
            }
            if !item.redoing && item.undone {
                return Err("Redo destination unexpectedly contains the retained object".into());
            }
        } else {
            if !item.undone && !item.redoing {
                return Err("transfer is not awaiting Redo".into());
            }
            if item.undone
                && !item.redoing
                && let Some(info) = &item.restore_info
            {
                info.check(tree, context)?;
            }
            let (material, leaf, material_path) = redo_material(receipt, item, tree, context)?;
            let entry = optional_entry(&material, &leaf)?.ok_or("Redo object no longer exists")?;
            if !seal.matches(&entry.metadata().map_err(|error| error.to_string())?) {
                return Err("Redo object was replaced or changed ambiguously".into());
            }
            // Write intent before moving the exact object. A retry either
            // observes that object at the destination or moves it once.
            self.change(|receipt| {
                receipt
                    .items
                    .get_mut(index)
                    .ok_or("clipboard item unavailable")?
                    .redoing = true;
                receipt
                    .items
                    .get_mut(index)
                    .ok_or("clipboard item unavailable")?
                    .undone = false;
                Ok(())
            })?;
            context.authorize(Permission::Undoable)?;
            receipt.destination.open(tree, context)?;
            if receipt.cut {
                item.parent.open(tree, context)?;
            }
            relocate::Mover {
                receipt: self,
                index,
                tree,
                context,
            }
            .move_entry(&material_path, &target)?;
            File::open(destination.anchored_root())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
            File::open(material.anchored_root())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
        }
        if !receipt.cut
            && let Some(path) = &item.trash
        {
            super::trash::consume_info(&decode_path(path)?)?;
        }
        if let Some(info) = &item.restore_info {
            info.remove(tree, context)?;
        }
        let metadata = destination
            .entry(name)?
            .metadata()
            .map_err(|error| error.to_string())?;
        self.change(|receipt| {
            let item = receipt
                .items
                .get_mut(index)
                .ok_or("clipboard item unavailable")?;
            item.staged = Some((target.as_os_str().as_bytes().into(), Seal::of(&metadata)));
            item.undone = false;
            item.redoing = false;
            item.undo_recovery = None;
            item.trash = None;
            Ok(())
        })
    }
    pub(super) fn undo(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Undoable)?;
        self.resume_moves(tree, context)?;
        let receipt = self
            .receipt
            .lock()
            .map_err(|_| "clipboard journal unavailable")?
            .clone();
        if receipt.workspace != context.workspace() || receipt.window != context.window() {
            return Err("saved transfer belongs to another receiving record".into());
        }
        let mut error = None;
        for (index, item) in receipt.items.iter().enumerate().rev() {
            if item.undone || item.staged.is_none() {
                continue;
            }
            if let Err(reason) = self
                .undo_item(&receipt, index, tree, context)
                .and_then(|()| self.recover_quarantine(&receipt, index, tree, context))
                .and_then(|()| self.restore_metadata(index, tree, context))
            {
                error = Some(reason);
                break;
            }
            self.change(|receipt| {
                receipt
                    .items
                    .get_mut(index)
                    .ok_or("clipboard item unavailable")?
                    .undone = true;
                Ok(())
            })?;
        }
        self.change(|receipt| {
            receipt.error.clone_from(&error);
            receipt.settled = true;
            Ok(())
        })?;
        let mut reply = self.answer()?;
        if let Reply::Settled { error: actual, .. } = &mut reply {
            *actual = error;
        }
        Ok(reply)
    }
    fn recover_quarantine(
        &self,
        receipt: &Receipt,
        index: usize,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let item = receipt
            .items
            .get(index)
            .ok_or("clipboard item unavailable")?;
        let Some(quarantine) = &item.quarantine else {
            return Ok(());
        };
        let path = decode_path(quarantine)?;
        let original = decode_path(&item.original)?;
        if path.parent() != original.parent() {
            return Err("saved quarantine escaped its original folder".into());
        }
        let source = item.parent.open(tree, context)?;
        let name = path.file_name().ok_or("saved quarantine has no name")?;
        let Some(entry) = optional_entry(&source, name)? else {
            return Ok(());
        };
        if !item
            .original_seal
            .matches(&entry.metadata().map_err(|error| error.to_string())?)
        {
            return Err("saved source quarantine was replaced or changed ambiguously".into());
        }
        // A interrupted recursive removal may leave only part of the original.
        // The fully synced stage restores the source; its leftover original is
        // preserved separately in the existing Trash rather than deleted.
        let slot = super::trash::TrashSlot::prepare(&original)?;
        self.change(|receipt| {
            receipt
                .items
                .get_mut(index)
                .ok_or("clipboard item unavailable")?
                .quarantine_trash = Some(slot.path().as_os_str().as_bytes().into());
            Ok(())
        })?;
        item.parent.open(tree, context)?;
        let target = slot.path().to_path_buf();
        slot.publish_with(&source, name, || {
            relocate::Mover {
                receipt: self,
                index,
                tree,
                context,
            }
            .move_entry(&path, &target)
        })?;
        Ok(())
    }
    fn undo_item(
        &self,
        receipt: &Receipt,
        index: usize,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let item = receipt
            .items
            .get(index)
            .ok_or("clipboard item unavailable")?;
        let destination = receipt.destination.open(tree, context)?;
        let (staged, seal) = item
            .staged
            .as_ref()
            .ok_or("saved transfer material unavailable")?;
        let original = decode_path(&item.original)?;
        let original_name = original.file_name().ok_or("saved original has no name")?;
        let source = if receipt.cut {
            Some(item.parent.open(tree, context)?)
        } else {
            None
        };
        if already_restored(item, source.as_ref(), &destination, original_name)? {
            return Ok(());
        }
        let root = decode_path(&receipt.destination.path)?;
        let (directory, name, path) = recovery_material(item, &root, &destination, staged, seal)?;
        context.authorize(Permission::Undoable)?;
        receipt.destination.open(tree, context)?;
        if let Some(source) = &source
            && optional_entry(source, original_name)?.is_none()
        {
            item.parent.open(tree, context)?;
            relocate::Mover {
                receipt: self,
                index,
                tree,
                context,
            }
            .move_entry(&path, &original)?;
            File::open(source.anchored_root())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
            File::open(directory.anchored_root())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
        let restore_to = item
            .destination
            .as_ref()
            .map(|path| decode_path(path))
            .transpose()?
            .unwrap_or_else(|| root.join(original_name));
        let slot = super::trash::TrashSlot::prepare(&restore_to)?;
        self.change(|receipt| {
            receipt
                .items
                .get_mut(index)
                .ok_or("clipboard item unavailable")?
                .trash = Some(slot.path().as_os_str().as_bytes().into());
            Ok(())
        })?;
        let target = slot.path().to_path_buf();
        slot.publish_with(&directory, &name, || {
            relocate::Mover {
                receipt: self,
                index,
                tree,
                context,
            }
            .move_entry(&path, &target)
        })?;
        // A private staging parent may now be empty. Only this exact recorded
        // private container is eligible; a receiving directory is never removed.
        if path == decode_path(staged)?
            && let Some(parent) = path.parent()
            && parent != root
            && parent.parent() == Some(root.as_path())
        {
            let _ = fs::remove_dir(parent);
        }
        drop(destination);
        Ok(())
    }
}

fn already_restored(
    item: &Item,
    source: Option<&SecureNearbyDestination>,
    destination: &SecureNearbyDestination,
    original_name: &std::ffi::OsStr,
) -> Result<bool, String> {
    let (_, seal) = item
        .staged
        .as_ref()
        .ok_or("saved transfer material unavailable")?;
    if let Some(source) = source
        && let Some(entry) = optional_entry(source, original_name)?
    {
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        if seal.matches(&metadata) {
            File::open(source.anchored_root())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
            File::open(destination.anchored_root())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
            return Ok(true);
        }
        if !item.original_seal.matches(&metadata) {
            return Err("original source name is occupied by another object".into());
        }
        // Before destructive Cut I/O, the original remains intact.
        // Keep it; the staged copy can be recovered through Trash.
    }
    if let Some(path) = &item.trash {
        let path = decode_path(path)?;
        let files = super::trash::files_directory()?;
        let expected = tilo_paths::data_dir()
            .ok_or("home Trash unavailable")?
            .join("Trash/files");
        if path.parent() != Some(expected.as_path()) {
            return Err("saved Trash scope changed".into());
        }
        if let Some(entry) = optional_entry(
            &files,
            path.file_name().ok_or("saved Trash item has no name")?,
        )? {
            if seal.matches(&entry.metadata().map_err(|error| error.to_string())?) {
                return Ok(true);
            }
            return Err("saved Trash item was replaced".into());
        }
    }
    Ok(false)
}

fn redo_material(
    receipt: &Receipt,
    item: &Item,
    tree: &WorkspaceTreeHandle,
    context: &FileClipboardContext,
) -> Result<(SecureNearbyDestination, OsString, PathBuf), String> {
    let (material, leaf) = if receipt.cut {
        let original = decode_path(&item.original)?;
        (
            item.parent.open(tree, context)?,
            original
                .file_name()
                .ok_or("saved original has no name")?
                .to_os_string(),
        )
    } else {
        let trash = decode_path(
            item.trash
                .as_ref()
                .ok_or("Copy Undo recovery is unavailable")?,
        )?;
        let expected = tilo_paths::data_dir()
            .ok_or("home Trash unavailable")?
            .join("Trash/files");
        if trash.parent() != Some(expected.as_path()) {
            return Err("saved Trash scope changed".into());
        }
        (
            super::trash::files_directory()?,
            trash
                .file_name()
                .ok_or("saved Trash item has no name")?
                .to_os_string(),
        )
    };
    let path = if receipt.cut {
        decode_path(&item.original)?
    } else {
        decode_path(item.trash.as_ref().ok_or("saved Trash item unavailable")?)?
    };
    Ok((material, leaf, path))
}

fn recovery_material(
    item: &Item,
    root: &Path,
    destination: &SecureNearbyDestination,
    staged: &[u8],
    seal: &Seal,
) -> Result<(SecureNearbyDestination, OsString, PathBuf), String> {
    let candidates = item
        .undo_recovery
        .iter()
        .chain(item.destination.iter())
        .map(Vec::as_slice)
        .chain(std::iter::once(staged));
    let mut material = None;
    for candidate in candidates {
        let path = decode_path(candidate)?;
        if !path.starts_with(root) {
            return Err("saved recovery escaped its destination".into());
        }
        let parent = path.parent().ok_or("saved material has no parent")?;
        let name = path.file_name().ok_or("saved material has no name")?;
        if parent != root {
            if parent.parent() != Some(root) {
                return Err("saved recovery is not a direct private child".into());
            }
            if optional_entry(
                destination,
                parent
                    .file_name()
                    .ok_or("saved recovery parent has no name")?,
            )?
            .is_none()
            {
                continue;
            }
        }
        let directory = SecureNearbyDestination::open_beneath(root, parent)?;
        let Some(entry) = optional_entry(&directory, name)? else {
            continue;
        };
        if !seal.matches(&entry.metadata().map_err(|error| error.to_string())?) {
            return Err("saved transfer material was replaced or changed ambiguously".into());
        }
        material = Some((directory, name.to_os_string(), path));
        break;
    }
    material.ok_or_else(|| "saved transfer material is unavailable".into())
}

/// Only ENOENT means absent. Permission, descriptor and storage failures must
/// refuse recovery instead of pretending its subject could not be observed.
pub(super) fn optional_entry(
    directory: &SecureNearbyDestination,
    name: &std::ffi::OsStr,
) -> Result<Option<File>, String> {
    if Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err("recovery entry is not one leaf".into());
    }
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|error| error.to_string())?;
    // SAFETY: retained directory and terminated single leaf. O_PATH and
    // O_NOFOLLOW retain a symlink as data instead of opening its target.
    let fd = unsafe {
        libc::openat(
            directory.directory_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error.to_string())
        };
    }
    // SAFETY: successful openat transferred this new descriptor.
    Ok(Some(unsafe { File::from_raw_fd(fd) }))
}

fn receipt_name(workspace: &str, window: &str, operation: &str) -> Result<String, String> {
    let namespace =
        serde_json::to_vec(&(workspace, window, operation)).map_err(|error| error.to_string())?;
    Ok(format!("{}.json", blake3::hash(&namespace)))
}

/// Completed receipts remain addressable for history; only unresolved intents
/// enter the bounded recovery index. Its marker is synced before file I/O.
fn update_pending(
    directory: &SecureNearbyDestination,
    name: &str,
    receipt: &Receipt,
) -> Result<(), String> {
    let pending = super::trash::private_directory(directory, "pending")?;
    let active = pending_active(receipt);
    let leaf = std::ffi::OsStr::new(name);
    if active {
        if let Some(entry) = optional_entry(&pending, leaf)? {
            let metadata = entry.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file()
                || metadata.len() != 0
                || !tilo_files::native::private::private_to_me(&metadata)
            {
                return Err("invalid clipboard pending marker".into());
            }
        } else {
            let entries =
                fs::read_dir(pending.anchored_root()).map_err(|error| error.to_string())?;
            let mut count = 0usize;
            for entry in entries.take(MAX_OPERATIONS) {
                entry.map_err(|error| error.to_string())?;
                count = count.saturating_add(1);
            }
            if count >= MAX_OPERATIONS {
                return Err("recover interrupted transfers before starting another".into());
            }
            let marker = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(pending.anchored_root().join(leaf))
                .map_err(|error| error.to_string())?;
            marker.sync_all().map_err(|error| error.to_string())?;
        }
    } else {
        match fs::remove_file(pending.anchored_root().join(leaf)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    File::open(pending.anchored_root())
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod chunk_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn item(dir: &Path, index: usize) -> Item {
        let seal = Seal::of(&fs::metadata(dir).unwrap());
        Item {
            original: format!("{}/f{index:05}.jpg", dir.display()).into_bytes(),
            parent: Folder {
                path: dir.as_os_str().as_bytes().into(),
                root: dir.as_os_str().as_bytes().into(),
                root_identity: seal.clone(),
                identity: seal.clone(),
            },
            original_seal: seal,
            staged: None,
            quarantine: None,
            quarantine_trash: None,
            destination: None,
            published: false,
            undo_recovery: None,
            trash: None,
            undone: false,
            redoing: false,
            moving: None,
            restore_info: None,
        }
    }

    /// A paste of thousands of entries keeps a receipt of bounded files: a
    /// header and chunks, of which a whole write rewrites only those that
    /// changed, and steps between writes replay from the log.
    #[test]
    fn a_receipt_of_any_size_is_a_header_plus_chunks() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = SecureNearbyDestination::open_beneath(root.path(), root.path()).unwrap();
        let seal = Seal::of(&fs::metadata(root.path()).unwrap());
        let count = 5_000;
        let receipt = Receipt {
            version: CHUNKED_VERSION,
            workspace: "w".into(),
            owner: "o".into(),
            window: "win".into(),
            operation: uuid::Uuid::new_v4().to_string(),
            cut: false,
            destination: Folder {
                path: root.path().as_os_str().as_bytes().into(),
                root: root.path().as_os_str().as_bytes().into(),
                root_identity: seal.clone(),
                identity: seal,
            },
            items: (0..count).map(|index| item(root.path(), index)).collect(),
            error: None,
            settled: false,
            generation: 0,
            chunks: Vec::new(),
        };
        let name = receipt.filename().unwrap();
        let pending = PendingReceipt {
            directory: Some(directory),
            receipt: Mutex::new(receipt),
            published: AtomicBool::new(false),
            logged: std::sync::atomic::AtomicUsize::new(0),
            dirty: Mutex::default(),
            positions: Mutex::default(),
        };
        pending.change(|_| Ok(())).unwrap();
        let chunks = count.div_ceil(CHUNK_ITEMS);
        for index in 0..chunks {
            let file = root.path().join(chunk_name(&name, index, 1).unwrap());
            assert!(fs::metadata(&file).unwrap().len() <= MAX_RECEIPT_BYTES);
        }
        assert!(
            fs::metadata(root.path().join(&name)).unwrap().len() < 64 * 1024,
            "the header stays small"
        );

        // One item changes: the next whole write rewrites only its chunk.
        {
            let mut receipt = pending.receipt.lock().unwrap();
            receipt.items[300].published = true;
            pending.dirty.lock().unwrap().insert(300 / CHUNK_ITEMS);
            pending.persist(&mut receipt, false).unwrap();
            drop(receipt);
        }
        assert!(
            root.path().join(chunk_name(&name, 0, 1).unwrap()).exists(),
            "untouched chunk kept"
        );
        assert!(root.path().join(chunk_name(&name, 1, 2).unwrap()).exists());
        assert!(
            !root.path().join(chunk_name(&name, 1, 1).unwrap()).exists(),
            "superseded chunk gone"
        );

        // A step logged since: replayed on read; a torn last line is ignored.
        let mut step = item(root.path(), 7);
        step.undone = true;
        let mut log = serde_json::to_vec(&LogLine {
            generation: 2,
            index: 7,
            item: step,
        })
        .unwrap();
        log.extend_from_slice(b"\n{\"generation\":2,\"ind");
        fs::write(root.path().join(log_name(&name).unwrap()), log).unwrap();
        fs::set_permissions(
            root.path().join(log_name(&name).unwrap()),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let read = Journal::read(
            pending.directory.as_ref().unwrap(),
            std::ffi::OsStr::new(&name),
        )
        .unwrap();
        assert_eq!(read.items.len(), count);
        assert!(read.items[300].published);
        assert!(read.items[7].undone);
        assert!(!read.items[8].undone);
    }
}
