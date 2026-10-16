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

/// Carry out `op` on this device's filesystem. A revert is given the changes
/// the operation it takes back made (`None` when that operation is unknown).
pub fn execute(op: &FileOp, reverting: Option<&[Change]>) -> Outcome {
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
fn trash(path: &Path) -> Result<Change, String> {
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
                (Some(origin), _) => fileops::move_into(Path::new(to), origin)?,
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
