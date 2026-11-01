//! The desk as this device keeps it: one automerge document, whose own change
//! history is what happened to the desk, who did it and when.
//!
//! The session keeps no second copy of the desk. The tree it draws is a
//! projection of this document ([`desktop_doc`]): a pass that finds the desk
//! changed writes one change of it, a version restored from the history is
//! another change on top, and the activity pane lists the document's own
//! changes. A desk kept as one document means one file, one history and one
//! answer to what the desk is.
//!
//! Every window is kept in the form that opens it again, never as it is
//! running: a live window's surface id names a buffer belonging to the
//! compositor that mapped it, and after a restart the same number is another
//! window or none. So a running app's tab is written as the app
//! ([`tilo_desktop_model::TileContent::LocalApp`]), which the next session
//! launches into the tab it was in; a tab whose content already opens again —
//! a local app, a placeholder, a stub, a window hosted on another device — is
//! written as it is.
//!
//! The document is written away from the frame that changed it
//! ([`tilo_durable_writer`]): a drag writes once, when the drag is over.
//!
//! A desk this build cannot read is never written over: the session starts on
//! an empty one, keeps nothing, and the person is told where their own desk
//! still is ([`StateNotice`]).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc as _, ScalarValue, Value};
use tilo_desktop_model::{DesktopTree, STATE_SCHEMA_VERSION, desktop_doc};

use crate::desk_migration::Adopted;
use crate::i18n::{t, t_args};

/// The document beside the rest of this device's state.
pub(crate) const FILE: &str = "desk.automerge";

/// The key in the document that says which shape of desk it holds: a build
/// that does not know the shape leaves the desk alone rather than guess.
const SCHEMA_KEY: &str = "schema";

/// How long a drag or a burst of switches waits for its last frame before how
/// the desk looks is written.
const DRAG_QUIET: Duration = Duration::from_secs(2);

/// Where the document lives under `state`, this device's state directory.
pub(crate) fn path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// What became of the desk this device kept.
pub(crate) enum Kept {
    /// The desk as it was left, ready to be put back. Boxed: the whole desk
    /// is many times the size of the other two answers, and every call that
    /// returns one pays for the largest.
    Desk(Box<DesktopTree>),
    /// This device kept no desk, or one with nothing on it: a first start.
    None,
    /// The desk is on disk and this build will not open it, with what the
    /// person is told about it.
    Refused(StateNotice),
}

/// Why the desk on disk was left closed. It is still there and untouched:
/// this session starts on an empty desk instead, and the person has to be
/// told, because a desk that is not theirs looks exactly like theirs.
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
}

impl StateNotice {
    /// What happened, in one line.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::NewerSchema { .. } => t("desk-not-opened-newer-title"),
            Self::Unreadable { .. } => t("desk-not-opened-unreadable-title"),
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
        }
    }

    /// Both lines of it, as the person reads them.
    fn whole(&self) -> String {
        format!("{}\n\n{}", self.title(), self.explains())
    }
}

/// Leave `notice` with the desk file it is about, for anyone not looking at
/// the screen when it happened: repair mode, support, the next login.
pub(crate) fn refuse(notice: &StateNotice) {
    let path = match notice {
        StateNotice::NewerSchema { path, .. } | StateNotice::Unreadable { path, .. } => path,
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

/// The desk's document, and where it is kept.
pub(crate) struct DeskDoc {
    doc: Rc<RefCell<AutoCommit>>,
    /// Where the document is kept; a session without a state directory keeps
    /// it in memory alone, and one that did not open the desk on disk keeps
    /// nothing at all.
    path: Option<PathBuf>,
    /// The desk on disk this build did not open, if any.
    refused: Option<StateNotice>,
    /// When how the desk looks was last written, so a drag or a burst of
    /// switches stays one change.
    looks_at: Cell<Option<Instant>>,
}

impl DeskDoc {
    /// The document kept under `state`, read into from what older sessions
    /// kept there ([`crate::desk_migration`]) when there is none yet, and
    /// empty on a device that has no desk yet. Its changes are written by
    /// `person`, so the activity names them as this person's own.
    pub(crate) fn load(state: Option<&Path>, person: &str) -> Self {
        let actor = actor_of_person(person);
        let Some(state) = state else {
            return Self::keeping(AutoCommit::new(), None, actor);
        };
        let at = path(state);
        match read(&at) {
            Read::Document(doc) => Self::keeping(*doc, Some(at), actor),
            Read::Refused(notice) => Self::refusing(notice, actor),
            // No document yet: the desk older sessions kept here moves into
            // one before the files it came from go.
            Read::Nothing => match crate::desk_migration::adopt(state, &at, actor.clone()) {
                Adopted::Document(doc) => Self::keeping(*doc, Some(at), actor),
                Adopted::Nothing => Self::keeping(AutoCommit::new(), Some(at), actor),
                Adopted::Refused(notice) => Self::refusing(notice, actor),
            },
        }
    }

    /// A document this session writes, kept at `path` when it has one.
    fn keeping(mut doc: AutoCommit, path: Option<PathBuf>, actor: automerge::ActorId) -> Self {
        doc.set_actor(actor);
        Self {
            doc: Rc::new(RefCell::new(doc)),
            path,
            refused: None,
            looks_at: Cell::default(),
        }
    }

    /// An empty desk that keeps nothing: the desk on disk is this build's to
    /// leave alone.
    fn refusing(notice: StateNotice, actor: automerge::ActorId) -> Self {
        refuse(&notice);
        let mut doc = AutoCommit::new();
        doc.set_actor(actor);
        Self {
            doc: Rc::new(RefCell::new(doc)),
            path: None,
            refused: Some(notice),
            looks_at: Cell::default(),
        }
    }

    /// What became of the desk this device kept, with the refusal it did not
    /// open, if any.
    pub(crate) fn opened(&self) -> Kept {
        if let Some(notice) = &self.refused {
            return Kept::Refused(notice.clone());
        }
        match self.tree() {
            Some(tree) => Kept::Desk(Box::new(tree)),
            None => Kept::None,
        }
    }

    /// The desk the document holds, as this session draws it, and `None` on a
    /// document that holds no workspace yet.
    pub(crate) fn tree(&self) -> Option<DesktopTree> {
        let doc = self.doc.borrow();
        let mut tree = DesktopTree::default();
        desktop_doc::rebuild(&doc, &mut tree);
        desktop_doc::rebuild_view(&doc, &mut tree);
        (!tree.workspaces.is_empty()).then_some(tree)
    }

    /// Record `desktop` as it stands, at `time` (seconds since the epoch): one
    /// change of the desk, kept. Whether the desk changed at all.
    ///
    /// Only how the person shows the desk — a drag, a switch — is written once
    /// the drag or the burst of switches is over: a resize writes the view on
    /// every frame, and those are not changes of the desk.
    pub(crate) fn record(&self, desktop: &DesktopTree, time: i64) -> bool {
        self.write(desktop, time, true)
    }

    /// [`Self::record`], and write even how the desk looks now: what ends the
    /// session, which the next one starts from.
    pub(crate) fn record_now(&self, desktop: &DesktopTree, time: i64) -> bool {
        self.write(desktop, time, false)
    }

    fn write(&self, desktop: &DesktopTree, time: i64, wait_out_a_drag: bool) -> bool {
        let mut doc = self.doc.borrow_mut();
        stamp_schema(&mut doc);
        let looks_only = desktop_doc::seed_of_the_view(&mut doc, &relaunchable(desktop));
        if doc.pending_ops() == 0 {
            return false;
        }
        if looks_only && wait_out_a_drag && self.looks_kept_recently() {
            doc.rollback();
            return false;
        }
        doc.commit_with(CommitOptions::default().with_time(time));
        if looks_only {
            self.looks_at.set(Some(std::time::Instant::now()));
        }
        if let Some(at) = &self.path {
            tilo_durable_writer::keep(at.clone(), doc.save());
        }
        true
    }

    /// Whether how the desk looks was kept so lately that what is written now
    /// is still one drag or one burst of switches.
    fn looks_kept_recently(&self) -> bool {
        self.looks_at
            .get()
            .is_some_and(|at| at.elapsed() < DRAG_QUIET)
    }

    /// The document, for a reader that lists what happened to the desk.
    pub(crate) fn document(&self) -> AutoCommit {
        self.doc.borrow().clone()
    }

    /// The changes the desk is at, so a reader can tell it moved.
    pub(crate) fn heads(&self) -> Vec<ChangeHash> {
        self.doc.borrow().base_heads()
    }

    /// The names this device's changes are known by, by actor (hex).
    pub(crate) fn authors(&self) -> std::collections::BTreeMap<String, String> {
        desktop_doc::author_names(&self.doc.borrow())
    }
}

impl crate::Shared {
    /// Record this device's desk as it stands: its workspaces, the windows in
    /// them and the strips and displays they are laid out on, so the next
    /// session starts here. The change of the desk's document is written away
    /// from the frame, and a drag or a rename is one change however many
    /// frames it takes.
    pub(crate) fn keep_desk(&self) {
        let tree = self.handle.read(Clone::clone);
        self.activity
            .record_desk(&tree, crate::activity::now_seconds());
    }

    /// [`Self::keep_desk`], and write the desk as it looks now rather than
    /// once a drag is over, then wait for the write: what ends the session,
    /// and anything that reads the desk's document after it.
    pub(crate) fn keep_desk_now(&self) {
        let tree = self.handle.read(Clone::clone);
        self.activity
            .record_desk_now(&tree, crate::activity::now_seconds());
        tilo_durable_writer::global().flush();
    }
}

/// The actor `person` writes the desk's changes as: their handle itself, so a
/// change of the desk reads as theirs here without anything else naming it
/// ([`ui::history_source::actor_handle`]).
pub(crate) fn actor_of_person(person: &str) -> automerge::ActorId {
    automerge::ActorId::from(person.as_bytes())
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

/// The document kept at `path`, if it is one this build opens.
fn read(path: &Path) -> Read {
    let unreadable = |error: String| {
        Read::Refused(StateNotice::Unreadable {
            path: path.to_path_buf(),
            error,
        })
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Read::Nothing,
        Err(error) => return unreadable(error.to_string()),
    };
    let options = automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore);
    let doc = match AutoCommit::load_with_options(&bytes, options) {
        Ok(doc) => doc,
        Err(error) => return unreadable(error.to_string()),
    };
    match schema_of(&doc) {
        Some(found) if found > STATE_SCHEMA_VERSION => Read::Refused(StateNotice::NewerSchema {
            path: path.to_path_buf(),
            found,
            understood: STATE_SCHEMA_VERSION,
        }),
        _ => Read::Document(Box::new(doc)),
    }
}

/// The shape of desk `doc` says it holds, `None` on one that says nothing.
pub(crate) fn schema_of(doc: &AutoCommit) -> Option<u32> {
    let Ok(Some((Value::Scalar(found), _))) = doc.get(ROOT, SCHEMA_KEY) else {
        return None;
    };
    match found.as_ref() {
        ScalarValue::Uint(found) => u32::try_from(*found).ok(),
        _ => None,
    }
}

/// Say which shape of desk `doc` holds, in the change about to be made: a
/// newer build that finds a newer one leaves the desk alone.
pub(crate) fn stamp_schema(doc: &mut AutoCommit) {
    if schema_of(doc) == Some(STATE_SCHEMA_VERSION) {
        return;
    }
    if let Err(error) = doc.put(ROOT, SCHEMA_KEY, u64::from(STATE_SCHEMA_VERSION)) {
        tracing::debug!(%error, "the desk does not say which shape it is");
    }
}

/// `desktop` as the document keeps it: every window in the form that opens it
/// again, and no window that opens again in no way.
pub(crate) fn relaunchable(desktop: &DesktopTree) -> DesktopTree {
    let mut desktop = desktop.clone();
    desktop.schema_version = STATE_SCHEMA_VERSION;
    for workspace in &mut desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    *tab = tab.relaunchable();
                }
                // A window whose app could not be told apart from any other —
                // the compositor named none — is kept as a tab with no app to
                // open it: nothing would ever come back into it, and the tab
                // would wait on the desk for good. What cannot open again is
                // not kept.
                group.tabs.retain(|tab| {
                    !matches!(&tab.tile, tilo_desktop_model::TileContent::LocalApp { app_id, .. }
                        if app_id.trim().is_empty())
                });
                if !group.tabs.iter().any(|tab| tab.id == group.active_tab)
                    && let Some(first) = group.tabs.first()
                {
                    group.active_tab = first.id;
                }
                let open = &group.tabs;
                group
                    .focus_history
                    .retain(|tab| open.iter().any(|candidate| candidate.id == *tab));
            }
        }
    }
    desktop
}
