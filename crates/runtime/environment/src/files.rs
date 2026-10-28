//! An environment's file actions: `cp`, `mv`, `rm`, `mkdir`, `rmdir`, `ln`,
//! `truncate`, `touch` and `append` as the record's own verbs.
//!
//! A command typed in a guest, and an agent calling `move` with paths, are the
//! same thing: one invocation of one of the environment's published actions.
//! The verb checks every path it names against the environment's grants, as a
//! whole, and queues one [`FileTask`] in the record's `files` outbox for the
//! device whose disk it changes. That device's runtime carries the task out
//! and settles it with what it did ([`UndoEntry`]); undoing the action queues
//! its [`FileAction::Revert`], which the same device carries out by walking
//! those entries back. So History, undo and the permission check are the
//! record's, like every other action of the record's.
//!
//! What is here is the data and the check; carrying a task out touches the
//! filesystem and lives with the runtime (`ush::file_actions`).

use std::path::{Component, Path, PathBuf};

use collab_types::caps::{Access, Actor, CapDecision, CapOverride, ResourceRef, decide_capability};
use serde::{Deserialize, Serialize};
use tilo_resource::Model;

use crate::RUNTIME_APP_ID;

/// The most paths one invocation names: a command line with more is
/// refused whole, never carried out in part.
pub const MAX_OPERANDS: usize = 256;

/// How many carried-out file actions the record keeps, so recent ones can
/// still be undone. What an older one kept aside stays in the environment's
/// recoverable store, listed and restored by its id.
pub const KEPT_TASKS: usize = 64;

/// The most text one `append` carries in the record.
pub const MAX_APPEND_BYTES: usize = 64 * 1024;

/// What a command asked for, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum FileAction {
    Copy(CopyOp),
    Move(MoveOp),
    /// Remove `paths` into the recoverable store.
    Remove(RemoveOp),
    MakeDir(MakeDirOp),
    RemoveDir(RemoveDirOp),
    Link(LinkOp),
    Truncate(TruncateOp),
    /// Create `files` if they are missing, leave their contents alone.
    Touch(AppendFileOp),
    /// Add `text` to the end of each of `files`.
    Append(AppendFileOp),
    /// Take back the task `entry` of the record's outbox: what undoing an
    /// action queues.
    Revert(RevertOp),
    /// Put back one thing the recoverable store keeps, by its id: what an
    /// older runtime kept there, or what a task the record no longer lists
    /// moved aside.
    Restore(RestoreOp),
}

impl Default for FileAction {
    fn default() -> Self {
        Self::Touch(AppendFileOp::default())
    }
}

tilo_resource::json_model!(FileAction);

impl FileAction {
    /// The verb, as the record's actions and the audit log name it.
    #[must_use]
    pub const fn verb(&self) -> &'static str {
        match self {
            Self::Copy(_) => "copy",
            Self::Move(_) => "move",
            Self::Remove(_) => "remove",
            Self::MakeDir(_) => "make-dir",
            Self::RemoveDir(_) => "remove-dir",
            Self::Link(_) => "link",
            Self::Truncate(_) => "truncate",
            Self::Touch(_) => "touch",
            Self::Append(_) => "append",
            Self::Revert(_) => "revert",
            Self::Restore(_) => "restore",
        }
    }

    /// The paths this action only reads, by name: a copy's sources. The
    /// grant must at least let the guest read them; the runtime carrying it
    /// out reads the host's filesystem, which is wider than the guest's.
    #[must_use]
    pub fn reads(&self) -> Vec<PathBuf> {
        match self {
            Self::Copy(op) => op.sources.clone(),
            _ => Vec::new(),
        }
    }

    /// The paths this action writes, creates or gives up, by name. A move
    /// gives its sources up, so they are written too, and a hard link writes
    /// its target: both names are one file afterwards.
    #[must_use]
    pub fn writes(&self) -> Vec<PathBuf> {
        match self {
            Self::Copy(op) => vec![op.destination.clone()],
            Self::Move(op) => op
                .sources
                .iter()
                .cloned()
                .chain(std::iter::once(op.destination.clone()))
                .collect(),
            Self::Remove(op) => op.paths.clone(),
            Self::MakeDir(op) => op.dirs.clone(),
            Self::RemoveDir(op) => op.dirs.clone(),
            Self::Link(op) if op.symbolic => vec![op.path.clone()],
            Self::Link(op) => vec![op.path.clone(), op.target.clone()],
            Self::Truncate(op) => op.files.clone(),
            Self::Touch(op) | Self::Append(op) => op.files.clone(),
            Self::Revert(_) | Self::Restore(_) => Vec::new(),
        }
    }

    /// Every path the action names: what the audit log and History are
    /// about.
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        let mut out = self.reads();
        out.extend(self.writes());
        out
    }

    /// Whether `grants` allow the whole invocation as of `now` (seconds):
    /// every path it reads readable, every path it writes writable. The
    /// answer is the first path refused, so a command with forty operands is
    /// refused in one piece, or nothing.
    ///
    /// # Errors
    ///
    /// Why the invocation as a whole is refused.
    pub fn admit(&self, grants: &[CapOverride], now: Option<u64>) -> Result<(), String> {
        let named = self.paths().len();
        if named > MAX_OPERANDS {
            return Err(format!(
                "{named} paths in one {}; at most {MAX_OPERANDS}",
                self.verb()
            ));
        }
        if let Self::Append(op) = self
            && op.text.len() > MAX_APPEND_BYTES
        {
            return Err(format!(
                "append carries at most {MAX_APPEND_BYTES} bytes of text"
            ));
        }
        for path in self.paths() {
            if !path.is_absolute() {
                return Err(format!("{}: not an absolute path", path.display()));
            }
        }
        for path in self.reads() {
            if !permits_in(grants, &path, Access::Read, now) {
                return Err(ungranted(&path));
            }
        }
        for path in self.writes() {
            if !permits_in(grants, &path, Access::Write, now) {
                return Err(ungranted(&path));
            }
        }
        Ok(())
    }
}

/// The refusal for a path no grant covers, as the person reads it.
#[must_use]
pub fn ungranted(path: &Path) -> String {
    format!(
        "{}: not granted to this environment; grant its directory with `perm trust-dir`",
        path.display()
    )
}

/// Is `path` reachable at `verb` under `grants`? The one question every
/// grant check of an environment asks, with the same `decide_capability`
/// the compositor's permissions panel and the launcher use.
#[must_use]
pub fn permits(grants: &[CapOverride], path: &Path, verb: Access, now: Option<u64>) -> bool {
    let actor = Actor::app(RUNTIME_APP_ID);
    let resource = ResourceRef::FsPath(normalize(path).to_string_lossy().into_owned());
    decide_capability(&actor, &resource, verb, grants, now) == CapDecision::Allow
}

/// Whether `path` itself or the directory it would be written into is
/// granted for `verb`.
///
/// A grant names a directory, and the files in it are created later, so a
/// question about a file that does not exist yet is a question about its
/// parent. Answering only the path would refuse every `mkdir ~/src/new` and
/// every file a command writes into a granted directory.
#[must_use]
pub fn permits_in(grants: &[CapOverride], path: &Path, verb: Access, now: Option<u64>) -> bool {
    if permits(grants, path, verb, now) {
        return true;
    }
    normalize(path)
        .parent()
        .is_some_and(|dir| !dir.as_os_str().is_empty() && permits(grants, dir, verb, now))
}

/// Lexically normalise a path: no `.` or `..` segments, no doubled
/// separators, so `granted/../elsewhere` is asked about as `elsewhere`, and
/// the same file is one subject in the log.
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// `cp` as it was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyOp {
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub no_clobber: bool,
}

/// `mv` as it was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveOp {
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
    #[serde(default)]
    pub no_clobber: bool,
}

/// `rm` as it was asked for. `force` (GNU's `-f`) says nothing about a path
/// that is not there; what is there is kept aside like any removal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveOp {
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub force: bool,
}

/// `mkdir` as it was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MakeDirOp {
    pub dirs: Vec<PathBuf>,
    #[serde(default)]
    pub parents: bool,
}

/// `rmdir` as it was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveDirOp {
    pub dirs: Vec<PathBuf>,
    #[serde(default)]
    pub parents: bool,
}

/// `ln` as it was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkOp {
    pub target: PathBuf,
    pub path: PathBuf,
    #[serde(default)]
    pub symbolic: bool,
    #[serde(default)]
    pub force: bool,
    /// `ln -s TARGET DIR` and `ln TARGET` on a directory make the link inside
    /// it, named after the target.
    #[serde(default)]
    pub into_directory: bool,
}

/// `truncate` as it was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruncateOp {
    pub files: Vec<PathBuf>,
    pub size: u64,
    #[serde(default)]
    pub create: bool,
}

/// `touch` and `append` as they were asked for: the same files, and the text
/// `append` adds (empty for `touch`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendFileOp {
    pub files: Vec<PathBuf>,
    #[serde(default)]
    pub text: String,
}

/// Take back the outbox entry `entry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertOp {
    pub entry: String,
}

/// Put back the recoverable store's entry `kept`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOp {
    pub kept: String,
}

/// One file action queued for the device whose disk it changes.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct FileTask {
    /// The device whose runtime alone carries it out.
    pub device: String,
    pub action: FileAction,
}

/// One operation of a carried-out action, as undoing reads it: what a task
/// settles with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "undo", rename_all = "kebab-case")]
pub enum UndoEntry {
    /// Delete the directory that was made.
    MakeDir { path: PathBuf },
    /// Make the directory that was removed again.
    RemoveDir { path: PathBuf },
    /// Put back what was moved into the recoverable store as `id`.
    Kept { id: String, path: PathBuf },
    /// Remove the file that was written; what it replaced comes back from
    /// the entry kept beside it.
    Write { path: PathBuf },
    /// Remove the file that was made.
    MakeFile { path: PathBuf },
    /// Nothing changed, so undoing takes nothing away.
    Touched,
    /// Remove the copy. Undo does not take back the source.
    Copy { from: PathBuf, to: PathBuf },
    /// Remove the link.
    Link { path: PathBuf, symbolic: bool },
    /// The store's entry `id` was put back at `path`: undoing moves it
    /// aside again.
    Restored { id: String, path: PathBuf },
}

impl UndoEntry {
    /// The path undoing this entry writes, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::MakeDir { path }
            | Self::RemoveDir { path }
            | Self::Kept { path, .. }
            | Self::Write { path }
            | Self::MakeFile { path }
            | Self::Link { path, .. }
            | Self::Restored { path, .. }
            | Self::Copy { to: path, .. } => Some(path),
            Self::Touched => None,
        }
    }
}

/// The paths of a verb's parameter, as the record stores them.
pub(crate) fn paths_of(list: Vec<String>) -> Vec<PathBuf> {
    list.into_iter().map(PathBuf::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir_grant;

    fn granted(dir: &str) -> Vec<CapOverride> {
        vec![dir_grant(Path::new(dir), true).unwrap()]
    }

    #[test]
    fn a_whole_invocation_is_admitted_or_refused_as_one() {
        let grants = granted("/srv/work");
        let inside: Vec<PathBuf> = (0..39)
            .map(|n| PathBuf::from(format!("/srv/work/{n}.txt")))
            .collect();
        let ok = FileAction::Copy(CopyOp {
            sources: inside.clone(),
            destination: "/srv/work/out/".into(),
            ..CopyOp::default()
        });
        ok.admit(&grants, None).unwrap();
        let over = FileAction::Copy(CopyOp {
            sources: inside,
            destination: "/srv/other/".into(),
            ..CopyOp::default()
        });
        let refused = over.admit(&grants, None).unwrap_err();
        assert!(refused.contains("/srv/other"), "{refused}");
    }

    #[test]
    fn a_source_the_guest_cannot_see_is_never_read_for_it() {
        let grants = granted("/srv/work");
        let copy = FileAction::Copy(CopyOp {
            sources: vec!["/home/you/.ssh/id_ed25519".into()],
            destination: "/srv/work/".into(),
            ..CopyOp::default()
        });
        assert!(copy.admit(&grants, None).is_err());
        let moved = FileAction::Move(MoveOp {
            sources: vec!["/home/you/.bashrc".into()],
            destination: "/srv/work/".into(),
            no_clobber: false,
        });
        assert!(
            moved.admit(&grants, None).is_err(),
            "a move gives its source up"
        );
    }

    #[test]
    fn dot_dot_does_not_climb_out_of_a_grant() {
        let grants = granted("/srv/work");
        let escape = FileAction::Remove(RemoveOp {
            paths: vec!["/srv/work/../secret".into()],
            ..RemoveOp::default()
        });
        assert!(escape.admit(&grants, None).is_err());
        let relative = FileAction::Remove(RemoveOp {
            paths: vec!["notes.md".into()],
            ..RemoveOp::default()
        });
        assert!(relative.admit(&grants, None).is_err());
    }
}
