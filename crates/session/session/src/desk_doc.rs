//! The desk as this device keeps it, where the session keeps it: the file under the state
//! directory, the older desks a device may have here and the notices a person is owed when this
//! build did not open theirs.
//! What the desk is — its document, the rule that one change of it is one step of a person's, the
//! undo that takes the step back — is [`tilo_session_core::desk`]'s, written once for every host.
//! This module is the native host's side of it: [`load`](DeskDoc::load) reads the file (and brings
//! over what older Tilos kept, [`crate::desk_migration`]), and what is kept is written away from
//! the frame ([`tilo_durable_writer`]).
//! A desk this build cannot read is never written over: the session starts on an empty one, keeps
//! nothing, and the person is told where their own desk still is ([`StateNotice`]).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use automerge::AutoCommit;
use tilo_desktop_model::{DesktopTree, STATE_SCHEMA_VERSION};
use tilo_session_core::desk::{Desk, Loaded, Store};

use crate::desk_migration::Adopted;
use crate::desk_migration::shell;
use crate::i18n::{t, t_args};

pub(crate) use tilo_session_core::desk::{actor_of_person, relaunchable, stamp_schema};

/// The document beside the rest of this device's state.
pub(crate) const FILE: &str = "desk.automerge";

/// Where the document lives under `state` , this device's state directory.
pub(crate) fn path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// What became of the desk this device kept.
pub(crate) enum Kept {
    /// The desk as it was left, ready to be put back. Boxed: the whole desk is many times the size
    /// of the other two answers, and every call that returns one pays for the largest.
    Desk(Box<DesktopTree>),
    /// This device kept no desk, or one with nothing on it: a first start.
    None,
    /// The desk is on disk and this build will not open it, with what the person is told about it.
    Refused(StateNotice),
}

/// Why the desk on disk was left closed. It is still there and untouched: this session starts on an
/// empty desk instead, and the person has to be told, because a desk that is not theirs looks
/// exactly like theirs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StateNotice {
    /// The file was written by a newer Tilo than this build understands.
    NewerSchema {
        path: PathBuf,
        found: u32,
        understood: u32,
    },
    /// The file exists and could not be read at all.
    Unreadable { path: PathBuf, error: String },
    /// The desk the shell this session replaced kept was written by a newer Tilo than this build
    /// understands.
    OldDeskNewer {
        path: PathBuf,
        found: u32,
        understood: u32,
    },
    /// The desk the shell this session replaced kept is there and could not be read.
    OldDeskUnreadable { path: PathBuf, error: String },
}

impl StateNotice {
    /// What happened, in one line.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::NewerSchema { .. } => t("desk-not-opened-newer-title"),
            Self::Unreadable { .. } => t("desk-not-opened-unreadable-title"),
            Self::OldDeskNewer { .. } => t("desk-not-opened-old-newer-title"),
            Self::OldDeskUnreadable { .. } => t("desk-not-opened-old-unreadable-title"),
        }
    }

    /// Where the person's desk is and that nothing was written over it.
    ///
    /// The layout versions go in as text: they are names of on-disk shapes,
    /// not quantities, and a group separator in the middle of one reads as a
    /// different number.
    pub(crate) fn explains(&self) -> String {
        match self {
            Self::NewerSchema {
                path,
                found,
                understood,
            } => t_args(
                "desk-not-opened-newer-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("found", found.to_string().into()),
                    ("understood", understood.to_string().into()),
                ],
            ),
            Self::Unreadable { path, error } => t_args(
                "desk-not-opened-unreadable-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("error", error.clone().into()),
                ],
            ),
            Self::OldDeskNewer {
                path,
                found,
                understood,
            } => t_args(
                "desk-not-opened-old-newer-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("found", found.to_string().into()),
                    ("understood", understood.to_string().into()),
                ],
            ),
            Self::OldDeskUnreadable { path, error } => t_args(
                "desk-not-opened-old-unreadable-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("error", error.clone().into()),
                ],
            ),
        }
    }

    /// Both lines of it, as the person reads them.
    fn whole(&self) -> String {
        format!("{}\n\n{}", self.title(), self.explains())
    }
}

/// Leave `notice` with the desk file it is about, for anyone not looking at the screen when it
/// happened: repair mode, support, the next login.
pub(crate) fn refuse(notice: &StateNotice) {
    let path = match notice {
        StateNotice::NewerSchema { path, .. }
        | StateNotice::Unreadable { path, .. }
        | StateNotice::OldDeskNewer { path, .. }
        | StateNotice::OldDeskUnreadable { path, .. } => path,
    };
    tracing::error!(path = %path.display(), "{}", notice.explains());
    let beside = PathBuf::from(format!("{}.not-opened", path.display()));
    let body = format!(
        "{}\n\nWritten by Tilo when it declined to open the file next to this \
         one. Delete this file once the situation is resolved; nothing reads \
         it.\n",
        notice.whole()
    );
    if let Err(error) = std::fs::write(&beside, body) {
        tracing::warn!(
            path = %beside.display(),
            %error,
            "could not leave the explanation beside the desk file"
        );
    }
}

/// The desk's document, and what this device made of the one it kept.
pub(crate) struct DeskDoc {
    /// Shared with the arrangement, which reads and writes the same desk.
    desk: Rc<Desk>,
    /// The desk on disk this build did not open, if any.
    refused: Option<StateNotice>,
    /// The desk an older Tilo kept on this device, which this one did not bring over, if any. The
    /// person is told where it still is; this device starts on an empty desk and keeps it.
    notice: Option<StateNotice>,
}

impl DeskDoc {
    /// The document kept under `state` , read into from what older Tilos kept there
    /// ([`crate::desk_migration`]) when it holds no desk yet, and empty on a device that has no
    /// desk yet. `old_desk` is the desk file the shell this session replaced kept, for a device
    /// that upgraded and has no desk of its own here. Its changes are written by `person` , so the
    /// activity names them as this person's own.
    pub(crate) fn load(state: Option<&Path>, old_desk: Option<&Path>, person: &str) -> Self {
        let actor = actor_of_person(person);
        let Some(state) = state else {
            return Self::keeping(AutoCommit::new(), None, person, None);
        };
        let at = path(state);
        let mut doc = match read(&at) {
            Read::Document(doc) => *doc,
            Read::Refused(notice) => return Self::refusing(notice, person),
            // No document yet: the desk older sessions kept here moves into one before the files it
            // came from go.
            Read::Nothing => match crate::desk_migration::adopt(state, &at, actor) {
                Adopted::Document(doc) => *doc,
                Adopted::Nothing => AutoCommit::new(),
                Adopted::Refused(notice) => return Self::refusing(notice, person),
            },
        };
        // A device that ran the shell kept its desk nowhere this session reads: the first start
        // with no desk of its own brings it over.
        let notice = match old_desk {
            Some(old) => {
                match crate::desk_migration::shell::bring_over(
                    old,
                    &at,
                    &mut doc,
                    actor_of_person(person),
                ) {
                    shell::Imported::Nothing | shell::Imported::Desk => None,
                    shell::Imported::Refused(notice) => {
                        refuse(&notice);
                        Some(notice)
                    }
                }
            }
            None => None,
        };
        Self::keeping(doc, Some(at), person, notice)
    }

    /// A document this session writes, kept at `path` when it has one.
    fn keeping(
        doc: AutoCommit,
        path: Option<PathBuf>,
        person: &str,
        notice: Option<StateNotice>,
    ) -> Self {
        let store = path.map(|path| Rc::new(FileAt(path)) as Rc<dyn Store>);
        Self {
            desk: Rc::new(Desk::new(doc, person, store)),
            refused: None,
            notice,
        }
    }

    /// An empty desk that keeps nothing: the desk on disk is this build's to leave alone.
    fn refusing(notice: StateNotice, person: &str) -> Self {
        refuse(&notice);
        Self {
            desk: Rc::new(Desk::new(AutoCommit::new(), person, None)),
            refused: Some(notice),
            notice: None,
        }
    }

    /// What became of the desk this device kept, with the refusal it did not open, if any: the desk
    /// on disk, or why the desk the person left is not the one they are looking at.
    pub(crate) fn opened(&self) -> Kept {
        if let Some(notice) = self.refused.as_ref().or(self.notice.as_ref()) {
            return Kept::Refused(notice.clone());
        }
        match self.desk.tree() {
            Some(tree) => Kept::Desk(Box::new(tree)),
            None => Kept::None,
        }
    }

    /// The desk the document holds, for a test that reads what a session left.
    #[cfg(test)]
    pub(crate) fn tree(&self) -> Option<DesktopTree> {
        self.desk.tree()
    }

    /// The desk document itself, as the arrangement keeps it too.
    pub(crate) fn desk(&self) -> Rc<Desk> {
        Rc::clone(&self.desk)
    }

    /// Record `desktop` as it stands, at `time` (seconds since the epoch): one change of the desk,
    /// kept. Whether the desk changed at all.
    pub(crate) fn record(
        &self,
        desktop: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.desk.record(desktop, time, shows, None)
    }

    /// [`Self::record`], and write even how the desk looks now: what ends the session, which the
    /// next one starts from.
    pub(crate) fn record_now(
        &self,
        desktop: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.desk.record_now(desktop, time, shows, None)
    }

    /// The document, for a reader that lists what happened to the desk.
    pub(crate) fn document(&self) -> AutoCommit {
        self.desk.document()
    }

    /// Write what `write` decides into the desk's document — a description of a change, a name over
    /// a run of them — as one change by this device, and keep it. Whether anything was written.
    pub(crate) fn write_metadata(&self, write: impl FnOnce(&mut AutoCommit) -> bool) -> bool {
        self.desk.write_metadata(write)
    }

    /// The changes the desk is at, so a reader can tell it moved.
    pub(crate) fn heads(&self) -> Vec<automerge::ChangeHash> {
        self.desk.heads()
    }

    /// The names this device's changes are known by, by actor (hex).
    pub(crate) fn authors(&self) -> std::collections::BTreeMap<String, String> {
        self.desk.authors()
    }
}

/// Keep the desk where this device keeps it: the file, written by the writer that never waits on a
/// frame.
struct FileAt(PathBuf);

impl Store for FileAt {
    fn keep(&self, bytes: Vec<u8>) {
        tilo_durable_writer::keep(self.0.clone(), bytes);
    }
}

impl crate::Shared {
    /// Record this device's desk as it stands: its workspaces, the windows in them and the strips
    /// and displays they are laid out on, so the next session starts here. The change of the desk's
    /// document is written away from the frame, and a drag or a rename is one change however many
    /// frames it takes.
    pub(crate) fn keep_desk(&self) {
        let tree = self.handle.read(Clone::clone);
        let shows = self.notes.records().into_iter().collect::<Vec<_>>();
        self.activity
            .record_desk(&tree, crate::activity::now_seconds(), &shows);
    }

    /// [`Self::keep_desk`], and write the desk as it looks now rather than
    /// once a drag is over, then wait for the write: what ends the session,
    /// and anything that reads the desk's document after it.
    pub(crate) fn keep_desk_now(&self) {
        let tree = self.handle.read(Clone::clone);
        let shows = self.notes.records().into_iter().collect::<Vec<_>>();
        self.activity
            .record_desk_now(&tree, crate::activity::now_seconds(), &shows);
        tilo_durable_writer::global().flush();
    }
}

/// What the file at `path` holds.
enum Read {
    /// The document the desk is kept as.
    Document(Box<AutoCommit>),
    /// This device kept no desk there yet.
    Nothing,
    /// The file is there and this build does not open it.
    Refused(StateNotice),
}

/// The document kept at `path` , if it is one this build opens.
fn read(path: &Path) -> Read {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Read::Nothing,
        Err(error) => {
            return Read::Refused(StateNotice::Unreadable {
                path: path.to_path_buf(),
                error: error.to_string(),
            });
        }
    };
    match tilo_session_core::desk::open(&bytes) {
        Loaded::Document(doc) => Read::Document(doc),
        Loaded::Unreadable(error) => Read::Refused(StateNotice::Unreadable {
            path: path.to_path_buf(),
            error,
        }),
        Loaded::Newer { found } => Read::Refused(StateNotice::NewerSchema {
            path: path.to_path_buf(),
            found,
            understood: STATE_SCHEMA_VERSION,
        }),
    }
}
