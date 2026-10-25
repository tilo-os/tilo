//! File actions: the verbs that change a granted directory, as typed
//! values rather than flags assembled by hand.
//!
//! `cp`, `mv`, `rm`, `mkdir`, `rmdir`, `ln` and `truncate` inside a guest are
//! this module. Each invocation parses into one [`FileAction`] carrying its
//! arguments, is classified once ([`FileAction::plan`]) into the places it
//! would write and the operations it would perform, is checked as a whole
//! against the environment's grants, and is carried out through
//! [`FileAction::apply`] — which also hands back the [undo entry](UndoEntry)
//! that puts the tree back.
//!
//! # Why one value per invocation
//!
//! A shell one-liner that copies forty files is one thing the person asked
//! for. Refusing the seventeenth file would leave thirty-nine copies and no
//! way to tell which; logging each `open` would fill the log with the
//! implementation of one intent. So the action is parsed, planned and
//! checked before anything is written, and written as one unit.
//!
//! # Nothing is unlinked
//!
//! `rm` renames the path into the environment's recoverable history
//! ([`RECOVERABLE_DIR`]) instead of unlinking it, and an overwrite (`cp`,
//! `mv`, `ln -sf`, `truncate`) moves the previous contents aside the same
//! way. `rmdir` cannot lose anything — it only succeeds on an empty
//! directory — so it removes the entry and its undo recreates the directory.
//! The one path that really destroys data is [`FileAction::irreversible`]: a
//! removal asked for with `--force`, which callers name to the person before
//! it runs.
//!
//! # Where the check happens
//!
//! Inside a guest the supervisor answers, over the broker, because the
//! environment's grants are the record the supervisor owns and the guest can
//! neither read nor rewrite ([`Request::FileAction`](crate::broker::Request)).
//! On a device without that runtime (the host's own shell) there is no
//! environment record to check, so [`no_grants`] allows every path and the
//! person's own permissions are the only bound — the same command, doing the
//! same thing, with the same recoverable history.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub mod cli;

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;

use crate::audit::{AuditLog, AuditRecord};
use crate::broker::{BrokerState, EnvironmentHandle, Response};
use crate::policy::Policy;

/// Where a removed path (or the previous contents of an overwritten one) is
/// kept instead of being unlinked: under the environment's own `/var`, so it
/// lives and dies with the environment and never collides with the guest's
/// own `/var/lib`.
pub const RECOVERABLE_DIR: &str = "/var/lib/tilo-recoverable";

/// The same directory as the environment's `/var` holds it, which is how the
/// supervisor finds it on the host.
pub const RECOVERABLE_IN_VAR: &str = "tilo-recoverable";

/// Metadata kept beside every recorded entry, so `ush history` can say what
/// happened to what without reading the audit log's many lines.
pub const ENTRY_FILE: &str = "entry.json";

/// What a command asked for, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum FileAction {
    Copy(CopyOp),
    Move(MoveOp),
    /// Remove `paths`: into the recoverable history, or permanently when the
    /// caller asked for `--force` on what has already been removed.
    Remove(RemoveOp),
    MakeDir(MakeDirOp),
    RemoveDir(RemoveDirOp),
    Link(LinkOp),
    Truncate(TruncateOp),
}

impl FileAction {
    /// The verb, as the audit log and history name it.
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
        }
    }

    /// Whether carrying this out loses anything the recoverable history does
    /// not hold. Callers name it to the person before asking for consent.
    #[must_use]
    pub const fn irreversible(&self) -> bool {
        !matches!(self, Self::Remove(RemoveOp { force: false, .. }))
    }

    /// Every path this action reads out of, writes to, or creates. Both the
    /// grant check and the audit log are about these.
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Copy(op) => op.paths(),
            Self::Move(op) => op.paths(),
            Self::Remove(op) => op.paths.clone(),
            Self::MakeDir(op) => op.dirs.clone(),
            Self::RemoveDir(op) => op.dirs.clone(),
            Self::Link(op) => op.paths(),
            Self::Truncate(op) => op.files.clone(),
        }
    }

    /// Plan the writes against the tree as it is, then ask
    /// `allowed` about each of them. The answer is the refusal that stops the
    /// whole invocation, or the plan it may carry out.
    ///
    /// # Errors
    ///
    /// The first refused path, or the reason nothing can be done (a missing
    /// source, a destination that cannot take the plan).
    pub fn plan(&self, allowed: &dyn Fn(&Path) -> bool) -> Result<Plan, Refusal> {
        let plan = match self {
            Self::Copy(op) => plan_copy(op)?,
            Self::Move(op) => plan_move(op)?,
            Self::Remove(op) => plan_remove(op)?,
            Self::MakeDir(op) => plan_make_dir(op)?,
            Self::RemoveDir(op) => plan_remove_dir(op)?,
            Self::Link(op) => plan_link(op)?,
            Self::Truncate(op) => plan_truncate(op)?,
        };
        for path in &plan.writes {
            if !allowed(path) {
                return Err(Refusal::Ungranted(path.clone()));
            }
        }
        Ok(plan)
    }

    /// Carry out a plan this value produced, and record each operation in the
    /// recoverable history as it happens, so a failure halfway leaves the
    /// operations already done recoverable.
    ///
    /// # Errors
    ///
    /// The filesystem error that stopped it, after the operations behind it
    /// were carried out.
    pub fn apply(&self, plan: &Plan, history: &Recoverable) -> Result<Outcome, ActionError> {
        let id = history.mint().map_err(ActionError::History)?;
        let mut outcome = Outcome {
            id: id.clone(),
            done: Vec::new(),
        };
        for op in &plan.ops {
            match op {
                Operation::MakeDir { path, parents } => {
                    create_dir(path, *parents).map_err(|error| ActionError::Io {
                        path: path.clone(),
                        error,
                    })?;
                    history.record(&id, op)?;
                    outcome.done.push(UndoEntry::MakeDir { path: path.clone() });
                }
                Operation::RemoveDir { path } => {
                    fs::remove_dir(path).map_err(|error| ActionError::Io {
                        path: path.clone(),
                        error,
                    })?;
                    history.record(&id, op)?;
                    outcome
                        .done
                        .push(UndoEntry::RemoveDir { path: path.clone() });
                }
                Operation::Keep { path, contents } => {
                    history.stash(&id, path, contents.as_deref())?;
                    outcome.done.push(UndoEntry::Kept {
                        id: id.clone(),
                        path: path.clone(),
                    });
                }
                Operation::Discard { path, recursive } => {
                    discard(path, *recursive).map_err(|error| ActionError::Io {
                        path: path.clone(),
                        error,
                    })?;
                    history.record(&id, op)?;
                    outcome
                        .done
                        .push(UndoEntry::Discarded { path: path.clone() });
                }
                Operation::Write { path, contents } => {
                    write_file(path, contents).map_err(|error| ActionError::Io {
                        path: path.clone(),
                        error,
                    })?;
                    history.record(&id, op)?;
                    outcome.done.push(UndoEntry::Write { path: path.clone() });
                }
                Operation::Copy { from, to } => {
                    copy_tree(from, to).map_err(|error| ActionError::Io {
                        path: to.clone(),
                        error,
                    })?;
                    history.record(&id, op)?;
                    outcome.done.push(UndoEntry::Copy {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
                Operation::Link {
                    target,
                    path,
                    symbolic,
                } => {
                    link(target, path, *symbolic).map_err(|error| ActionError::Io {
                        path: path.clone(),
                        error,
                    })?;
                    history.record(&id, op)?;
                    outcome.done.push(UndoEntry::Link {
                        path: path.clone(),
                        symbolic: *symbolic,
                    });
                }
            }
        }
        Ok(outcome)
    }
}

/// `cp` as it was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyOp {
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
    pub recursive: bool,
    pub update: bool,
    pub no_clobber: bool,
}

impl CopyOp {
    fn paths(&self) -> Vec<PathBuf> {
        self.sources
            .iter()
            .cloned()
            .chain(std::iter::once(self.destination.clone()))
            .collect()
    }
}

/// `mv` as it was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveOp {
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
    pub no_clobber: bool,
}

impl MoveOp {
    fn paths(&self) -> Vec<PathBuf> {
        self.sources
            .iter()
            .cloned()
            .chain(std::iter::once(self.destination.clone()))
            .collect()
    }
}

/// `rm` as it was asked for. `force` (GNU's `-f`) records nothing: the
/// caller has said it does not want the path back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveOp {
    pub paths: Vec<PathBuf>,
    pub recursive: bool,
    pub force: bool,
}

/// `mkdir` as it was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MakeDirOp {
    pub dirs: Vec<PathBuf>,
    pub parents: bool,
}

/// `rmdir` as it was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveDirOp {
    pub dirs: Vec<PathBuf>,
    pub parents: bool,
}

/// `ln` as it was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkOp {
    pub target: PathBuf,
    pub path: PathBuf,
    pub symbolic: bool,
    pub force: bool,
    /// `ln -s TARGET DIR` and `ln TARGET` on a directory make the link inside
    /// it, named after the target.
    pub into_directory: bool,
}

impl LinkOp {
    fn paths(&self) -> Vec<PathBuf> {
        vec![self.target.clone(), self.path.clone()]
    }
}

/// `truncate` as it was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruncateOp {
    pub files: Vec<PathBuf>,
    pub size: u64,
    pub create: bool,
}

/// Why an action will not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The path is outside every directory the caller was granted.
    Ungranted(PathBuf),
    /// The path is not there. `-f` silences it for `rm`, as GNU does.
    Missing(PathBuf),
    /// A directory was asked for where a file stands, or the other way round.
    NotADirectory(PathBuf),
    /// `cp` of a directory without `-r`.
    IsDirectory(PathBuf),
    /// `cp -r` of a directory into itself.
    IntoItself(PathBuf),
    /// A destination already holds something and `-n` was given.
    Exists(PathBuf),
    /// `rmdir` of a directory that still holds something, or `mv` of a
    /// directory into itself.
    NotEmpty(PathBuf),
    /// `rm` of something that is neither a file nor `-r`. GNU's wording.
    CannotRemove(PathBuf),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ungranted(path) => write!(
                f,
                "{}: not granted to this environment; grant its directory with `perm trust-dir`",
                path.display()
            ),
            Self::Missing(path) => write!(f, "{}: no such file or directory", path.display()),
            Self::NotADirectory(path) => write!(f, "{}: not a directory", path.display()),
            Self::IsDirectory(path) => write!(f, "{}: is a directory", path.display()),
            Self::IntoItself(path) => {
                write!(f, "{}: cannot copy a directory into itself", path.display())
            }
            Self::Exists(path) => write!(f, "{}: already exists", path.display()),
            Self::NotEmpty(path) => write!(f, "{}: directory not empty", path.display()),
            Self::CannotRemove(path) => {
                write!(
                    f,
                    "{}: is a directory (use -r to remove it)",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// Why an action stopped partway.
#[derive(Debug)]
pub enum ActionError {
    Io { path: PathBuf, error: io::Error },
    History(io::Error),
}

/// Either half of what [`perform`] can answer with: the check refused the
/// action before it did anything, or the filesystem stopped it partway.
#[derive(Debug)]
pub enum FileActionError {
    Refused(Refusal),
    Failed(ActionError),
}

impl std::fmt::Display for FileActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Failed(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FileActionError {}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Self::History(error) => write!(f, "cannot record the deletion: {error}"),
        }
    }
}

impl std::error::Error for ActionError {}

/// One thing to do, in the order to do it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Create a directory, and its parents when `parents`.
    MakeDir {
        path: PathBuf,
        parents: bool,
    },
    /// Remove an empty directory.
    RemoveDir {
        path: PathBuf,
    },
    /// Move what is at `path` into the recoverable history. `contents` is the
    /// data to keep when the path may not be readable later (a truncated
    /// file), so what is kept is what was there.
    Keep {
        path: PathBuf,
        contents: Option<Vec<u8>>,
    },
    /// Remove `path` without keeping it: what `--force` asks for.
    Discard {
        path: PathBuf,
        recursive: bool,
    },
    /// Create or replace `path` with `contents`.
    Write {
        path: PathBuf,
        contents: Vec<u8>,
    },
    Copy {
        from: PathBuf,
        to: PathBuf,
    },
    Link {
        target: PathBuf,
        path: PathBuf,
        symbolic: bool,
    },
}

/// What an action will do, decided before it does any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Every path it writes to or creates, in order.
    pub writes: Vec<PathBuf>,
    pub ops: Vec<Operation>,
}

impl Plan {
    fn new() -> Self {
        Self {
            writes: Vec::new(),
            ops: Vec::new(),
        }
    }

    fn writes(mut self, path: impl Into<PathBuf>) -> Self {
        self.writes.push(path.into());
        self
    }

    fn push(mut self, op: Operation) -> Self {
        self.ops.push(op);
        self
    }
}

/// What an applied action leaves behind: one entry per operation, in the
/// order they happened, which is what undoing has to walk backwards.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The invocation's entry in the recoverable history: every operation it
    /// recorded shares it, so `undo` takes back exactly this action and not
    /// what another command did to the same path afterwards.
    pub id: String,
    pub done: Vec<UndoEntry>,
}

/// One operation of an applied action, as undoing reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "undo", rename_all = "kebab-case")]
pub enum UndoEntry {
    /// Delete the directory that was made.
    MakeDir { path: PathBuf },
    /// Make the directory that was removed again.
    RemoveDir { path: PathBuf },
    /// Put back what was moved into the recoverable history.
    Kept { id: String, path: PathBuf },
    /// Remove the file that was written, or put back what it replaced.
    Write { path: PathBuf },
    /// Remove the copy. Undo does not take back the source.
    Copy { from: PathBuf, to: PathBuf },
    /// Remove the link.
    Link { path: PathBuf, symbolic: bool },
    /// Removed without keeping it: nothing to put back, so undoing takes
    /// nothing away either.
    Discarded { path: PathBuf },
}

/// Undo a set of entries, latest first.
///
/// What can be put back is; an entry whose path has changed since (a file
/// another process wrote) is reported, not overwritten.
///
/// # Errors
///
/// The first path that could not be put back.
pub fn undo(entries: &[UndoEntry], history: &Recoverable) -> Result<(), ActionError> {
    // Past the writes that named a path: what was kept goes back last, so a
    // `mv` restores its source after the copy it made has gone.
    for entry in entries.iter().rev() {
        match entry {
            UndoEntry::MakeDir { path } => {
                if path.is_dir() {
                    fs::remove_dir(path).map_err(|error| ActionError::Io {
                        path: path.clone(),
                        error,
                    })?;
                }
            }
            UndoEntry::RemoveDir { path } => {
                fs::create_dir_all(path).map_err(|error| ActionError::Io {
                    path: path.clone(),
                    error,
                })?;
            }
            UndoEntry::Kept { id, path } => {
                history.restore(id, path)?;
            }
            UndoEntry::Write { path } => {
                // What the action wrote goes; what it replaced comes back
                // from the entry recorded beside it, later in this walk.
                remove_if_present(path)?;
            }
            // Removed on purpose: there is nothing here to take away.
            UndoEntry::Discarded { .. } => {}
            UndoEntry::Copy { to, .. } | UndoEntry::Link { path: to, .. } => {
                remove_if_present(to)?;
            }
        }
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), ActionError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ActionError::Io {
                path: path.to_path_buf(),
                error,
            });
        }
    };
    let result = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|error| ActionError::Io {
        path: path.to_path_buf(),
        error,
    })
}

/// The recoverable history: a directory of entries, one per removed or
/// replaced path, each naming the path it came from.
#[derive(Debug, Clone)]
pub struct Recoverable {
    dir: PathBuf,
}

/// What one entry of the history remembers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The entry's id inside the history: when it was recorded, and a
    /// sequence so two in the same second do not collide.
    pub id: String,
    /// The invocation that recorded it. A `mv` records several operations
    /// under one id, and undoing it takes back exactly those.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    /// Seconds since the Unix epoch.
    pub at: u64,
    /// What happened: `keep`, `write`, `copy`, `link`, `make-dir`, `remove-dir`.
    pub op: String,
    /// The path the entry came from.
    pub path: PathBuf,
    /// The path of the operation's other end (`copy` source, `link` target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<PathBuf>,
    /// The entry's directory in the history.
    pub file: PathBuf,
}

impl Recoverable {
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn mint(&self) -> io::Result<String> {
        fs::create_dir_all(&self.dir)?;
        let at = unix_now();
        for seq in 0..1000u16 {
            let id = format!("{at}-{seq:03}");
            if !self.dir.join(&id).exists() {
                return Ok(id);
            }
        }
        Err(io::Error::other("the recoverable history is full"))
    }

    /// Move `path` into the history, keeping what was there.
    ///
    /// # Errors
    ///
    /// Whatever kept the move from happening — in which case the caller has
    /// changed nothing.
    pub fn stash(
        &self,
        action: &str,
        path: &Path,
        contents: Option<&[u8]>,
    ) -> Result<(), ActionError> {
        let id = self.mint().map_err(ActionError::History)?;
        let slot = self.dir.join(&id);
        let kept = slot.join("payload");
        let result = (|| -> io::Result<()> {
            fs::create_dir_all(&slot)?;
            match contents {
                Some(bytes) => {
                    fs::write(&kept, bytes)?;
                    fs::remove_file(path)?;
                }
                None => fs::rename(path, &kept)?,
            }
            Ok(())
        })();
        if let Err(error) = result {
            // Nothing was changed; the slot holds no payload to put back.
            let _ = fs::remove_dir_all(&slot);
            return Err(ActionError::Io {
                path: path.to_path_buf(),
                error,
            });
        }
        Self::write_entry(&HistoryEntry {
            id,
            action: action.to_string(),
            at: unix_now(),
            op: "keep".into(),
            path: path.to_path_buf(),
            other: None,
            file: slot,
        })
        .map_err(ActionError::History)
    }

    /// Put an entry back where it came from.
    ///
    /// # Errors
    ///
    /// The path it came from now holds something else, or the entry is gone.
    pub fn restore(&self, id: &str, path: &Path) -> Result<(), ActionError> {
        let slot = self.dir.join(id);
        let kept = slot.join("payload");
        if fs::symlink_metadata(path).is_ok() {
            return Err(ActionError::Io {
                path: path.to_path_buf(),
                error: io::Error::from(io::ErrorKind::AlreadyExists),
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| ActionError::Io {
                path: parent.to_path_buf(),
                error,
            })?;
        }
        fs::rename(&kept, path).map_err(|error| ActionError::Io {
            path: path.to_path_buf(),
            error,
        })?;
        fs::write(slot.join("restored"), unix_now().to_string()).map_err(ActionError::History)
    }

    /// Record an operation that did not keep anything: what it wrote, where.
    ///
    /// # Errors
    ///
    /// The history's directory could not be written.
    pub fn record(&self, action: &str, op: &Operation) -> Result<(), ActionError> {
        let (verb, path, other) = match op {
            Operation::MakeDir { path, .. } => ("make-dir", path, None),
            Operation::RemoveDir { path } => ("remove-dir", path, None),
            Operation::Write { path, .. } => ("write", path, None),
            Operation::Copy { from, to } => ("copy", to, Some(from)),
            Operation::Discard { path, .. } => ("discard", path, None),
            Operation::Link { target, path, .. } => ("link", path, Some(target)),
            // A keep is recorded by `stash`, which holds the entry's id.
            Operation::Keep { path, .. } => ("keep", path, None),
        };
        let id = self.mint().map_err(ActionError::History)?;
        let slot = self.dir.join(&id);
        fs::create_dir_all(&slot).map_err(ActionError::History)?;
        Self::write_entry(&HistoryEntry {
            id,
            action: action.to_string(),
            at: unix_now(),
            op: verb.into(),
            path: path.clone(),
            other: other.cloned(),
            file: slot,
        })
        .map_err(ActionError::History)
    }

    fn write_entry(entry: &HistoryEntry) -> io::Result<()> {
        let body = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
        fs::write(entry.file.join(ENTRY_FILE), body)
    }

    /// Every entry, newest first.
    #[must_use]
    pub fn entries(&self) -> Vec<HistoryEntry> {
        let mut out: Vec<HistoryEntry> = Vec::new();
        let Ok(dir) = fs::read_dir(&self.dir) else {
            return out;
        };
        for slot in dir.flatten() {
            let file = slot.path().join(ENTRY_FILE);
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            if let Ok(mut entry) = serde_json::from_str::<HistoryEntry>(&text) {
                entry.file = slot.path();
                out.push(entry);
            }
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        out
    }

    /// The newest entry that moved `path` into the history.
    #[must_use]
    pub fn latest_for(&self, path: &Path) -> Option<String> {
        self.entries()
            .into_iter()
            .find(|entry| entry.op == "keep" && entry.path == path)
            .map(|entry| entry.id)
    }

    /// The operations of one invocation, in the order they happened.
    /// [`undo`] walks them backwards from there.
    #[must_use]
    pub fn undo_set(&self, id: &str) -> Option<Vec<UndoEntry>> {
        let entries: Vec<HistoryEntry> = self
            .entries()
            .into_iter()
            .filter(|entry| entry.action == id)
            .collect();
        if entries.is_empty() {
            return None;
        }
        let restored = |entry: &HistoryEntry| -> bool {
            fs::read_to_string(entry.file.join("restored")).is_ok()
        };
        // `entries()` answers newest first; undoing reads them oldest first
        // and walks back from there.
        let mut done: Vec<HistoryEntry> = entries
            .into_iter()
            .filter(|entry| !restored(entry))
            .collect();
        done.reverse();
        Some(
            done.into_iter()
                .map(|entry| match entry.op.as_str() {
                    "make-dir" => UndoEntry::MakeDir { path: entry.path },
                    "remove-dir" => UndoEntry::RemoveDir { path: entry.path },
                    "keep" => UndoEntry::Kept {
                        id: entry.id,
                        path: entry.path,
                    },
                    "discard" => UndoEntry::Discarded { path: entry.path },
                    "copy" => UndoEntry::Copy {
                        from: entry.other.unwrap_or_default(),
                        to: entry.path,
                    },
                    "link" => UndoEntry::Link {
                        path: entry.path,
                        symbolic: true,
                    },
                    // What was written, and an operation an older history
                    // wrote a verb for that this build no longer knows: the
                    // file it left is what undoing takes away.
                    _ => UndoEntry::Write { path: entry.path },
                })
                .collect(),
        )
    }
}

fn plan_copy(op: &CopyOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::new();
    let into = destination_is_dir(&op.destination, op.sources.len());
    for source in &op.sources {
        let to = if into {
            let name = source
                .file_name()
                .ok_or_else(|| Refusal::Missing(source.clone()))?;
            op.destination.join(name)
        } else {
            op.destination.clone()
        };
        let meta = fs::symlink_metadata(source).map_err(|_| Refusal::Missing(source.clone()))?;
        if meta.is_dir() {
            if !op.recursive {
                return Err(Refusal::IsDirectory(source.clone()));
            }
            if to.starts_with(source) {
                return Err(Refusal::IntoItself(source.clone()));
            }
        }
        if op.no_clobber && fs::symlink_metadata(&to).is_ok() {
            return Err(Refusal::Exists(to));
        }
        if op.update && is_newer(&to, source) {
            continue;
        }
        plan = plan.writes(to.clone());
        if fs::symlink_metadata(&to).is_ok() {
            plan = plan.push(Operation::Keep {
                path: to.clone(),
                contents: None,
            });
        }
        plan = plan.push(Operation::Copy {
            from: source.clone(),
            to,
        });
    }
    Ok(plan)
}

fn plan_move(op: &MoveOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::new();
    let into = destination_is_dir(&op.destination, op.sources.len());
    for source in &op.sources {
        let to = if into {
            let name = source
                .file_name()
                .ok_or_else(|| Refusal::Missing(source.clone()))?;
            op.destination.join(name)
        } else {
            op.destination.clone()
        };
        let meta = fs::symlink_metadata(source).map_err(|_| Refusal::Missing(source.clone()))?;
        if meta.is_dir() && to.starts_with(source) {
            return Err(Refusal::IntoItself(source.clone()));
        }
        if op.no_clobber && fs::symlink_metadata(&to).is_ok() {
            return Err(Refusal::Exists(to));
        }
        plan = plan.writes(to.clone());
        if fs::symlink_metadata(&to).is_ok() {
            plan = plan.push(Operation::Keep {
                path: to.clone(),
                contents: None,
            });
        }
        plan = plan.push(Operation::Copy {
            from: source.clone(),
            to: to.clone(),
        });
        // The source is what a `mv` gives up: kept, not unlinked.
        plan = plan.push(Operation::Keep {
            path: source.clone(),
            contents: None,
        });
    }
    Ok(plan)
}

fn plan_remove(op: &RemoveOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::new();
    for path in &op.paths {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            // GNU's `rm -f` says nothing about what was not there.
            Err(_) if op.force => continue,
            Err(_) => return Err(Refusal::Missing(path.clone())),
        };
        if meta.is_dir() && !op.recursive {
            return Err(Refusal::CannotRemove(path.clone()));
        }
        plan = plan.writes(path.clone());
        if op.force {
            // `--force` is the caller saying it does not want this back, so
            // this is the one operation that really removes.
            plan = plan.push(Operation::Discard {
                path: path.clone(),
                recursive: op.recursive,
            });
        } else {
            plan = plan.push(Operation::Keep {
                path: path.clone(),
                contents: None,
            });
        }
    }
    Ok(plan)
}

fn plan_make_dir(op: &MakeDirOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::new();
    for dir in &op.dirs {
        if fs::symlink_metadata(dir).is_ok() {
            // `-p` on a directory that is already there is a no-op; without
            // it, GNU refuses.
            if op.parents && dir.is_dir() {
                continue;
            }
            return Err(Refusal::Exists(dir.clone()));
        }
        // Every directory `-p` has to make, outermost first, the directory
        // itself last: one record each, so undo takes back exactly those.
        let made = if op.parents {
            missing_ancestors(dir)
        } else {
            vec![dir.clone()]
        };
        for path in made {
            plan = plan.writes(path.clone()).push(Operation::MakeDir {
                path,
                parents: false,
            });
        }
    }
    Ok(plan)
}

fn plan_remove_dir(op: &RemoveDirOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::new();
    for dir in &op.dirs {
        let meta = fs::symlink_metadata(dir).map_err(|_| Refusal::Missing(dir.clone()))?;
        if !meta.is_dir() {
            return Err(Refusal::NotADirectory(dir.clone()));
        }
        if has_entries(dir) {
            return Err(Refusal::NotEmpty(dir.clone()));
        }
        let mut targets = vec![dir.clone()];
        if op.parents {
            targets.extend(existing_ancestors_are_empty(dir));
        }
        for path in targets {
            plan = plan
                .writes(path.clone())
                .push(Operation::RemoveDir { path });
        }
    }
    Ok(plan)
}

fn plan_link(op: &LinkOp) -> Result<Plan, Refusal> {
    let path = if op.into_directory {
        let name = op
            .target
            .file_name()
            .ok_or_else(|| Refusal::Missing(op.target.clone()))?;
        op.path.join(name)
    } else {
        op.path.clone()
    };
    let mut plan = Plan::new().writes(path.clone());
    if fs::symlink_metadata(&path).is_ok() {
        if !op.force {
            return Err(Refusal::Exists(path));
        }
        plan = plan.push(Operation::Keep {
            path: path.clone(),
            contents: None,
        });
    }
    Ok(plan.push(Operation::Link {
        target: op.target.clone(),
        path,
        symbolic: op.symbolic,
    }))
}

fn plan_truncate(op: &TruncateOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::new();
    for file in &op.files {
        match fs::symlink_metadata(file) {
            Ok(meta) => {
                if meta.is_dir() {
                    return Err(Refusal::IsDirectory(file.clone()));
                }
                let contents = fs::read(file).map_err(|_| Refusal::Missing(file.clone()))?;
                plan = plan
                    .writes(file.clone())
                    .push(Operation::Keep {
                        path: file.clone(),
                        contents: Some(contents),
                    })
                    .push(Operation::Write {
                        path: file.clone(),
                        contents: sized(op.size),
                    });
            }
            Err(_) if op.create => {
                plan = plan.writes(file.clone()).push(Operation::Write {
                    path: file.clone(),
                    contents: sized(op.size),
                });
            }
            Err(_) => return Err(Refusal::Missing(file.clone())),
        }
    }
    Ok(plan)
}

/// `size` bytes of zeroes, which is what `truncate` writes.
fn sized(size: u64) -> Vec<u8> {
    vec![0u8; usize::try_from(size).unwrap_or(usize::MAX)]
}

fn destination_is_dir(destination: &Path, sources: usize) -> bool {
    if destination.is_dir() {
        return true;
    }
    // A destination ending in a separator names a directory even before it
    // exists, as GNU reads it.
    sources > 1 || destination.as_os_str().to_string_lossy().ends_with('/')
}

fn is_newer(destination: &Path, source: &Path) -> bool {
    let stamp = |path: &Path| fs::metadata(path).and_then(|m| m.modified()).ok();
    match (stamp(destination), stamp(source)) {
        (Some(dest), Some(src)) => dest > src,
        _ => false,
    }
}

/// The directories under `path` that `mkdir -p` has to make, outermost first,
/// stopping at the first that exists and is not a symlink.
fn missing_ancestors(path: &Path) -> Vec<PathBuf> {
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut current = path.to_path_buf();
    while fs::symlink_metadata(&current).is_err() {
        if current.parent().is_none() {
            break;
        }
        missing.push(current.clone());
        if !current.pop() {
            break;
        }
    }
    missing.reverse();
    missing
}

/// The existing ancestors of `dir` that `rmdir -p` may also remove, nearest
/// first, as long as each is empty.
fn existing_ancestors_are_empty(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut current = dir.to_path_buf();
    while current.pop() {
        // `/` and `.` are never removed, whatever the flags say.
        if current.as_os_str().is_empty()
            || current.components().count() <= 1
            || !current.is_dir()
            || has_entries(&current)
        {
            break;
        }
        out.push(current.clone());
    }
    out
}

fn has_entries(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

fn create_dir(path: &Path, parents: bool) -> io::Result<()> {
    if parents {
        fs::create_dir_all(path)
    } else {
        fs::create_dir(path)
    }
}

fn write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// Copy a file or a whole tree, following the tree but not the symbolic links
/// inside it.
/// Remove a path without keeping it.
fn discard(path: &Path, recursive: bool) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && !recursive {
        return Err(io::Error::other("is a directory"));
    }
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to).map(|_| ())
    }
}

fn link(target: &Path, path: &Path, symbolic: bool) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if symbolic {
        std::os::unix::fs::symlink(target, path)
    } else {
        fs::hard_link(target, path)
    }
}

/// The answer for a filesystem with no environment record behind it (the
/// device's own shell): every path the person may reach is theirs to act on.
#[must_use]
pub fn no_grants(_: &Path) -> bool {
    true
}

/// Answer a file action against an environment's grants, and carry it out.
///
/// This is the whole of what the supervisor does for one: the same
/// [`Policy`] every other path question goes through, the same
/// [`Recoverable`] history, and — by the caller — the same [`AuditLog`]
/// line.
///
/// # Errors
///
/// The refusal that stopped it before it changed anything, or the
/// filesystem error that stopped it partway.
pub fn perform(
    action: &FileAction,
    policy: &Policy,
    history: &Recoverable,
    now: Option<u64>,
) -> Result<Outcome, FileActionError> {
    let allowed = |path: &Path| policy.permits_in(path, collab_types::caps::Access::Write, now);
    let plan = action.plan(&allowed).map_err(FileActionError::Refused)?;
    action
        .apply(&plan, history)
        .map_err(FileActionError::Failed)
}

/// What the action was about, for the audit log's `subject`: its paths as
/// they were written, so the log names one file once.
#[must_use]
pub fn subjects(action: &FileAction) -> String {
    action
        .paths()
        .iter()
        .map(|path| normalize(path).display().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The audit line for a file action that was carried out.
#[must_use]
pub fn audit_record(
    session: &str,
    environment: &str,
    action: &FileAction,
) -> crate::audit::AuditRecord {
    crate::audit::AuditRecord::new(session, environment, action.verb(), true)
        .about(subjects(action))
}

/// Lexically normalise a path: no `.` or `..` segments, no doubled
/// separators, so the same file is one subject in the log.
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

/// Write one line for `action` to the audit log.
pub fn log(log: &AuditLog, session: &str, environment: &str, action: &FileAction) {
    log.append(&audit_record(session, environment, action));
}

#[cfg(test)]
mod tests;

/// Where an environment's recoverable history lives: under its own `/var`,
/// so it follows the environment and never reaches another one.
#[must_use]
pub fn history_in(environment: &EnvironmentHandle) -> PathBuf {
    environment.var_dir.join("lib").join(RECOVERABLE_IN_VAR)
}

/// The broker's answer to [`Request::FileAction`](crate::broker::Request):
/// check the action against the environment's grants, carry it out, and
/// record it in the audit log however it went.
pub fn brokered(state: &BrokerState, action: &FileAction) -> Response {
    let history = Recoverable::at(history_in(&state.environment));
    let policy = state.environment.policy();
    match perform(action, &policy, &history, Some(unix_now())) {
        Ok(recorded) => {
            log(
                &state.audit,
                &state.session_id,
                &state.environment.key,
                action,
            );
            Response::FileAction {
                id: recorded.id,
                operations: recorded.done.len(),
            }
        }
        Err(error) => {
            let reason = error.to_string();
            deny(state, action.verb(), &subjects(action), &reason);
            Response::Denied { reason }
        }
    }
}

/// The broker's answer to [`Request::UndoFileAction`](crate::broker::Request):
/// put one recorded invocation back, latest operation first.
pub fn brokered_undo(state: &BrokerState, id: &str) -> Response {
    let history = Recoverable::at(history_in(&state.environment));
    let Some(entries) = history.undo_set(id) else {
        let reason = format!("{id}: no such action in the recoverable history");
        deny(state, "file-action-undo", id, &reason);
        return Response::Denied { reason };
    };
    match undo(&entries, &history) {
        Ok(()) => {
            state.audit.append(
                &AuditRecord::new(
                    &state.session_id,
                    &state.environment.key,
                    "file-action-undo",
                    true,
                )
                .about(id),
            );
            Response::Undone {
                id: id.to_string(),
                operations: entries.len(),
            }
        }
        Err(error) => {
            let reason = error.to_string();
            deny(state, "file-action-undo", id, &reason);
            Response::Denied { reason }
        }
    }
}

fn deny(state: &BrokerState, request: &str, subject: &str, reason: &str) {
    state.audit.append(
        &AuditRecord::new(&state.session_id, &state.environment.key, request, false)
            .about(subject)
            .because(reason),
    );
}
