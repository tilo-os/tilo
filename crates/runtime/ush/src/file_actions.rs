//! Carrying out an environment's file actions.
//!
//! `cp`, `mv`, `rm`, `mkdir`, `rmdir`, `ln`, `truncate`, `touch` and `append`
//! inside a guest are the environment record's own verbs
//! ([`tilo_environment::files`]): each command line parses into one
//! [`FileAction`] ([`cli`]), is invoked on the record as the person who typed
//! it ([`invoke`]), and lands in the record's `files` outbox — checked as a
//! whole against the record's grants by the verb itself. This module is the
//! device's half: it [plans](plan) a queued task against the tree as it is,
//! checks that plan against the same grants with the same function, carries
//! it out, and settles the task with the [`UndoEntry`]s that take it back.
//! History, undo and attribution are the record's, as for every other action
//! of the record's: there is no second history here.
//!
//! # Why one action per invocation
//!
//! A shell one-liner that copies forty files is one thing the person asked
//! for. Refusing the seventeenth file would leave thirty-nine copies and no
//! way to tell which; recording each `open` would fill History with the
//! implementation of one intent. So the action is checked before anything is
//! written, and written as one unit.
//!
//! # Nothing is unlinked
//!
//! `rm` renames the path into the environment's recoverable store
//! ([`RECOVERABLE_DIR`]) instead of unlinking it, and an overwrite (`cp`,
//! `mv`, `ln -f`, `truncate`, `append`) moves the previous contents aside the
//! same way. `rmdir` cannot lose anything — it only succeeds on an empty
//! directory — so it removes the entry and its undo recreates the directory.
//! `rm -f` is GNU's: it says nothing of a path that is not there, and what is
//! there is kept aside like any removal.
//!
//! The store is where kept contents are, not a history: each entry names the
//! path it came from, so what a task the record no longer lists kept aside —
//! and what an older runtime kept, when it still wrote its own history — stays
//! listed by `history` and comes back with the record's `restore` action.
//!
//! # Outside a guest
//!
//! The host's own shell has no environment record, so there is nothing to
//! invoke an action on, no grants to check and no History to record in: a
//! verb run there ([`perform`]) carries out the same plan with the person's
//! own permissions as its only bound, still keeping what it replaces in
//! `~/.local/share/ush/recoverable`, where `history` lists it and
//! `history restore` puts it back.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub mod cli;
pub mod command;
pub mod watch;

use collab_types::caps::Access;
use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;
use tilo_environment::Environment;
pub use tilo_environment::files::{
    AppendFileOp, CopyOp, FileAction, FileTask, LinkOp, MakeDirOp, MoveOp, RemoveDirOp, RemoveOp,
    RestoreOp, RevertOp, TruncateOp, UndoEntry, normalize,
};
use tilo_resource::{Id, Invoke, Replica, Settled};

use crate::audit::{AuditLog, AuditRecord};
use crate::broker::{BrokerState, EnvironmentHandle, Response};
use crate::policy::Policy;
use crate::records::{Invoker, RecordError, Records};

/// Where a removed path (or the previous contents of an overwritten one) is
/// kept instead of being unlinked: under the environment's own `/var`, so it
/// lives and dies with the environment and never collides with the guest's
/// own `/var/lib`.
pub const RECOVERABLE_DIR: &str = "/var/lib/tilo-recoverable";

/// The same directory as the environment's `/var` holds it, which is how the
/// supervisor finds it on the host.
pub const RECOVERABLE_IN_VAR: &str = "tilo-recoverable";

/// Metadata kept beside every kept entry: the path it came from.
pub const ENTRY_FILE: &str = "entry.json";

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
    /// `rmdir` of a directory that still holds something.
    NotEmpty(PathBuf),
    /// `rm` of something that is neither a file nor `-r`. GNU's wording.
    CannotRemove(PathBuf),
    /// A revert or a restore names nothing that can be put back.
    Nothing(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ungranted(path) => f.write_str(&tilo_environment::files::ungranted(path)),
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
            Self::Nothing(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Refusal {}

/// Why an action stopped partway.
#[derive(Debug)]
pub enum ActionError {
    Io { path: PathBuf, error: io::Error },
    Store(io::Error),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Store(error) => write!(f, "cannot keep what it replaces: {error}"),
        }
    }
}

impl std::error::Error for ActionError {}

/// Either half of what carrying an action out can answer with: refused
/// before it did anything, or stopped partway by the filesystem.
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

/// One thing to do, in the order to do it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Create a directory.
    MakeDir {
        path: PathBuf,
    },
    /// Remove an empty directory.
    RemoveDir {
        path: PathBuf,
    },
    /// Move what is at `path` into the recoverable store. `contents` is the
    /// data to keep when the path is rewritten in place (a truncated file),
    /// so what is kept is what was there.
    Keep {
        path: PathBuf,
        contents: Option<Vec<u8>>,
    },
    /// Create or replace `path` with `contents`.
    Write {
        path: PathBuf,
        contents: Vec<u8>,
    },
    /// Create `path`, empty, only where nothing is there yet.
    MakeFile {
        path: PathBuf,
    },
    /// Leave `path` as it is: `touch` on a file that is already there.
    Touch {
        path: PathBuf,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Every path it reads from, in order.
    pub reads: Vec<PathBuf>,
    /// Every path it writes to, creates or gives up, in order.
    pub writes: Vec<PathBuf>,
    pub ops: Vec<Operation>,
}

impl Plan {
    fn reads(mut self, path: impl Into<PathBuf>) -> Self {
        self.reads.push(path.into());
        self
    }

    fn writes(mut self, path: impl Into<PathBuf>) -> Self {
        self.writes.push(path.into());
        self
    }

    fn push(mut self, op: Operation) -> Self {
        self.ops.push(op);
        self
    }

    /// The first path `policy` does not allow as of `now`, if any: the
    /// same question the verb asked of the paths it was named, asked again
    /// of every path the plan found on the tree.
    #[must_use]
    pub fn refused_by(&self, policy: &Policy, now: Option<u64>) -> Option<Refusal> {
        self.reads
            .iter()
            .find(|path| !policy.permits_in(path, Access::Read, now))
            .or_else(|| {
                self.writes
                    .iter()
                    .find(|path| !policy.permits_in(path, Access::Write, now))
            })
            .map(|path| Refusal::Ungranted(path.clone()))
    }
}

/// Plan `action` against the tree as it is. A revert or a restore is not
/// planned here; [`carry_out`] reads what it puts back.
///
/// # Errors
///
/// The reason nothing can be done: a missing source, a destination that
/// cannot take the plan.
pub fn plan(action: &FileAction) -> Result<Plan, Refusal> {
    match action {
        FileAction::Copy(op) => plan_copy(op),
        FileAction::Move(op) => plan_move(op),
        FileAction::Remove(op) => plan_remove(op),
        FileAction::MakeDir(op) => plan_make_dir(op),
        FileAction::RemoveDir(op) => plan_remove_dir(op),
        FileAction::Link(op) => plan_link(op),
        FileAction::Truncate(op) => plan_truncate(op),
        FileAction::Touch(op) => plan_touch(op),
        FileAction::Append(op) => plan_append(op),
        FileAction::Revert(_) | FileAction::Restore(_) => Ok(Plan::default()),
    }
}

/// Carry out `plan`, keeping what it replaces in `store`. The answer is
/// every operation done, in order, which is what undoing walks backwards;
/// on failure, the error and the operations done before it, so a task that
/// stopped partway can still be taken back.
///
/// # Errors
///
/// The filesystem error that stopped it, with what was done before it.
pub fn apply(
    plan: &Plan,
    store: &Recoverable,
) -> Result<Vec<UndoEntry>, (ActionError, Vec<UndoEntry>)> {
    let mut done = Vec::new();
    for op in &plan.ops {
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |error| ActionError::Io { path, error }
        };
        let step = match op {
            Operation::MakeDir { path } => fs::create_dir(path)
                .map_err(io(path))
                .map(|()| UndoEntry::MakeDir { path: path.clone() }),
            Operation::RemoveDir { path } => fs::remove_dir(path)
                .map_err(io(path))
                .map(|()| UndoEntry::RemoveDir { path: path.clone() }),
            Operation::Keep { path, contents } => {
                store
                    .stash(path, contents.as_deref())
                    .map(|id| UndoEntry::Kept {
                        id,
                        path: path.clone(),
                    })
            }
            Operation::Touch { .. } => Ok(UndoEntry::Touched),
            Operation::MakeFile { path } => create_file(path)
                .map_err(io(path))
                .map(|()| UndoEntry::MakeFile { path: path.clone() }),
            Operation::Write { path, contents } => write_file(path, contents)
                .map_err(io(path))
                .map(|()| UndoEntry::Write { path: path.clone() }),
            Operation::Copy { from, to } => {
                copy_tree(from, to)
                    .map_err(io(to))
                    .map(|()| UndoEntry::Copy {
                        from: from.clone(),
                        to: to.clone(),
                    })
            }
            Operation::Link {
                target,
                path,
                symbolic,
            } => link(target, path, *symbolic)
                .map_err(io(path))
                .map(|()| UndoEntry::Link {
                    path: path.clone(),
                    symbolic: *symbolic,
                }),
        };
        match step {
            Ok(entry) => done.push(entry),
            Err(error) => return Err((error, done)),
        }
    }
    Ok(done)
}

/// Undo a set of entries, latest first.
///
/// What can be put back is; an entry whose path has changed since (a file
/// another process wrote) is reported, not overwritten.
///
/// # Errors
///
/// The first path that could not be put back.
pub fn undo(entries: &[UndoEntry], store: &Recoverable) -> Result<(), ActionError> {
    // What was kept goes back last, so a `mv` restores its source after the
    // copy it made has gone.
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
            UndoEntry::Kept { id, path } => store.restore(id, path)?,
            UndoEntry::Restored { id, path } => store.put_back(id, path)?,
            UndoEntry::Touched => {}
            UndoEntry::MakeFile { path }
            | UndoEntry::Write { path }
            | UndoEntry::Copy { to: path, .. }
            | UndoEntry::Link { path, .. } => remove_if_present(path)?,
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

/// The recoverable store: a directory of entries, one per removed or
/// replaced path, each holding what was there and naming where it came from.
#[derive(Debug, Clone)]
pub struct Recoverable {
    dir: PathBuf,
}

/// What one entry of the store remembers. Entries an older runtime wrote
/// for every operation it did read the same way; only `keep` entries hold
/// anything to put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptEntry {
    /// The entry's id: when it was kept, and a sequence so two in the same
    /// second do not collide.
    pub id: String,
    /// The invocation an older runtime recorded it under.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    /// Seconds since the Unix epoch.
    pub at: u64,
    /// `keep` for an entry holding contents.
    pub op: String,
    /// The path the entry came from.
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<PathBuf>,
    /// The entry's directory in the store.
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
        Err(io::Error::other("the recoverable store is full"))
    }

    /// Move `path` into the store, keeping what was there; the entry's id.
    ///
    /// # Errors
    ///
    /// Whatever kept the move from happening — in which case nothing was
    /// changed.
    pub fn stash(&self, path: &Path, contents: Option<&[u8]>) -> Result<String, ActionError> {
        let id = self.mint().map_err(ActionError::Store)?;
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
        let entry = KeptEntry {
            id: id.clone(),
            action: String::new(),
            at: unix_now(),
            op: "keep".into(),
            path: path.to_path_buf(),
            other: None,
            file: slot,
        };
        let body = serde_json::to_vec_pretty(&entry).map_err(io::Error::other);
        body.and_then(|body| fs::write(entry.file.join(ENTRY_FILE), body))
            .map_err(ActionError::Store)?;
        Ok(id)
    }

    /// Put the entry `id` back at `path`.
    ///
    /// # Errors
    ///
    /// The path now holds something else, or the entry is gone.
    pub fn restore(&self, id: &str, path: &Path) -> Result<(), ActionError> {
        let slot = self.slot(id)?;
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
        fs::write(slot.join("restored"), unix_now().to_string()).map_err(ActionError::Store)
    }

    /// Move what [`Self::restore`] put back at `path` into its entry `id`
    /// again: the undo of a restore.
    ///
    /// # Errors
    ///
    /// The entry already holds something, or the path cannot be moved.
    pub fn put_back(&self, id: &str, path: &Path) -> Result<(), ActionError> {
        let slot = self.slot(id)?;
        let kept = slot.join("payload");
        if fs::symlink_metadata(&kept).is_ok() {
            return Err(ActionError::Io {
                path: kept,
                error: io::Error::from(io::ErrorKind::AlreadyExists),
            });
        }
        fs::rename(path, &kept).map_err(|error| ActionError::Io {
            path: path.to_path_buf(),
            error,
        })?;
        match fs::remove_file(slot.join("restored")) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(ActionError::Store(error)),
            _ => Ok(()),
        }
    }

    /// Hold the store for one device's carrying out: two sessions of one
    /// environment never carry the same task out twice. Released when the
    /// answer is dropped.
    ///
    /// # Errors
    ///
    /// The store's directory cannot be made or its lock taken.
    pub fn lock(&self) -> io::Result<advisory_lock::FileLock> {
        fs::create_dir_all(&self.dir)?;
        advisory_lock::FileLock::open(&self.dir.join(".lock"), advisory_lock::LockMode::Exclusive)
    }

    /// The directory of the entry `id`, refusing an id that is not one.
    fn slot(&self, id: &str) -> Result<PathBuf, ActionError> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(ActionError::Store(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{id:?} is not a kept entry's id"),
            )));
        }
        Ok(self.dir.join(id))
    }

    /// Every entry still holding what it kept, newest first: what `history`
    /// lists and `restore` takes.
    #[must_use]
    pub fn kept(&self) -> Vec<KeptEntry> {
        let mut out: Vec<KeptEntry> = Vec::new();
        let Ok(dir) = fs::read_dir(&self.dir) else {
            return out;
        };
        for slot in dir.flatten() {
            let Ok(text) = fs::read_to_string(slot.path().join(ENTRY_FILE)) else {
                continue;
            };
            let Ok(mut entry) = serde_json::from_str::<KeptEntry>(&text) else {
                continue;
            };
            if entry.op != "keep" || slot.path().join("restored").exists() {
                continue;
            }
            entry.file = slot.path();
            out.push(entry);
        }
        out.sort_by(|a, b| b.id.cmp(&a.id));
        out
    }

    /// The kept entry `id`, while it still holds what it kept.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<KeptEntry> {
        self.kept().into_iter().find(|entry| entry.id == id)
    }
}

/// What a device answers for one queued task: carried out with what it did,
/// or refused or stopped with why and what it did before.
pub type Settlement = Settled<Vec<UndoEntry>>;

/// Carry out one queued task under `policy` as of `now`. `earlier` reads how
/// another task of the record settled: what a revert takes back.
///
/// Nothing is done unless all of it is allowed: the plan found on the tree
/// is checked as a whole before its first operation.
pub fn carry_out(
    action: &FileAction,
    policy: &Policy,
    store: &Recoverable,
    now: Option<u64>,
    earlier: &dyn Fn(&str) -> Option<Settlement>,
) -> Settlement {
    match action {
        FileAction::Revert(RevertOp { entry }) => {
            let Some(settled) = earlier(entry) else {
                return Settled::failed(format!("{entry}: nothing recorded to take back"));
            };
            let entries = settled.result();
            if let Some(path) = entries
                .iter()
                .filter_map(UndoEntry::path)
                .find(|path| !policy.permits_in(path, Access::Write, now))
            {
                return Settled::failed(Refusal::Ungranted(path.to_path_buf()).to_string());
            }
            match undo(entries, store) {
                Ok(()) => Settled::Done(Vec::new()),
                Err(error) => Settled::failed(error.to_string()),
            }
        }
        FileAction::Restore(RestoreOp { kept }) => {
            let Some(entry) = store.entry(kept) else {
                return Settled::failed(format!("{kept}: nothing kept under that id"));
            };
            if !policy.permits_in(&entry.path, Access::Write, now) {
                return Settled::failed(Refusal::Ungranted(entry.path).to_string());
            }
            match store.restore(kept, &entry.path) {
                Ok(()) => Settled::Done(vec![UndoEntry::Restored {
                    id: kept.clone(),
                    path: entry.path,
                }]),
                Err(error) => Settled::failed(error.to_string()),
            }
        }
        action => match perform(action, policy, store, now) {
            Ok(done) => Settled::Done(done),
            Err((error, partial)) => Settled::Failed {
                reason: error.to_string(),
                partial,
            },
        },
    }
}

/// Plan `action`, check the plan against `policy` as a whole, and carry it
/// out: the whole of what a device does for one copy, move or removal.
///
/// # Errors
///
/// The refusal that stopped it before it changed anything, or the
/// filesystem error that stopped it partway, with what was done before it.
pub fn perform(
    action: &FileAction,
    policy: &Policy,
    store: &Recoverable,
    now: Option<u64>,
) -> Result<Vec<UndoEntry>, (FileActionError, Vec<UndoEntry>)> {
    let plan = plan(action).map_err(|refusal| (FileActionError::Refused(refusal), Vec::new()))?;
    if let Some(refusal) = plan.refused_by(policy, now) {
        return Err((FileActionError::Refused(refusal), Vec::new()));
    }
    apply(&plan, store).map_err(|(error, done)| (FileActionError::Failed(error), done))
}

/// The answer to one invocation on the record.
#[derive(Debug, Clone, PartialEq)]
pub struct Invoked {
    /// The outbox entry the invocation queued.
    pub entry: Option<Id>,
    /// How it settled here, once this device carried it out.
    pub settled: Option<Settlement>,
}

/// Invoke `invoke` on the environment `key` as `invoker` (when there is
/// one), then carry out every task the record holds for `device`, settling
/// each with what it did: one write of the record.
///
/// The invocation goes through the record's action door, so it is checked
/// against the record's grants by the verb, attributed to `invoker` and
/// recorded in the record's History with the compensation its undo applies.
///
/// # Errors
///
/// The verb's refusal (nothing is queued or written), or a record that
/// cannot be read or written.
pub fn invoke(
    records: &Records,
    key: &str,
    invoker: &Invoker,
    run: Option<&crate::broker::RunGroup>,
    invoke: Option<Invoke<Environment>>,
    device: &str,
    store: &Recoverable,
) -> Result<Invoked, RecordError> {
    let _held = store
        .lock()
        .map_err(|e| RecordError::Store(format!("the recoverable store: {e}")))?;
    records.act_within(key, invoker, run, |replica| {
        let entry = match invoke {
            Some(invoke) => {
                let envelope = replica
                    .apply_confirmed(invoke)
                    .map_err(|e| RecordError::Refused(e.to_string()))?;
                tilo_resource::minted_ids(envelope).next()
            }
            None => None,
        };
        let settled = drain(replica, device, store)?;
        let settled = entry.as_ref().and_then(|entry| {
            settled
                .into_iter()
                .find_map(|(id, settled)| (id == *entry).then_some(settled))
        });
        Ok(Invoked { entry, settled })
    })
}

/// Carry out every task `replica`'s record holds for `device`, oldest
/// first, each under the record's grants as they stand, and settle it.
///
/// # Errors
///
/// A record that cannot be read, or a settlement it refuses.
pub fn drain(
    replica: &mut Replica<Environment>,
    device: &str,
    store: &Recoverable,
) -> Result<Vec<(Id, Settlement)>, RecordError> {
    let mut settled = Vec::new();
    loop {
        let state = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        let Some((id, task)) = state
            .files
            .pending()
            .find(|(id, task)| task.device == device && !settled.iter().any(|(s, _)| s == *id))
            .map(|(id, task)| (id.clone(), task.clone()))
        else {
            return Ok(settled);
        };
        let policy = Policy::new(state.grants.clone());
        let earlier = |entry: &str| state.files.outcome(&Id::from_raw(entry)).cloned();
        let outcome = carry_out(&task.action, &policy, store, Some(unix_now()), &earlier);
        replica
            .apply(Environment::paths().files.settle(&id, &outcome))
            .map_err(|e| RecordError::Refused(e.to_string()))?;
        settled.push((id, outcome));
    }
}

/// What the action was about, for the audit log's `subject`: its paths as
/// they were written, so the log names one file once.
#[must_use]
pub fn subjects(action: &FileAction) -> String {
    match action {
        FileAction::Revert(RevertOp { entry }) => entry.clone(),
        FileAction::Restore(RestoreOp { kept }) => kept.clone(),
        action => action
            .paths()
            .iter()
            .map(|path| normalize(path).display().to_string())
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The audit line for a file action that was carried out.
#[must_use]
pub fn audit_record(session: &str, environment: &str, action: &FileAction) -> AuditRecord {
    AuditRecord::new(session, environment, action.verb(), true).about(subjects(action))
}

/// Write one line for `action` to the audit log.
pub fn log(log: &AuditLog, session: &str, environment: &str, action: &FileAction) {
    log.append(&audit_record(session, environment, action));
}

/// Where an environment's recoverable store lives: under its own `/var`,
/// so it follows the environment and never reaches another one.
#[must_use]
pub fn store_in(environment: &EnvironmentHandle) -> PathBuf {
    environment.var_dir.join("lib").join(RECOVERABLE_IN_VAR)
}

/// The broker's answer to [`Request::FileAction`](crate::broker::Request):
/// invoke the record's verb as the session's person, carry it out on this
/// device, and record it in the audit log however it went.
pub fn brokered(
    state: &BrokerState,
    action: &FileAction,
    run: Option<&crate::broker::RunGroup>,
) -> Response {
    let store = Recoverable::at(store_in(&state.environment));
    let invoker = Invoker::Person(state.person.clone());
    let answer = invoke(
        &state.environment.records,
        &state.environment.key,
        &invoker,
        run,
        Some(Environment::file_invoke(&state.device, action)),
        &state.device,
        &store,
    );
    let refused = |reason: String| {
        deny(state, action.verb(), &subjects(action), &reason);
        Response::Denied { reason }
    };
    match answer {
        Ok(Invoked {
            entry: Some(entry),
            settled: Some(Settled::Done(done)),
        }) => {
            log(
                &state.audit,
                &state.session_id,
                &state.environment.key,
                action,
            );
            Response::FileAction {
                id: entry.as_str().to_owned(),
                operations: done.len(),
            }
        }
        Ok(Invoked {
            settled: Some(Settled::Failed { reason, .. }),
            ..
        }) => refused(reason),
        Ok(_) => refused("the action was queued but not carried out on this device".into()),
        Err(error) => refused(error.to_string()),
    }
}

/// Carry out every task the session's environment holds for this device
/// that was queued elsewhere: an undo from History, an agent's `move`.
/// Each settles in the record and is written to the audit log.
pub fn carry_out_queued(state: &BrokerState) {
    let env = state.environment.current();
    if !env
        .files
        .pending()
        .any(|(_, task)| task.device == state.device)
    {
        return;
    }
    let store = Recoverable::at(store_in(&state.environment));
    let invoker = Invoker::Person(state.person.clone());
    let carried = store
        .lock()
        .map_err(|e| RecordError::Store(format!("the recoverable store: {e}")))
        .and_then(|_held| {
            state
                .environment
                .records
                .act(&state.environment.key, &invoker, |replica| {
                    let settled = drain(replica, &state.device, &store)?;
                    let state = replica
                        .state()
                        .map_err(|e| RecordError::Model(e.to_string()))?;
                    Ok(settled
                        .into_iter()
                        .filter_map(|(id, settled)| {
                            Some((state.files.get(&id)?.value.action.clone(), settled))
                        })
                        .collect::<Vec<_>>())
                })
        });
    match carried {
        Ok(carried) => {
            for (action, settled) in carried {
                match settled.reason() {
                    None => log(
                        &state.audit,
                        &state.session_id,
                        &state.environment.key,
                        &action,
                    ),
                    Some(reason) => deny(state, action.verb(), &subjects(&action), reason),
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "the environment's queued file actions were not carried out");
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

/// `action` with every path it names made absolute against `cwd`, the
/// directory the command was typed in. A symbolic link's target stays as it
/// was written: it is read from where the link is, not from `cwd`.
#[must_use]
pub fn anchored(action: FileAction, cwd: &Path) -> FileAction {
    let at = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            // `join` keeps a trailing separator, which says "a directory".
            cwd.join(path)
        }
    };
    let all = |paths: Vec<PathBuf>| paths.into_iter().map(at).collect();
    match action {
        FileAction::Copy(op) => FileAction::Copy(CopyOp {
            sources: all(op.sources),
            destination: at(op.destination),
            ..op
        }),
        FileAction::Move(op) => FileAction::Move(MoveOp {
            sources: all(op.sources),
            destination: at(op.destination),
            ..op
        }),
        FileAction::Remove(op) => FileAction::Remove(RemoveOp {
            paths: all(op.paths),
            ..op
        }),
        FileAction::MakeDir(op) => FileAction::MakeDir(MakeDirOp {
            dirs: all(op.dirs),
            ..op
        }),
        FileAction::RemoveDir(op) => FileAction::RemoveDir(RemoveDirOp {
            dirs: all(op.dirs),
            ..op
        }),
        FileAction::Link(op) => FileAction::Link(LinkOp {
            target: if op.symbolic {
                op.target
            } else {
                at(op.target)
            },
            path: at(op.path),
            ..op
        }),
        FileAction::Truncate(op) => FileAction::Truncate(TruncateOp {
            files: all(op.files),
            ..op
        }),
        FileAction::Touch(op) => FileAction::Touch(AppendFileOp {
            files: all(op.files),
            ..op
        }),
        FileAction::Append(op) => FileAction::Append(AppendFileOp {
            files: all(op.files),
            ..op
        }),
        other @ (FileAction::Revert(_) | FileAction::Restore(_)) => other,
    }
}

fn plan_copy(op: &CopyOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::default();
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
        plan = plan.reads(source.clone()).writes(to.clone());
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
    let mut plan = Plan::default();
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
        // The source is what a `mv` gives up: written, and kept.
        plan = plan.writes(source.clone()).writes(to.clone());
        if fs::symlink_metadata(&to).is_ok() {
            plan = plan.push(Operation::Keep {
                path: to.clone(),
                contents: None,
            });
        }
        plan = plan
            .push(Operation::Copy {
                from: source.clone(),
                to: to.clone(),
            })
            .push(Operation::Keep {
                path: source.clone(),
                contents: None,
            });
    }
    Ok(plan)
}

fn plan_remove(op: &RemoveOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::default();
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
        plan = plan.writes(path.clone()).push(Operation::Keep {
            path: path.clone(),
            contents: None,
        });
    }
    Ok(plan)
}

fn plan_make_dir(op: &MakeDirOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::default();
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
        // itself last: one entry each, so undo takes back exactly those.
        let made = if op.parents {
            missing_ancestors(dir)
        } else {
            vec![dir.clone()]
        };
        for path in made {
            plan = plan.writes(path.clone()).push(Operation::MakeDir { path });
        }
    }
    Ok(plan)
}

fn plan_remove_dir(op: &RemoveDirOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::default();
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
    let mut plan = Plan::default().writes(path.clone());
    if !op.symbolic {
        // A hard link is a second name for the target's file: writing one
        // writes the other.
        plan = plan.writes(op.target.clone());
    }
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
    let mut plan = Plan::default();
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

fn plan_touch(op: &AppendFileOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::default();
    for file in &op.files {
        match fs::symlink_metadata(file) {
            Ok(meta) => {
                if meta.is_dir() {
                    return Err(Refusal::IsDirectory(file.clone()));
                }
                // Touching what is there changes nothing: there is nothing
                // to put back.
                plan = plan
                    .writes(file.clone())
                    .push(Operation::Touch { path: file.clone() });
            }
            Err(_) => {
                plan = plan
                    .writes(file.clone())
                    .push(Operation::MakeFile { path: file.clone() });
            }
        }
    }
    Ok(plan)
}

fn plan_append(op: &AppendFileOp) -> Result<Plan, Refusal> {
    let mut plan = Plan::default();
    for file in &op.files {
        plan = plan.writes(file.clone());
        let mut contents = Vec::new();
        if let Ok(meta) = fs::symlink_metadata(file) {
            if meta.is_dir() {
                return Err(Refusal::IsDirectory(file.clone()));
            }
            // What is there is both what the text goes after and what
            // undoing puts back, so it is kept beside the write.
            let existing = fs::read(file).map_err(|_| Refusal::Missing(file.clone()))?;
            plan = plan.push(Operation::Keep {
                path: file.clone(),
                contents: Some(existing.clone()),
            });
            contents = existing;
        }
        contents.extend_from_slice(op.text.as_bytes());
        plan = plan.push(Operation::Write {
            path: file.clone(),
            contents,
        });
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

/// Create `path` empty, refusing to replace what is there. `create_new` is
/// what makes that a check on the filesystem and not on a plan.
fn create_file(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
}

fn write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// Copy a file or a whole tree, following the tree but not the symbolic links
/// inside it.
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

#[cfg(test)]
mod tests;
