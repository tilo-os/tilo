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
    /// Pack `paths` into the zip archive `name` (a free name) in `folder`.
    Compress {
        paths: Vec<String>,
        folder: String,
        name: String,
    },
    /// A transfer the native host already carried out, by its receipt's
    /// operation. It is never carried out here; only the host compensates it.
    ClipboardReceipt { operation: String },
    /// Undo the queued operation `op`: compensate what it changed.
    Revert { op: String },
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

/// A local copy or move under way, as the window shows it: how far it is,
/// and whether the person asked to stop.
#[derive(Debug, Default)]
pub struct LocalTransfer {
    /// Bytes copied so far, and of how many.
    pub done: std::sync::atomic::AtomicU64,
    pub total: std::sync::atomic::AtomicU64,
    /// Set by Cancel; the transfer stops at the next chunk.
    pub cancel: AtomicBool,
    /// Copying (else moving), how many items, and into which folder.
    pub copy: bool,
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
        copy,
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
        } => compress(paths, Path::new(folder), name).map(|change| changes.push(change)),
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

/// Pack `paths` into a new zip archive called `name`, or a free name like
/// it, in `folder`.
fn compress(paths: &[String], folder: &Path, name: &str) -> Result<Change, String> {
    let name = fileops::leaf_name(name)?;
    let archive = fileops::unique_destination(folder, name)?;
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    crate::archive::create_zip(&paths, &archive)
        .map_err(|error| format!("cannot create {}: {error}", archive.display()))?;
    Ok(Change::Wrote {
        path: text(&archive),
    })
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

/// Move `path` to Trash, noting the Trash entry it became when exactly one
/// appeared.
pub(crate) fn trash(path: &Path) -> Result<Change, String> {
    // A Trash that does not exist yet holds nothing: the entry the first
    // move to it makes is still found, so that move can be undone too.
    let before = fileops::trash_entries_snapshot().or_else(|| {
        fileops::trash_dir()
            .filter(|dir| !dir.exists())
            .map(|_| Vec::new())
    });
    fileops::delete_to_trash(&[path.to_path_buf()])?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn done(outcome: &Outcome) -> &[Change] {
        assert!(outcome.is_done(), "{outcome:?}");
        outcome.result()
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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
}
