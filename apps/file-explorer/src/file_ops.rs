//! Filesystem changes as the Files document records them.
//!
//! Creating a folder, renaming, moving to Trash, moving and copying files are document
//! actions: [`FileOp`]s queued in the Files document
//! ([`crate::resource::FilesDocAction::QueueFileOp`]) by the device whose
//! filesystem they change, then carried out there by a projection drain that
//! records what was done ([`Outcome`]) with
//! [`crate::resource::FilesDocAction::SettleFileOp`]. Being document actions,
//! they are in the document's history and undoable: undoing one queues a
//! [`FileOp::Revert`], which the same drain carries out by compensating each
//! recorded [`Change`] in reverse order — renaming back, moving back,
//! restoring from Trash, removing the created folder, moving a copy to
//! Trash.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
}

/// What carrying out an operation came to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// Every change was made.
    Done { changes: Vec<Change> },
    /// `error` stopped it after `changes` were made.
    Failed { error: String, changes: Vec<Change> },
}

impl Outcome {
    /// The changes that were made.
    pub fn changes(&self) -> &[Change] {
        match self {
            Self::Done { changes } | Self::Failed { changes, .. } => changes,
        }
    }

    /// Why it failed, when it did.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Done { .. } => None,
            Self::Failed { error, .. } => Some(error),
        }
    }
}

/// An operation queued in the Files document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedOp {
    pub id: String,
    /// The device whose filesystem it changes, which alone carries it out.
    pub device: String,
    pub op: FileOp,
    /// `None` until the device has carried it out.
    pub outcome: Option<Outcome>,
    /// Its place in the queue, for keeping the most recent.
    pub order: i64,
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// Carry out `op` on this device's filesystem. A revert is given the outcome
/// of the operation it takes back (`None` when that operation is unknown or
/// not carried out yet).
pub fn execute(op: &FileOp, reverting: Option<&Outcome>) -> Outcome {
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
        FileOp::Revert { .. } => match reverting {
            Some(outcome) => outcome
                .changes()
                .iter()
                .rev()
                .try_for_each(|change| compensate(change).map(|done| changes.push(done))),
            None => Err("there is nothing to undo: the operation was not carried out".into()),
        },
    };
    match result {
        Ok(()) => Outcome::Done { changes },
        Err(error) => Outcome::Failed { error, changes },
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

/// Move `path` to Trash, noting the Trash entry it became when exactly one
/// appeared.
fn trash(path: &Path) -> Result<Change, String> {
    let before = fileops::trash_entries_snapshot();
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
        // A restored entry goes back to Trash; so does a copy, which is not
        // deleted, so undoing the undo puts it back.
        Change::Restored { to, .. } | Change::Copied { to, .. } => trash(Path::new(to)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(outcome: &Outcome) -> &[Change] {
        assert!(outcome.error().is_none(), "{outcome:?}");
        outcome.changes()
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
        let reverted = execute(&revert, Some(&moved));
        done(&reverted);
        assert!(root.path().join("final.txt").exists());
        assert!(
            !root.path().join("Archive").exists(),
            "the created folder goes"
        );
        let again = execute(&revert, Some(&reverted));
        done(&again);
        assert!(root.path().join("Archive/final.txt").exists(), "redone");
        done(&execute(&revert, Some(&again)));

        done(&execute(&revert, Some(&renamed)));
        assert!(
            root.path().join("draft.txt").exists(),
            "the old name is back"
        );
        done(&execute(&revert, Some(&created)));
        assert!(!root.path().join("Projects").exists());

        let nothing = execute(&revert, None);
        assert!(nothing.error().is_some());
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
        assert!(outcome.error().is_some());
        assert_eq!(outcome.changes().len(), 1);
        done(&execute(&FileOp::Revert { op: "m".into() }, Some(&outcome)));
        assert!(root.path().join("a.txt").exists());
    }
}
