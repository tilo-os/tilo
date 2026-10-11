//! Durable write-ahead clipboard receipts. Saved paths never grant authority.
use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::OpenOptionsExt as _;

mod relocate;

const MAX_RECEIPT_BYTES: u64 = 2 * 1024 * 1024;

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
                    &source.entry.metadata().map_err(|error| error.to_string())?,
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
                1
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
        };
        let pending = Arc::new(PendingReceipt {
            directory: self.directory()?,
            receipt: Mutex::new(receipt),
            published: AtomicBool::new(false),
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
        // SAFETY: geteuid reads the process identity without pointers.
        let owner = unsafe { libc::geteuid() };
        if !metadata.is_file()
            || metadata.len() > MAX_RECEIPT_BYTES
            || metadata.uid() != owner
            || metadata.mode() & 0o077 != 0
        {
            return Err("invalid clipboard journal file".into());
        }
        let receipt: Receipt = serde_json::from_reader(file.take(MAX_RECEIPT_BYTES + 1))
            .map_err(|error| error.to_string())?;
        let restore_shape = receipt
            .items
            .iter()
            .filter(|item| item.restore_info.is_some())
            .count();
        if !matches!(receipt.version, 1 | 2)
            || (receipt.version == 1 && restore_shape != 0)
            || (receipt.version == 2
                && (receipt.items.len() != 1 || restore_shape != 1 || !receipt.cut))
            || name != std::ffi::OsStr::new(&receipt.filename()?)
            || receipt.items.len() > runtime_collab::file_clipboard::MAX_ENTRIES
        {
            return Err("invalid clipboard journal receipt".into());
        }
        Ok(receipt)
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
        let Some(directory) = &self.directory else {
            drop(receipt);
            return Err("durable clipboard storage unavailable".into());
        };
        let bytes = serde_json::to_vec(&*receipt).map_err(|error| error.to_string())?;
        if u64::try_from(bytes.len()).map_err(|error| error.to_string())? > MAX_RECEIPT_BYTES {
            return Err("clipboard journal receipt exceeds its bound".into());
        }
        let name = receipt.filename()?;
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
        let result = update_pending(directory, &name, &receipt);
        drop(receipt);
        result
    }
    fn item_change(
        &self,
        source: &Source,
        change: impl FnOnce(&mut Item) -> Result<(), String>,
    ) -> Result<(), String> {
        self.change(|receipt| {
            let item = receipt
                .items
                .iter_mut()
                .find(|item| item.original == source.path.as_os_str().as_bytes())
                .ok_or("clipboard journal source unavailable")?;
            change(item)
        })
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
    let active = (!receipt.settled || receipt.error.is_some())
        && receipt.items.iter().any(|item| !item.undone)
        || receipt.items.iter().any(|item| {
            !item.undone && (item.undo_recovery.is_some() || item.redoing || item.moving.is_some())
        });
    let leaf = std::ffi::OsStr::new(name);
    if active {
        if let Some(entry) = optional_entry(&pending, leaf)? {
            let metadata = entry.metadata().map_err(|error| error.to_string())?;
            // SAFETY: geteuid only reads this process's effective identity.
            if !metadata.is_file()
                || metadata.len() != 0
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
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
