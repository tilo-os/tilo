//! Filesystem changes as the Files document records them.
//!
//! Creating a folder, renaming, moving to Trash, moving and copying files,
//! writing a file and packing files into an archive are
//! the Files resource's verbs: each queues a [`FileOp`] in the document's
//! outbox ([`crate::resource::FileExplorer::ops`]) for the device whose
//! filesystem it changes, which carries it out and settles it with what was
//! done ([`Outcome`]). Being document actions, they are in the document's
//! history and undoable: undoing one queues a [`FileOp::Revert`], which the
//! same device carries out by compensating each recorded [`Change`] in
//! reverse order — renaming back, moving back, restoring from Trash, removing
//! the created folder, moving a copy or a written file to Trash.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ui::lock::MutexRecover as _;

pub use crate::cloud_transfer::TransferProgress;
use tilo_cloud_files::cloud::ImportEntry;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::Settled;

use crate::fileops;

/// One filesystem change a person or an agent asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileOp {
    /// Create the folder `name` in `parent`.
    NewFolder { parent: String, name: String },
    /// Rename `path` to `name`, in the same folder.
    Rename { path: String, name: String },
    /// Move `paths` to Trash.
    Trash { paths: Vec<String> },
    /// Move `paths` into `folder`, keeping their names; create `folder` first
    /// when `create` and it does not exist.
    Move {
        paths: Vec<String>,
        folder: String,
        create: bool,
    },
    /// Copy `paths` into `folder`; a copy whose name is taken gets a free
    /// one.
    Copy { paths: Vec<String>, folder: String },
    /// Import independent, owner-writable copies of mounted native files.
    /// Uses the same recorded changes and undo as an ordinary copy.
    ImportMounted { paths: Vec<String>, folder: String },
    /// Import independent local copies from a cloud account.
    CloudImport {
        job_id: String,
        account: String,
        entries: Vec<ImportEntry>,
        folder: String,
        destination: crate::cloud_transfer::DestinationIdentity,
    },
    /// Write `text` as the new file `name` in `folder`, as given
    /// ([`crate::documents::bytes_for`]). With `replace`, a file already
    /// called that goes to Trash first.
    Write {
        folder: String,
        name: String,
        text: String,
        replace: bool,
    },
    /// Pack `paths` into the archive `name` (a free name) in `folder`: a
    /// tar compressed with zstd when the name ends in `.tar.zst`, else zip.
    Compress {
        paths: Vec<String>,
        folder: String,
        name: String,
    },
    /// Extract the archive `archive` into `folder`: as the one folder the
    /// archive holds at its top, else into a new folder named after it.
    /// `if_exists` settles a name `folder` already has.
    Extract {
        archive: String,
        folder: String,
        #[serde(default)]
        if_exists: IfExists,
    },
    /// A transfer the native host already carried out, by its receipt's
    /// operation. It is never carried out here; only the host compensates it.
    ClipboardReceipt { operation: String },
    /// Undo the queued operation `op`: compensate what it changed.
    Revert { op: String },
}

/// What an extraction does when its folder's name is taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IfExists {
    /// The extracted folder gets a free name, like “Photos 2”.
    #[default]
    KeepBoth,
    /// The existing item goes to Trash first; undo brings it back.
    Replace,
    /// Nothing is extracted.
    Skip,
}

/// One change an operation made, enough to take it back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// A folder was created at `path`.
    Created { path: String },
    /// `from` was renamed (or moved) to `to`.
    Moved { from: String, to: String },
    /// `from` was copied to `to`.
    Copied { from: String, to: String },
    /// An admitted cloud import produced this exact local object.
    CloudImported {
        from: String,
        to: String,
        destination: crate::cloud_transfer::DestinationIdentity,
        output: crate::cloud_transfer::DestinationIdentity,
    },
    /// The exact imported output is in Trash; retain its admission for redo.
    CloudTrashed {
        to: String,
        entry: String,
        destination: crate::cloud_transfer::DestinationIdentity,
        output: crate::cloud_transfer::DestinationIdentity,
    },
    /// `from` was moved to Trash as the Trash entry `entry` (when Files
    /// could tell which one it became).
    Trashed { from: String, entry: Option<String> },
    /// The Trash entry `entry` was put back at `to`.
    Restored { entry: String, to: String },
    /// The empty folder `path` was removed.
    Removed { path: String },
    /// The new file `path` was written.
    Wrote { path: String },
    /// The native host's transfer `operation` is in effect.
    ClipboardTransfer { operation: String },
    /// The native host took the transfer `operation` back.
    ClipboardUndo { operation: String },
}

impl Default for FileOp {
    /// Nothing to do: Trash of no paths.
    fn default() -> Self {
        Self::Trash { paths: Vec::new() }
    }
}

/// What carrying out an operation came to: every change it made, or why it
/// stopped after the ones it made.
pub type Outcome = Settled<Vec<Change>>;

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// What a local transfer does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferKind {
    #[default]
    Copy,
    Move,
    /// Extracting the archive named in [`LocalTransfer::folder`]'s place:
    /// `folder` is the archive.
    Extract,
    Compress,
}

/// A local copy, move, extraction or packing under way, as the window
/// shows it: how far it is, and whether the person asked to stop.
#[derive(Debug, Default)]
pub struct LocalTransfer {
    /// Bytes copied so far, and of how many. An extraction counts the
    /// archive's bytes read, of its size.
    pub done: std::sync::atomic::AtomicU64,
    pub total: std::sync::atomic::AtomicU64,
    /// Set by Cancel; the transfer stops at the next chunk.
    pub cancel: AtomicBool,
    /// What it does, how many items, and into which folder (for an
    /// extraction, the archive being extracted).
    pub kind: TransferKind,
    pub count: usize,
    pub folder: String,
    /// When it started, so a quick transfer never flashes a progress card.
    pub started: Option<ui::time::Instant>,
}

/// The local transfers running on this device now.
static ACTIVE: std::sync::Mutex<Vec<std::sync::Arc<LocalTransfer>>> =
    std::sync::Mutex::new(Vec::new());

/// The local copies and moves running now, oldest first.
pub fn active_transfers() -> Vec<std::sync::Arc<LocalTransfer>> {
    ACTIVE.lock_recover().clone()
}

/// Stop every running local transfer at its next chunk.
pub fn cancel_transfers() {
    for transfer in active_transfers() {
        transfer
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How many bytes copying `path` writes (symlinks count as themselves).
fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| tree_size(&entry.path()))
            .sum()
    })
}

/// Counts the bytes a transfer copies and stops it on Cancel.
struct Tracked<'a> {
    transfer: &'a LocalTransfer,
    report: &'a dyn Fn(&LocalTransfer),
    since_report: u64,
}

const CHUNK: usize = 1 << 20;
/// Report at most every this many bytes, so progress never floods the UI.
const REPORT_EVERY: u64 = 8 << 20;

impl Tracked<'_> {
    fn check(&self) -> std::io::Result<()> {
        if self
            .transfer
            .cancel
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                CANCELLED,
            ))
        } else {
            Ok(())
        }
    }
}

/// The reason a transfer the person stopped settles with.
pub const CANCELLED: &str = "cancelled";

impl fileops::TransferObserver for Tracked<'_> {
    fn before_copy_source(&mut self, _source: &Path) -> std::io::Result<()> {
        self.check()
    }

    fn copy_bytes(
        &mut self,
        source: &mut std::fs::File,
        destination: &mut std::fs::File,
    ) -> std::io::Result<u64> {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;
        let mut buffer = vec![0_u8; CHUNK];
        let mut copied = 0_u64;
        loop {
            self.check()?;
            let read = match source.read(&mut buffer) {
                Ok(0) => return Ok(copied),
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            destination.write_all(buffer.get(..read).unwrap_or_default())?;
            let read = read as u64;
            copied += read;
            self.transfer.done.fetch_add(read, Ordering::Relaxed);
            self.since_report += read;
            if self.since_report >= REPORT_EVERY {
                self.since_report = 0;
                (self.report)(self.transfer);
            }
        }
    }
}

/// Carry out a copy or move with its progress shown and Cancel honoured,
/// trying every item even when one fails; any other operation as
/// [`execute`]. `report` hears how far it is now and then, off the UI
/// thread.
pub fn execute_tracked(
    op: &FileOp,
    reverting: Option<&[Change]>,
    report: &dyn Fn(&LocalTransfer),
) -> Outcome {
    let (paths, folder, copy) = match op {
        FileOp::Copy { paths, folder } => (paths, folder, true),
        FileOp::Move {
            paths,
            folder,
            create: false,
        } => (paths, folder, false),
        FileOp::Extract {
            archive,
            folder,
            if_exists,
        } => return extract_tracked(Path::new(archive), Path::new(folder), *if_exists, report),
        FileOp::Compress {
            paths,
            folder,
            name,
        } => return compress_tracked(paths, Path::new(folder), name, report),
        _ => return execute(op, reverting),
    };
    let folder_path = Path::new(folder);
    if !folder_path.is_dir() {
        return Settled::Failed {
            reason: format!("{} is not a folder", folder_path.display()),
            partial: vec![],
        };
    }
    let transfer = std::sync::Arc::new(LocalTransfer {
        total: paths
            .iter()
            .map(|path| tree_size(Path::new(path)))
            .sum::<u64>()
            .into(),
        kind: if copy {
            TransferKind::Copy
        } else {
            TransferKind::Move
        },
        count: paths.len(),
        folder: folder.clone(),
        started: Some(ui::time::now()),
        ..LocalTransfer::default()
    });
    ACTIVE.lock_recover().push(std::sync::Arc::clone(&transfer));
    let mut observer = Tracked {
        transfer: &transfer,
        report,
        since_report: 0,
    };
    let mut changes = Vec::new();
    let mut failures = Vec::new();
    for path in paths {
        if transfer.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        match fileops::transfer_into_with(Path::new(path), folder_path, !copy, &mut observer) {
            Ok(done) => changes.push(if copy {
                Change::Copied {
                    from: path.clone(),
                    to: text(&done),
                }
            } else {
                Change::Moved {
                    from: path.clone(),
                    to: text(&done),
                }
            }),
            Err(error) => failures.push(error),
        }
    }
    ACTIVE
        .lock_recover()
        .retain(|active| !std::sync::Arc::ptr_eq(active, &transfer));
    report(&transfer);
    if transfer.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Settled::Failed {
            reason: CANCELLED.into(),
            partial: changes,
        };
    }
    match failures.first() {
        None => Settled::Done(changes),
        Some(first) => Settled::Failed {
            reason: first.clone(),
            partial: changes,
        },
    }
}

/// Carry out `op` on this device's filesystem. A revert is given the changes
/// the operation it takes back made (`None` when that operation is unknown).
pub fn execute(op: &FileOp, reverting: Option<&[Change]>) -> Outcome {
    execute_with_progress(op, reverting, &AtomicBool::new(false), |_| {})
}

/// Carry out an operation on a worker thread, with cancellable cloud imports.
/// Progress callbacks run on that same worker and must not touch the UI.
pub fn execute_with_progress(
    op: &FileOp,
    reverting: Option<&[Change]>,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
) -> Outcome {
    if matches!(
        op,
        FileOp::CloudImport { .. } | FileOp::ImportMounted { .. }
    ) {
        return Settled::Failed {
            reason: "cloud import requires a host-private source consent".into(),
            partial: vec![],
        };
    }
    let _ = (cancel, progress);
    let mut changes = Vec::new();
    let result = match op {
        FileOp::NewFolder { parent, name } => {
            fileops::new_folder(Path::new(parent), name).map(|created| {
                changes.push(Change::Created {
                    path: text(&created),
                });
            })
        }
        FileOp::Rename { path, name } => fileops::rename(Path::new(path), name).map(|renamed| {
            changes.push(Change::Moved {
                from: path.clone(),
                to: text(&renamed),
            });
        }),
        FileOp::Trash { paths } => paths
            .iter()
            .try_for_each(|path| trash(Path::new(path)).map(|change| changes.push(change))),
        FileOp::Move {
            paths,
            folder,
            create,
        } => move_into(paths, Path::new(folder), *create, &mut changes),
        FileOp::Copy { paths, folder } => paths.iter().try_for_each(|path| {
            fileops::copy_into(Path::new(path), Path::new(folder)).map(|copied| {
                changes.push(Change::Copied {
                    from: path.clone(),
                    to: text(&copied),
                });
            })
        }),
        FileOp::ImportMounted { .. } => Err("mounted import requires host-private consent".into()),
        FileOp::CloudImport { .. } => Err("cloud import requires its transfer worker".into()),
        FileOp::Write {
            folder,
            name,
            text,
            replace,
        } => write_file(Path::new(folder), name, text, *replace, &mut changes),
        FileOp::Compress {
            paths,
            folder,
            name,
        } => {
            return compress_tracked(paths, Path::new(folder), name, &|_| {});
        }
        FileOp::Extract {
            archive,
            folder,
            if_exists,
        } => {
            return extract_tracked(Path::new(archive), Path::new(folder), *if_exists, &|_| {});
        }
        FileOp::ClipboardReceipt { .. } => Err("clipboard receipt requires its native host".into()),
        FileOp::Revert { .. } => match reverting {
            Some(done) => done
                .iter()
                .rev()
                .try_for_each(|change| compensate(change).map(|done| changes.push(done))),
            None => Err("there is nothing to undo: the operation was not carried out".into()),
        },
    };
    match result {
        Ok(()) => Settled::Done(changes),
        Err(reason) => Settled::Failed {
            reason,
            partial: changes,
        },
    }
}

fn move_into(
    paths: &[String],
    folder: &Path,
    create: bool,
    changes: &mut Vec<Change>,
) -> Result<(), String> {
    if !folder.exists() {
        if !create {
            return Err(format!("{} does not exist", folder.display()));
        }
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("cannot create {}: {error}", folder.display()))?;
        changes.push(Change::Created { path: text(folder) });
    } else if !folder.is_dir() {
        return Err(format!("{} is a file, not a folder", folder.display()));
    }
    for path in paths {
        let moved = fileops::move_into(Path::new(path), folder)?;
        changes.push(Change::Moved {
            from: path.clone(),
            to: text(&moved),
        });
    }
    Ok(())
}

/// Register `transfer` as running until the returned guard drops.
struct Running(std::sync::Arc<LocalTransfer>);

impl Running {
    fn start(transfer: LocalTransfer) -> Self {
        let transfer = std::sync::Arc::new(transfer);
        ACTIVE.lock_recover().push(std::sync::Arc::clone(&transfer));
        Self(transfer)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        ACTIVE
            .lock_recover()
            .retain(|active| !std::sync::Arc::ptr_eq(active, &self.0));
    }
}

/// Show `transfer` as running for the rest of the process: a capture's
/// staged transfer, followed by the window like any other.
#[doc(hidden)]
pub fn run_for_capture(transfer: LocalTransfer) {
    ACTIVE.lock_recover().push(std::sync::Arc::new(transfer));
}

/// Free space an extraction always leaves on the disk it writes to.
const SPACE_RESERVE: u64 = 512 << 20;
/// How often, in bytes written, an extraction looks at the free space.
const SPACE_CHECK_EVERY: u64 = 64 << 20;

/// An archive operation's progress: the bytes it read reach the window and
/// the caller's job, Cancel stops it, and an extraction stops before the
/// disk it writes to is full.
struct ArchiveProgress<'a> {
    transfer: &'a LocalTransfer,
    report: &'a dyn Fn(&LocalTransfer),
    since_report: std::cell::Cell<u64>,
    /// The folder written to, and the bytes written since its free space
    /// was last looked at.
    destination: &'a Path,
    unchecked: std::cell::Cell<u64>,
}

impl<'a> ArchiveProgress<'a> {
    fn new(
        transfer: &'a LocalTransfer,
        report: &'a dyn Fn(&LocalTransfer),
        destination: &'a Path,
    ) -> Self {
        Self {
            transfer,
            report,
            since_report: std::cell::Cell::new(0),
            destination,
            // Look before the first write.
            unchecked: std::cell::Cell::new(SPACE_CHECK_EVERY),
        }
    }
}

impl crate::archive::Observer for ArchiveProgress<'_> {
    fn advance(&self, bytes: u64) {
        use std::sync::atomic::Ordering;
        self.transfer.done.fetch_add(bytes, Ordering::Relaxed);
        let since = self.since_report.get().saturating_add(bytes);
        if since >= REPORT_EVERY {
            self.since_report.set(0);
            (self.report)(self.transfer);
        } else {
            self.since_report.set(since);
        }
    }

    fn cancelled(&self) -> bool {
        self.transfer
            .cancel
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn wrote(&self, bytes: u64) -> std::io::Result<()> {
        let unchecked = self.unchecked.get().saturating_add(bytes);
        if unchecked < SPACE_CHECK_EVERY {
            self.unchecked.set(unchecked);
            return Ok(());
        }
        self.unchecked.set(0);
        match host_fs::space(self.destination) {
            Ok(space) if space.available < SPACE_RESERVE.saturating_add(SPACE_CHECK_EVERY) => {
                Err(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "there is not enough free space on the disk",
                ))
            }
            // A filesystem that cannot say how full it is still reports a
            // full disk when a write fails.
            _ => Ok(()),
        }
    }
}

/// The reason an archive operation's `error` settles with: [`CANCELLED`]
/// when the person stopped it.
fn archive_failure(what: &str, error: &std::io::Error) -> String {
    if tilo_archive::is_cancelled(error) {
        CANCELLED.into()
    } else {
        format!("{what}: {error}")
    }
}

/// Pack `paths` into a new archive called `name`, or a free name like it,
/// in `folder`, with progress and Cancel.
fn compress_tracked(
    paths: &[String],
    folder: &Path,
    name: &str,
    report: &dyn Fn(&LocalTransfer),
) -> Outcome {
    let failed = |reason: String| Settled::Failed {
        reason,
        partial: vec![],
    };
    let name = match fileops::leaf_name(name) {
        Ok(name) => name,
        Err(reason) => return failed(reason),
    };
    let archive = match fileops::unique_destination(folder, name) {
        Ok(archive) => archive,
        Err(reason) => return failed(reason),
    };
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let running = Running::start(LocalTransfer {
        total: paths.iter().map(|path| tree_size(path)).sum::<u64>().into(),
        kind: TransferKind::Compress,
        count: paths.len(),
        folder: text(folder),
        started: Some(ui::time::now()),
        ..LocalTransfer::default()
    });
    let progress = ArchiveProgress::new(&running.0, report, folder);
    let packing = crate::archive::packing_of(name);
    let packed = crate::archive::create(&paths, &archive, packing, &progress);
    report(&running.0);
    match packed {
        Ok(()) => Settled::Done(vec![Change::Wrote {
            path: text(&archive),
        }]),
        Err(error) => failed(archive_failure(
            &format!("cannot create {}", archive.display()),
            &error,
        )),
    }
}

/// Rename `from` to `to`, never over something already there.
fn place(from: &Path, to: &Path) -> Result<(), String> {
    match host_fs::rename_noreplace(from, to) {
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
            if std::fs::symlink_metadata(to).is_ok() {
                return Err(format!("{} already exists", to.display()));
            }
            std::fs::rename(from, to)
        }
        other => other,
    }
    .map_err(|error| format!("cannot move into {}: {error}", to.display()))
}

/// Extract `archive` into `folder` with progress and Cancel: into a hidden
/// folder first, moved into place only once complete, so a failed or
/// stopped extraction leaves nothing in `folder`.
fn extract_tracked(
    archive: &Path,
    folder: &Path,
    if_exists: IfExists,
    report: &dyn Fn(&LocalTransfer),
) -> Outcome {
    let mut changes = Vec::new();
    let result = extract_into_place(archive, folder, if_exists, report, &mut changes);
    match result {
        Ok(()) => Settled::Done(changes),
        Err(reason) => Settled::Failed {
            reason,
            partial: changes,
        },
    }
}

fn extract_into_place(
    archive: &Path,
    folder: &Path,
    if_exists: IfExists,
    report: &dyn Fn(&LocalTransfer),
    changes: &mut Vec<Change>,
) -> Result<(), String> {
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    let size = std::fs::metadata(archive)
        .map_err(|error| format!("cannot read {}: {error}", archive.display()))?
        .len();
    let running = Running::start(LocalTransfer {
        total: size.into(),
        kind: TransferKind::Extract,
        count: 1,
        folder: text(archive),
        started: Some(ui::time::now()),
        ..LocalTransfer::default()
    });
    let progress = ArchiveProgress::new(&running.0, report, folder);
    let extracted = crate::archive::extract(archive, folder, &progress);
    report(&running.0);
    let staging = extracted.map_err(|error| {
        archive_failure(&format!("cannot extract {}", archive.display()), &error)
    })?;
    let (result, own_folder) = staging
        .result()
        .map_err(|error| format!("cannot read what was extracted: {error}"))?;
    let name = match result.file_name() {
        Some(name) if own_folder => name.to_string_lossy().into_owned(),
        _ => crate::archive::stem(archive),
    };
    let name = fileops::leaf_name(&name)?.to_owned();
    let mut target = folder.join(&name);
    if std::fs::symlink_metadata(&target).is_ok() {
        match if_exists {
            IfExists::Skip => return Ok(()),
            IfExists::KeepBoth => target = fileops::unique_destination(folder, &name)?,
            IfExists::Replace => changes.push(trash(&target)?),
        }
    }
    place(&result, &target)?;
    staging.keep();
    changes.push(Change::Wrote {
        path: text(&target),
    });
    Ok(())
}

/// Write `bytes` as the new file `path`, never over an existing one.
fn write_new(path: &Path, bytes: &[u8]) -> Result<Change, String> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            // A half-written file is not left behind as if it were the result.
            let _ = std::fs::remove_file(path);
            format!("cannot write {}: {error}", path.display())
        })?;
    Ok(Change::Wrote { path: text(path) })
}

/// Write `content` as the file `name` in `folder`; with `replace`, a file
/// already called that goes to Trash first, so undo brings it back.
fn write_file(
    folder: &Path,
    name: &str,
    content: &str,
    replace: bool,
    changes: &mut Vec<Change>,
) -> Result<(), String> {
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    let name = fileops::leaf_name(name)?;
    let bytes = crate::documents::bytes_for(name, content)?;
    let path = folder.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(format!("{} is a folder", path.display()));
        }
        Ok(_) if !replace => {
            return Err(format!(
                "{} already exists; write it with replace to put the old one in Trash",
                path.display()
            ));
        }
        Ok(_) => changes.push(trash(&path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    }
    changes.push(write_new(&path, &bytes)?);
    Ok(())
}

/// Move `path` to Trash, noting the Trash entry it became: the one the host
/// names, else the one that appeared when exactly one did.
pub(crate) fn trash(path: &Path) -> Result<Change, String> {
    // A Trash that does not exist yet holds nothing: the entry the first
    // move to it makes is still found, so that move can be undone too.
    let before = fileops::trash_entries_snapshot().or_else(|| {
        fileops::trash_dir()
            .filter(|dir| !dir.exists())
            .map(|_| Vec::new())
    });
    if let Some(entry) = fileops::move_to_trash(path)? {
        return Ok(Change::Trashed {
            from: text(path),
            entry: Some(text(&entry)),
        });
    }
    let entry = before.and_then(|before| {
        let created: Vec<PathBuf> = fileops::trash_entries_snapshot()?
            .into_iter()
            .filter(|entry| !before.contains(entry))
            .collect();
        match created.as_slice() {
            [entry] => Some(text(entry)),
            _ => None,
        }
    });
    Ok(Change::Trashed {
        from: text(path),
        entry,
    })
}

/// Take back one change; the change that does so, which takes it back in
/// turn.
fn compensate(change: &Change) -> Result<Change, String> {
    match change {
        Change::CloudTrashed {
            to,
            entry,
            destination,
            output,
        } => crate::cloud_transfer::restore(to, entry, destination, output),
        Change::CloudImported {
            to,
            destination,
            output,
            ..
        } => crate::cloud_transfer::compensate(to, destination, output),
        Change::ClipboardTransfer { .. } | Change::ClipboardUndo { .. } => {
            Err("clipboard compensation requires its current native caller".into())
        }
        Change::Created { path } => {
            std::fs::remove_dir(path).map_err(|error| format!("cannot remove {path}: {error}"))?;
            Ok(Change::Removed { path: path.clone() })
        }
        Change::Removed { path } => {
            std::fs::create_dir(path).map_err(|error| format!("cannot create {path}: {error}"))?;
            Ok(Change::Created { path: path.clone() })
        }
        Change::Moved { from, to } => {
            let from = Path::new(from);
            let back = match (from.parent(), Path::new(to).parent()) {
                // A rename: give it its name back, in its folder.
                (Some(origin), Some(here)) if origin == here => {
                    let name = from
                        .file_name()
                        .ok_or_else(|| format!("{} has no name", from.display()))?;
                    fileops::rename(Path::new(to), &name.to_string_lossy())?
                }
                (Some(origin), _) => {
                    let name = from
                        .file_name()
                        .ok_or_else(|| format!("{} has no name", from.display()))?;
                    fileops::move_into_named(Path::new(to), origin, name)?
                }
                (None, _) => return Err(format!("{} has no folder", from.display())),
            };
            Ok(Change::Moved {
                from: to.clone(),
                to: text(&back),
            })
        }
        Change::Trashed { from, entry } => {
            let entry = entry
                .as_ref()
                .ok_or_else(|| format!("{from} cannot be found in Trash"))?;
            if fileops::restore_entries(&[PathBuf::from(entry)])? != 1 {
                return Err(format!("{from} is no longer in Trash"));
            }
            Ok(Change::Restored {
                entry: entry.clone(),
                to: from.clone(),
            })
        }
        // A restored entry goes back to Trash; so does a copy or a written
        // file, which is not deleted, so undoing the undo puts it back.
        Change::Restored { to, .. } | Change::Copied { to, .. } | Change::Wrote { path: to } => {
            trash(Path::new(to))
        }
    }
}

impl FileOp {
    /// The paths carrying this operation out writes (a folder covers what
    /// is under it): what an agent's invocation names before writing, so
    /// the versions these writes make record the agent
    /// ([`ui::framework::EffectCx::attribute_writes`]). A revert writes the
    /// paths of the changes it takes back. At most
    /// [`ui::collaboration::write_attribution::MAX_PATHS`]; more are named by
    /// their folders.
    pub fn written_paths(&self, reverting: Option<&[Change]>) -> Vec<String> {
        let within = |folder: &str, name: &str| text(&Path::new(folder).join(name));
        let mut paths: Vec<String> = match self {
            Self::NewFolder { parent, name } => vec![within(parent, name)],
            Self::Rename { path, name } => {
                vec![path.clone(), text(&Path::new(path).with_file_name(name))]
            }
            Self::Trash { paths } => paths.clone(),
            Self::Move { paths, folder, .. } => {
                paths.iter().cloned().chain([folder.clone()]).collect()
            }
            Self::Copy { folder, .. }
            | Self::ImportMounted { folder, .. }
            | Self::CloudImport { folder, .. }
            | Self::Compress { folder, .. }
            | Self::Extract { folder, .. } => vec![folder.clone()],
            Self::Write { folder, name, .. } => vec![within(folder, name)],
            Self::ClipboardReceipt { .. } => vec![],
            Self::Revert { .. } => reverting
                .unwrap_or_default()
                .iter()
                .flat_map(|change| match change {
                    Change::Created { path }
                    | Change::Removed { path }
                    | Change::Wrote { path } => {
                        vec![path.clone()]
                    }
                    Change::Moved { from, to } => vec![from.clone(), to.clone()],
                    Change::Copied { to, .. }
                    | Change::CloudImported { to, .. }
                    | Change::CloudTrashed { to, .. }
                    | Change::Restored { to, .. } => vec![to.clone()],
                    Change::Trashed { from, .. } => vec![from.clone()],
                    Change::ClipboardTransfer { .. } | Change::ClipboardUndo { .. } => vec![],
                })
                .collect(),
        };
        paths.sort();
        paths.dedup();
        if paths.len() > ui::collaboration::write_attribution::MAX_PATHS {
            paths = paths
                .iter()
                .filter_map(|path| Path::new(path).parent().map(text))
                .collect();
            paths.sort();
            paths.dedup();
            paths.truncate(ui::collaboration::write_attribution::MAX_PATHS);
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(outcome: &Outcome) -> &[Change] {
        assert!(outcome.is_done(), "{outcome:?}");
        outcome.result()
    }

    /// An operation names every path it writes, and a revert the paths of
    /// what it takes back, before an agent's invocation writes them.
    #[test]
    fn operations_name_the_paths_they_write() {
        let rename = FileOp::Rename {
            path: "/w/draft.txt".into(),
            name: "final.txt".into(),
        };
        assert_eq!(rename.written_paths(None), ["/w/draft.txt", "/w/final.txt"]);
        let moved = FileOp::Move {
            paths: vec!["/w/a.txt".into(), "/w/b.txt".into()],
            folder: "/w/Archive".into(),
            create: true,
        };
        assert_eq!(
            moved.written_paths(None),
            ["/w/Archive", "/w/a.txt", "/w/b.txt"]
        );
        assert_eq!(
            FileOp::NewFolder {
                parent: "/w".into(),
                name: "Projects".into()
            }
            .written_paths(None),
            ["/w/Projects"]
        );
        let reverting = [
            Change::Moved {
                from: "/w/draft.txt".into(),
                to: "/w/final.txt".into(),
            },
            Change::Created {
                path: "/w/Projects".into(),
            },
        ];
        assert_eq!(
            FileOp::Revert { op: "op".into() }.written_paths(Some(&reverting)),
            ["/w/Projects", "/w/draft.txt", "/w/final.txt"]
        );
        let many = FileOp::Trash {
            paths: (0..200).map(|i| format!("/w/{}/f{i}.txt", i % 3)).collect(),
        };
        assert_eq!(many.written_paths(None), ["/w/0", "/w/1", "/w/2"]);
    }

    /// Each operation records what it changed, and its revert takes exactly
    /// that back; a revert of the revert does it again.
    #[test]
    fn operations_record_their_changes_and_reverts_take_them_back() {
        let root = tempfile::tempdir().unwrap();
        let at = |name: &str| root.path().join(name).display().to_string();
        std::fs::write(root.path().join("draft.txt"), b"x").unwrap();

        let created = execute(
            &FileOp::NewFolder {
                parent: at(""),
                name: "Projects".into(),
            },
            None,
        );
        assert_eq!(
            done(&created),
            [Change::Created {
                path: at("Projects")
            }]
        );

        let renamed = execute(
            &FileOp::Rename {
                path: at("draft.txt"),
                name: "final.txt".into(),
            },
            None,
        );
        done(&renamed);
        let moved = execute(
            &FileOp::Move {
                paths: vec![at("final.txt")],
                folder: at("Archive"),
                create: true,
            },
            None,
        );
        assert_eq!(done(&moved).len(), 2, "the folder and the move");
        assert!(root.path().join("Archive/final.txt").exists());

        let revert = FileOp::Revert { op: "move".into() };
        let reverted = execute(&revert, Some(moved.result().as_slice()));
        done(&reverted);
        assert!(root.path().join("final.txt").exists());
        assert!(
            !root.path().join("Archive").exists(),
            "the created folder goes"
        );
        let again = execute(&revert, Some(reverted.result().as_slice()));
        done(&again);
        assert!(root.path().join("Archive/final.txt").exists(), "redone");
        done(&execute(&revert, Some(again.result().as_slice())));

        done(&execute(&revert, Some(renamed.result().as_slice())));
        assert!(
            root.path().join("draft.txt").exists(),
            "the old name is back"
        );
        done(&execute(&revert, Some(created.result().as_slice())));
        assert!(!root.path().join("Projects").exists());

        let nothing = execute(&revert, None);
        assert!(!nothing.is_done());
    }

    /// Move to Trash records the entry the item became and its undo puts the
    /// item back, on the freedesktop Trash managed without the mount table a
    /// sandboxed Files cannot read.
    #[cfg(unix)]
    #[test]
    fn move_to_trash_records_its_entry_and_undo_restores_it() {
        let home = tilo_test_env::TestHome::new().expect("private Trash for undo");
        let downloads = home.home().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let film = downloads.join("film.mkv");
        std::fs::write(&film, b"frames").unwrap();
        let film = film.canonicalize().unwrap();

        let trashed = execute(
            &FileOp::Trash {
                paths: vec![text(&film)],
            },
            None,
        );
        let [Change::Trashed { from, entry }] = done(&trashed) else {
            panic!("{trashed:?}");
        };
        assert_eq!(from, &text(&film));
        let entry = PathBuf::from(entry.as_deref().expect("the entry it became"));
        assert_eq!(entry.file_name(), film.file_name());
        assert_eq!(std::fs::read(&entry).unwrap(), b"frames");
        assert!(!film.exists());

        let revert = FileOp::Revert { op: "trash".into() };
        done(&execute(&revert, Some(trashed.result().as_slice())));
        assert_eq!(std::fs::read(&film).unwrap(), b"frames");
        assert!(!entry.exists());
    }

    /// Only a mounted import makes the independent destination editable.
    #[cfg(unix)]
    #[test]
    fn mounted_import_is_writable_without_changing_source_or_ordinary_copy() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let home = tilo_test_env::TestHome::new().expect("private Trash for undo");
        fileops::ensure_trash_dir().expect("open Trash before recording undo entries");
        let root = tempfile::tempdir_in(home.home()).unwrap();
        let source = root.path().join("mounted.txt");
        let folder = root.path().join("local");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&source, b"original").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o444)).unwrap();
        let paths = vec![text(&source)];
        let op = FileOp::ImportMounted {
            paths: paths.clone(),
            folder: text(&folder),
        };
        assert!(
            !execute(&op, None).is_done(),
            "serialized jobs supply no private consent"
        );
        let mut app =
            ui::prelude::TestApp::<crate::resource::FileExplorerShell>::on_device("laptop")
                .unwrap();
        app.invoke(crate::resource::FileExplorer::queueing("laptop", op).unwrap())
            .unwrap();
        let deadline = tilo_test_env::hang_deadline();
        while app.state().ops.pending().next().is_some() {
            assert!(std::time::Instant::now() < deadline);
            app.settle();
            std::thread::yield_now();
        }
        let imported = app
            .state()
            .ops
            .entries()
            .next()
            .unwrap()
            .1
            .settled
            .clone()
            .unwrap();
        let destination = folder.join("mounted.txt");
        assert_eq!(
            done(&imported),
            [Change::Copied {
                from: text(&source),
                to: text(&destination),
            }]
        );
        assert_eq!(
            std::fs::metadata(&destination).unwrap().mode() & 0o777,
            0o644
        );
        std::fs::write(&destination, b"edited locally").unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
        assert_eq!(std::fs::metadata(&source).unwrap().mode() & 0o777, 0o444);

        let copied = execute(
            &FileOp::Copy {
                paths,
                folder: text(&folder),
            },
            None,
        );
        let ordinary = folder.join("mounted 2.txt");
        assert_eq!(
            done(&copied),
            [Change::Copied {
                from: text(&source),
                to: text(&ordinary)
            }]
        );
        assert_eq!(std::fs::metadata(&ordinary).unwrap().mode() & 0o777, 0o444);

        let revert = FileOp::Revert {
            op: "import".into(),
        };
        let undone = execute(&revert, Some(done(&imported)));
        done(&undone);
        assert!(!destination.exists());
        let redone = execute(&revert, Some(done(&undone)));
        done(&redone);
        assert_eq!(std::fs::read(&destination).unwrap(), b"edited locally");
        assert_eq!(
            std::fs::metadata(&destination).unwrap().mode() & 0o777,
            0o644
        );
        assert_eq!(std::fs::metadata(&source).unwrap().mode() & 0o777, 0o444);
    }

    #[test]
    fn undo_move_restores_original_name_after_destination_collision() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("report.txt");
        let destination = root.path().join("archive");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(&source, b"moved report").unwrap();
        std::fs::write(destination.join("report.txt"), b"existing report").unwrap();
        let moved = execute(
            &FileOp::Move {
                paths: vec![text(&source)],
                folder: text(&destination),
                create: false,
            },
            None,
        );
        done(&moved);
        let suffixed = destination.join("report 2.txt");
        assert_eq!(std::fs::read(&suffixed).unwrap(), b"moved report");
        let revert = FileOp::Revert { op: "move".into() };
        let undone = execute(&revert, Some(done(&moved)));
        done(&undone);
        assert_eq!(std::fs::read(&source).unwrap(), b"moved report");
        assert!(!root.path().join("report 2.txt").exists());
        assert!(!suffixed.exists());
        let redone = execute(&revert, Some(done(&undone)));
        done(&redone);
        assert_eq!(std::fs::read(&suffixed).unwrap(), b"moved report");
        assert!(!source.exists());
        assert_eq!(
            std::fs::read(destination.join("report.txt")).unwrap(),
            b"existing report"
        );
    }

    /// A move stops at the first failure and records what it moved before.
    #[test]
    fn a_failed_operation_records_what_it_did() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), b"a").unwrap();
        let folder = root.path().join("into");
        std::fs::create_dir(&folder).unwrap();
        let outcome = execute(
            &FileOp::Move {
                paths: vec![
                    root.path().join("a.txt").display().to_string(),
                    root.path().join("missing.txt").display().to_string(),
                ],
                folder: folder.display().to_string(),
                create: false,
            },
            None,
        );
        assert!(!outcome.is_done());
        assert_eq!(outcome.result().len(), 1);
        done(&execute(
            &FileOp::Revert { op: "m".into() },
            Some(outcome.result().as_slice()),
        ));
        assert!(root.path().join("a.txt").exists());
    }

    #[test]
    fn write_and_compress_reject_names_that_address_other_folders() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("chosen");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(folder.join("nested")).unwrap();
        let source = folder.join("source.txt");
        std::fs::write(&source, b"source").unwrap();
        for name in [
            "../escaped.txt".to_owned(),
            "nested/escaped.txt".to_owned(),
            text(&root.path().join("absolute.txt")),
            ".".to_owned(),
            "..".to_owned(),
        ] {
            let write = execute(
                &FileOp::Write {
                    folder: text(&folder),
                    name: name.clone(),
                    text: "new contents".into(),
                    replace: false,
                },
                None,
            );
            assert!(!write.is_done(), "write accepted {name}: {write:?}");
            assert!(write.result().is_empty());
            let compressed = execute(
                &FileOp::Compress {
                    paths: vec![text(&source)],
                    folder: text(&folder),
                    name: name.clone(),
                },
                None,
            );
            assert!(
                !compressed.is_done(),
                "compress accepted {name}: {compressed:?}"
            );
            assert!(compressed.result().is_empty());
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
        assert_eq!(std::fs::read_dir(folder.join("nested")).unwrap().count(), 0);
        assert_eq!(std::fs::read(source).unwrap(), b"source");
    }

    /// Writing and compressing each record the new file, and
    /// their revert moves it to Trash; a replacing write keeps the old file
    /// in Trash and its revert brings it back.
    #[test]
    fn written_files_are_recorded_and_undone_through_trash() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().display().to_string();
        let at = |name: &str| root.path().join(name);

        let wrote = execute(
            &FileOp::Write {
                folder: folder.clone(),
                name: "Summary.md".into(),
                text: "# Summary\n\nShort.".into(),
                replace: false,
            },
            None,
        );
        assert_eq!(
            done(&wrote),
            [Change::Wrote {
                path: text(&at("Summary.md"))
            }]
        );
        assert_eq!(
            std::fs::read_to_string(at("Summary.md")).unwrap(),
            "# Summary\n\nShort."
        );
        let again = execute(
            &FileOp::Write {
                folder: folder.clone(),
                name: "Summary.md".into(),
                text: "other".into(),
                replace: false,
            },
            None,
        );
        assert!(!again.is_done(), "an existing file is never written over");

        let replaced = execute(
            &FileOp::Write {
                folder: folder.clone(),
                name: "Summary.md".into(),
                text: "# Summary\n\nLonger now.".into(),
                replace: true,
            },
            None,
        );
        assert_eq!(
            done(&replaced).len(),
            2,
            "the old one to Trash, the new one"
        );
        let revert = FileOp::Revert { op: "w".into() };
        done(&execute(&revert, Some(replaced.result().as_slice())));
        assert_eq!(
            std::fs::read_to_string(at("Summary.md")).unwrap(),
            "# Summary\n\nShort.",
            "undo brings the old file back"
        );

        std::fs::write(at("notes.txt"), "milk\n").unwrap();
        let compressed = execute(
            &FileOp::Compress {
                paths: vec![text(&at("Summary.md")), text(&at("notes.txt"))],
                folder,
                name: "Summary.zip".into(),
            },
            None,
        );
        assert_eq!(
            done(&compressed),
            [Change::Wrote {
                path: text(&at("Summary.zip"))
            }]
        );
        let listed = crate::archive::list_directory(&at("Summary.zip"), Path::new("")).unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");

        done(&execute(&revert, Some(compressed.result().as_slice())));
        assert!(!at("Summary.zip").exists());
        assert!(at("Summary.md").exists(), "the original stays");
    }

    /// The entries below `folder`, by name, recursively, hidden ones too.
    fn tree(folder: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut stack = vec![folder.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in std::fs::read_dir(&at).unwrap() {
                let path = entry.unwrap().path();
                names.push(path.strip_prefix(folder).unwrap().display().to_string());
                if path.is_dir() && !path.is_symlink() {
                    stack.push(path);
                }
            }
        }
        names.sort();
        names
    }

    /// Extract here: an archive holding one folder at its top extracts as
    /// that folder, anything else into a folder named after the archive; a
    /// taken name keeps both, replaces (the old one to Trash, back on
    /// undo) or skips; undo moves what was extracted to Trash.
    #[test]
    fn extracting_names_the_folder_and_settles_a_taken_name() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        let at = |name: &str| root.path().join(name);
        std::fs::create_dir_all(at("src/Project/docs")).unwrap();
        std::fs::write(at("src/Project/docs/a.txt"), b"a").unwrap();
        std::fs::write(at("src/loose.txt"), b"l").unwrap();
        let out = at("out");
        std::fs::create_dir(&out).unwrap();
        crate::archive::create(
            &[at("src/Project")],
            &at("one.tar.zst"),
            crate::archive::Packing::TarZst,
            &(),
        )
        .unwrap();
        crate::archive::create(
            &[at("src/Project"), at("src/loose.txt")],
            &at("two.zip"),
            crate::archive::Packing::Zip,
            &(),
        )
        .unwrap();
        let extract = |archive: &str, if_exists| {
            execute(
                &FileOp::Extract {
                    archive: text(&at(archive)),
                    folder: text(&out),
                    if_exists,
                },
                None,
            )
        };

        let own = extract("one.tar.zst", IfExists::KeepBoth);
        assert_eq!(
            done(&own),
            [Change::Wrote {
                path: text(&out.join("Project"))
            }]
        );
        assert_eq!(std::fs::read(out.join("Project/docs/a.txt")).unwrap(), b"a");
        let named = extract("two.zip", IfExists::KeepBoth);
        done(&named);
        assert_eq!(std::fs::read(out.join("two/loose.txt")).unwrap(), b"l");
        assert_eq!(
            std::fs::read(out.join("two/Project/docs/a.txt")).unwrap(),
            b"a"
        );

        let both = extract("one.tar.zst", IfExists::KeepBoth);
        assert_eq!(
            done(&both),
            [Change::Wrote {
                path: text(&out.join("Project 2"))
            }]
        );
        std::fs::write(out.join("Project/docs/a.txt"), b"edited").unwrap();
        let skipped = extract("one.tar.zst", IfExists::Skip);
        assert!(done(&skipped).is_empty());
        assert_eq!(
            std::fs::read(out.join("Project/docs/a.txt")).unwrap(),
            b"edited"
        );
        let replaced = extract("one.tar.zst", IfExists::Replace);
        assert_eq!(
            done(&replaced).len(),
            2,
            "the old one to Trash, the new one"
        );
        assert_eq!(std::fs::read(out.join("Project/docs/a.txt")).unwrap(), b"a");
        let revert = FileOp::Revert { op: "x".into() };
        done(&execute(&revert, Some(replaced.result().as_slice())));
        assert_eq!(
            std::fs::read(out.join("Project/docs/a.txt")).unwrap(),
            b"edited",
            "undo brings the replaced folder back"
        );
        done(&execute(&revert, Some(named.result().as_slice())));
        assert!(!out.join("two").exists(), "undo takes the extraction away");
        assert!(
            tree(&out).iter().all(|name| !name.starts_with(".tilo-")),
            "no staging is left: {:?}",
            tree(&out)
        );
    }

    /// Cancel stops an extraction and leaves the destination as it was:
    /// nothing partial, no staging folder. A refused archive (a name
    /// climbing out) does the same, and nothing lands beside it.
    #[test]
    fn a_stopped_or_refused_extraction_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let at = |name: &str| root.path().join(name);
        std::fs::create_dir_all(at("src/Big")).unwrap();
        // Noise, so the archive is as big as what it holds and its reading
        // reports progress before it is done.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for i in 0..8 {
            let noise: Vec<u8> = (0..3 << 20)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed.to_le_bytes()[0]
                })
                .collect();
            std::fs::write(at(&format!("src/Big/{i}.bin")), noise).unwrap();
        }
        let archive = at("big.tar.zst");
        crate::archive::create(
            &[at("src/Big")],
            &archive,
            crate::archive::Packing::TarZst,
            &(),
        )
        .unwrap();
        let out = at("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("kept.txt"), b"k").unwrap();
        let before = tree(&out);
        // Cancel as the person would, once the window shows progress.
        let stopped = execute_tracked(
            &FileOp::Extract {
                archive: text(&archive),
                folder: text(&out),
                if_exists: IfExists::KeepBoth,
            },
            None,
            &|transfer| {
                transfer
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            },
        );
        assert_eq!(stopped.reason(), Some(CANCELLED), "{stopped:?}");
        assert!(stopped.result().is_empty());
        assert_eq!(tree(&out), before, "nothing partial is left");
        assert!(active_transfers().is_empty(), "the card goes");

        let mut crafted = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.as_gnu_mut().unwrap().name[..12].copy_from_slice(b"../escape.tx");
        header.set_cksum();
        crafted.append(&header, &b"x"[..]).unwrap();
        std::fs::write(at("evil.tar"), crafted.into_inner().unwrap()).unwrap();
        let refused = execute(
            &FileOp::Extract {
                archive: text(&at("evil.tar")),
                folder: text(&out),
                if_exists: IfExists::KeepBoth,
            },
            None,
        );
        assert!(!refused.is_done(), "{refused:?}");
        assert_eq!(tree(&out), before);
        assert!(!at("escape.tx").exists());
    }
}
