//! The desk as this device keeps it: one automerge document, whose own change history is what
//! happened to the desk, who did it and when.
//! The session keeps no second copy of the desk. The tree it draws is a projection of this document
//! ([`desktop_doc`]): a pass that finds the desk changed writes one change of it, a version
//! restored from the history is another change on top, and the activity pane lists the document's
//! own changes. A desk kept as one document means one file, one history and one answer to what the
//! desk is.
//! Every window is kept in the form that opens it again, never as it is running: a live window's
//! surface id names a buffer belonging to the compositor that mapped it, and after a restart the
//! same number is another window or none. So a running app's tab is written as the app
//! ([`tilo_desktop_model::TileContent::LocalApp`]), which the next session launches into the tab it
//! was in; a tab whose content already opens again — a local app, a placeholder, a stub, a window
//! hosted on another device — is written as it is.
//! The document is written away from the frame that changed it ([`tilo_durable_writer`]): a drag
//! writes once, when the drag is over.
//! A desk this build cannot read is never written over: the session starts on an empty one, keeps
//! nothing, and the person is told where their own desk still is ([`StateNotice`]).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc as _, ScalarValue, Value};
use collab_types::presence::UserId;
use tilo_actions::{ActionContext, Did};
use tilo_desktop_model::{DesktopTree, STATE_SCHEMA_VERSION, desktop_doc};
use ui::state::UndoLog;

use crate::desk_migration::Adopted;
use crate::desk_migration::shell;
use crate::i18n::{t, t_args};

/// The document beside the rest of this device's state.
pub(crate) const FILE: &str = "desk.automerge";

/// The key in the document that says which shape of desk it holds: a build that does not know the
/// shape leaves the desk alone rather than guess.
const SCHEMA_KEY: &str = "schema";

/// The key an older build kept the arrangement's view choices and pins under,
/// beside a desk whose windows carried neither. The view each of those places
/// shows is now the window's own and the pin too, so the key is read once,
/// into the windows it names, and deleted.
const MARKS_KEY: &str = "arrangement_marks_v1";

/// The marks [`MARKS_KEY`] held.
#[derive(Default, serde::Deserialize)]
struct Marks {
    /// The view a place shows, by the place's id.
    #[serde(default)]
    views: std::collections::BTreeMap<u64, String>,
    /// The places the person pinned.
    #[serde(default)]
    pins: std::collections::BTreeSet<u64>,
}

/// How long a drag or a burst of switches waits for its last frame before how the desk looks is
/// written.
const DRAG_QUIET: Duration = Duration::from_secs(2);

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

/// The desk's document, and where it is kept.
pub(crate) struct DeskDoc {
    doc: Rc<RefCell<AutoCommit>>,
    /// The desk's history as this session drives it: each change of the desk is one step of the
    /// person's, and the next undo takes back all of the last one.
    undo: RefCell<UndoLog>,
    /// The person here, whose the desk is: their changes are theirs to take back.
    person: String,
    /// Where the document is kept; a session without a state directory keeps it in memory alone,
    /// and one that did not open the desk on disk keeps nothing at all.
    path: Option<PathBuf>,
    /// The desk on disk this build did not open, if any.
    refused: Option<StateNotice>,
    /// The desk an older Tilo kept on this device, which this one did not bring over, if any. The
    /// person is told where it still is; this device starts on an empty desk and keeps it.
    notice: Option<StateNotice>,
    /// When how the desk looks was last written, so a drag or a burst of switches stays one change.
    looks_at: Cell<Option<Instant>>,
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
            return Self::keeping(AutoCommit::new(), None, actor, person, None);
        };
        let at = path(state);
        let mut doc = match read(&at) {
            Read::Document(doc) => *doc,
            Read::Refused(notice) => return Self::refusing(notice, actor, person),
            // No document yet: the desk older sessions kept here moves into one before the files it
            // came from go.
            Read::Nothing => match crate::desk_migration::adopt(state, &at, actor.clone()) {
                Adopted::Document(doc) => *doc,
                Adopted::Nothing => AutoCommit::new(),
                Adopted::Refused(notice) => return Self::refusing(notice, actor, person),
            },
        };
        // A device that ran the shell kept its desk nowhere this session reads: the first start
        // with no desk of its own brings it over.
        let notice = match old_desk {
            Some(old) => {
                match crate::desk_migration::shell::bring_over(old, &at, &mut doc, actor.clone()) {
                    shell::Imported::Nothing | shell::Imported::Desk => None,
                    shell::Imported::Refused(notice) => {
                        refuse(&notice);
                        Some(notice)
                    }
                }
            }
            None => None,
        };
        adopt_marks(&mut doc);
        Self::keeping(doc, Some(at), actor, person, notice)
    }

    /// A document this session writes, kept at `path` when it has one.
    fn keeping(
        mut doc: AutoCommit,
        path: Option<PathBuf>,
        actor: automerge::ActorId,
        person: &str,
        notice: Option<StateNotice>,
    ) -> Self {
        doc.set_actor(actor);
        let undo = UndoLog::new(&mut doc);
        Self {
            doc: Rc::new(RefCell::new(doc)),
            undo: RefCell::new(undo),
            person: person.to_owned(),
            path,
            refused: None,
            notice,
            looks_at: Cell::default(),
        }
    }

    /// An empty desk that keeps nothing: the desk on disk is this build's to leave alone.
    fn refusing(notice: StateNotice, actor: automerge::ActorId, person: &str) -> Self {
        refuse(&notice);
        let mut doc = AutoCommit::new();
        doc.set_actor(actor);
        let undo = UndoLog::new(&mut doc);
        Self {
            doc: Rc::new(RefCell::new(doc)),
            undo: RefCell::new(undo),
            person: person.to_owned(),
            path: None,
            refused: Some(notice),
            notice: None,
            looks_at: Cell::default(),
        }
    }

    /// What became of the desk this device kept, with the refusal it did not open, if any: the desk
    /// on disk, or why the desk the person left is not the one they are looking at.
    pub(crate) fn opened(&self) -> Kept {
        if let Some(notice) = self.refused.as_ref().or(self.notice.as_ref()) {
            return Kept::Refused(notice.clone());
        }
        match self.tree() {
            Some(tree) => Kept::Desk(Box::new(tree)),
            None => Kept::None,
        }
    }

    /// The desk the document holds, as this session draws it, and `None` on a document that holds
    /// no workspace yet. The record each window of the desk shows, by tab, as the document keeps
    /// it: a window placed on a record names it there, so the place shows the record it was placed
    /// on.
    pub(crate) fn records(&self) -> Vec<(tilo_desktop_model::TabId, String)> {
        desktop_doc::records(&self.doc.borrow())
    }

    pub(crate) fn tree(&self) -> Option<DesktopTree> {
        let doc = self.doc.borrow();
        let mut tree = DesktopTree::default();
        desktop_doc::rebuild(&doc, &mut tree);
        desktop_doc::rebuild_view(&doc, &mut tree);
        (!tree.workspaces.is_empty()).then_some(tree)
    }

    /// Record `desktop` as it stands, at `time` (seconds since the epoch): one change of the desk,
    /// kept. Whether the desk changed at all.
    /// Only how the person shows the desk — a drag, a switch — is written once the drag or the
    /// burst of switches is over: a resize writes the view on every frame, and those are not
    /// changes of the desk.
    pub(crate) fn record(
        &self,
        desktop: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.write(desktop, time, true, None, shows)
    }

    /// [`Self::record`], and write even how the desk looks now: what ends the session, which the
    /// next one starts from.
    pub(crate) fn record_now(
        &self,
        desktop: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.write(desktop, time, false, None, shows)
    }

    /// Record `desktop` as it stands as `asked` 's arranging action: one change of the desk whose
    /// own entry says who asked and what they did, so the history names it and the step is the
    /// person's to take back. It is a change of the desk and never a frame of a drag: an action
    /// asked for is written as it is made. Whether the desk changed at all.
    pub(crate) fn record_action(
        &self,
        desktop: &DesktopTree,
        time: i64,
        asked: &ActionContext,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.write(desktop, time, false, Some(asked), shows)
    }

    /// Record `desktop` , and that the windows in `shows` show the records they name: a place the
    /// person put on a record, or the windows an arrangement a build before this one kept.
    fn write(
        &self,
        desktop: &DesktopTree,
        time: i64,
        wait_out_a_drag: bool,
        asked: Option<&ActionContext>,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        let mut doc = self.doc.borrow_mut();
        // Where this change starts: taken before the schema is stamped, so the stamp is part of the
        // change it was needed for and never a change of the desk's history of its own.
        let before = doc.get_heads();
        stamp_schema(&mut doc);
        // A document with no desk holds one after this change: the desk is being opened, here or as
        // it arrived from the workspace, and that is no change of the person's to take back.
        let opened = !desktop_doc::holds(&*doc);
        let looks_only = desktop_doc::seed_of_the_view(&mut doc, &relaunchable(desktop));
        // What each window shows is the desk's, not how this device shows it,
        // and it is written like the view is: what a window opened is no step
        // of the person's on its own, and an action that places a resource is
        // a step because it was asked for.
        for (tab, record) in shows {
            let _ = desktop_doc::note_record(&mut doc, *tab, record);
        }
        if doc.pending_ops() == 0 {
            return false;
        }
        if looks_only && wait_out_a_drag && self.looks_kept_recently() {
            doc.rollback();
            return false;
        }
        let mut options = CommitOptions::default().with_time(time);
        if let Some(message) = asked.and_then(ActionContext::change_message) {
            options = options.with_message(message);
        }
        doc.commit_with(options);
        // One change of the desk is one step of the person's: what they did here is theirs to take
        // back, whichever of their agents asked; so is a change asked for that only moves the view,
        // a width or a maximization, which is a step like any other when an action asks for it. How
        // the desk looks alone, unasked, is no step: those changes ride with the change before or
        // after them. Nor is the desk as it was opened.
        if !opened && (asked.is_some() || !looks_only) {
            self.undo.borrow_mut().record_committed_action_authored(
                &mut doc,
                before,
                None,
                None,
                Some(user(&self.person)),
            );
        }
        if looks_only {
            self.looks_at.set(Some(std::time::Instant::now()));
        }
        self.keep(&mut doc);
        true
    }

    /// Write `doc` where this device keeps it, away from the frame.
    fn keep(&self, doc: &mut AutoCommit) {
        if let Some(at) = &self.path {
            tilo_durable_writer::keep(at.clone(), doc.save());
        }
    }

    /// Take back `person` 's last change of the desk. Whether there was one.
    pub(crate) fn undo(&self, person: &str) -> bool {
        self.rewind(|undo, doc| undo.undo_as(doc, Some(&user(person)), None, None).is_some())
    }

    /// Bring back the last change of the desk `person` took back.
    pub(crate) fn redo(&self, person: &str) -> bool {
        self.rewind(|undo, doc| undo.redo_as(doc, Some(&user(person)), None, None).is_some())
    }

    fn rewind(&self, step: impl FnOnce(&mut UndoLog, &mut AutoCommit) -> bool) -> bool {
        let mut doc = self.doc.borrow_mut();
        let stepped = {
            let mut undo = self.undo.borrow_mut();
            step(&mut undo, &mut doc)
        };
        if stepped {
            self.keep(&mut doc);
        }
        stepped
    }

    /// The last step of the desk's history, as who made it and what it did: what the history names
    /// it by.
    pub(crate) fn last_step(&self, person: &str) -> Option<(String, String)> {
        let doc = self.doc.borrow();
        let entry = self.undo.borrow().next_undo_as(&doc, Some(&user(person)))?;
        let step = entry.step?;
        // Who asked for it: the invoker the step records, else the person whose desk it is.
        let who = step
            .invoker
            .map_or_else(|| person.to_owned(), |invoker| invoker.actor);
        let what = match step.what {
            Some(Did::Verb(call)) => call.action.action,
            _ => String::new(),
        };
        Some((who, what))
    }

    /// Whether how the desk looks was kept so lately that what is written now is still one drag or
    /// one burst of switches.
    fn looks_kept_recently(&self) -> bool {
        self.looks_at
            .get()
            .is_some_and(|at| at.elapsed() < DRAG_QUIET)
    }

    /// The document, for a reader that lists what happened to the desk.
    pub(crate) fn document(&self) -> AutoCommit {
        self.doc.borrow().clone()
    }

    /// Write what `write` decides into the desk's document — a description of
    /// a change, a name over a run of them — as one change by this device,
    /// and keep it. Whether anything was written: a `write` that decides
    /// nothing leaves the document as it was.
    pub(crate) fn write_metadata(&self, write: impl FnOnce(&mut AutoCommit) -> bool) -> bool {
        let mut doc = self.doc.borrow_mut();
        if !write(&mut doc) {
            doc.rollback();
            return false;
        }
        if doc.pending_ops() == 0 {
            return false;
        }
        doc.commit_with(CommitOptions::default().with_time(crate::activity::now_seconds()));
        if let Some(at) = &self.path {
            tilo_durable_writer::keep(at.clone(), doc.save());
        }
        true
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

/// Read the arrangement's marks (`MARKS_KEY`) into the desk's own windows — the view each place
/// shows, whether the person pinned it — and delete them. A document a build before this one wrote
/// keeps them there: the desk itself has no other way to know them, and its windows carry them from
/// now on.
fn adopt_marks(doc: &mut AutoCommit) {
    let Some(marks) = marks_key(doc) else {
        return;
    };
    let mut tree = DesktopTree::default();
    desktop_doc::rebuild(doc, &mut tree);
    for workspace in &mut tree.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    if let Some(view) = marks.views.get(&tab.id) {
                        tab.view.clone_from(view);
                    }
                    tab.pinned = marks.pins.contains(&tab.id);
                }
            }
        }
    }
    desktop_doc::seed(doc, &tree);
    let _ = doc.delete(ROOT, MARKS_KEY);
    doc.commit();
}

/// The marks the document holds, if it holds any this build reads.
fn marks_key(doc: &AutoCommit) -> Option<Marks> {
    let (Value::Scalar(scalar), _) = doc.get(ROOT, MARKS_KEY).ok().flatten()? else {
        return None;
    };
    let ScalarValue::Str(text) = scalar.as_ref() else {
        return None;
    };
    // ignored-failure: marks that do not read are a desk another build wrote, and its windows are what this one goes by
    serde_json::from_str(text).ok()
}

/// The actor `person` writes the desk's changes as: their handle itself, so a change of the desk
/// reads as theirs here without anything else naming it ([`ui::history_source::actor_handle`]).
pub(crate) fn actor_of_person(person: &str) -> automerge::ActorId {
    automerge::ActorId::from(person.as_bytes())
}

/// The person `person` , as the history names them.
fn user(person: &str) -> UserId {
    UserId(person.to_owned())
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

/// Say which shape of desk `doc` holds, in the change about to be made: a newer build that finds a
/// newer one leaves the desk alone.
pub(crate) fn stamp_schema(doc: &mut AutoCommit) {
    if schema_of(doc) == Some(STATE_SCHEMA_VERSION) {
        return;
    }
    if let Err(error) = doc.put(ROOT, SCHEMA_KEY, u64::from(STATE_SCHEMA_VERSION)) {
        tracing::warn!(%error, "the desk does not say which shape it is");
    }
}

/// `desktop` as the document keeps it: every window in the form that opens it again, and no window
/// that opens again in no way.
pub(crate) fn relaunchable(desktop: &DesktopTree) -> DesktopTree {
    let mut desktop = desktop.clone();
    desktop.schema_version = STATE_SCHEMA_VERSION;
    for workspace in &mut desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    *tab = tab.relaunchable();
                }
                // A window whose app could not be told apart from any other — the compositor named
                // none — is kept as a tab with no app to open it: nothing would ever come back into
                // it, and the tab would wait on the desk for good. What cannot open again is not
                // kept.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
    use tilo_desktop_model::{StripId, WindowWidth, Workspace, WorkspaceId, WorkspaceSlot};

    /// A desk of one workspace, one strip and one window.
    fn desk() -> (DesktopTree, u64) {
        let mut workspace = Workspace::in_workspace(
            WorkspaceSlot(1),
            WorkspaceId::parse("00000000-0000-4000-8000-000000000042").expect("an id"),
            "Desk",
        );
        let mut strip = Strip::new(StripId(1), "A");
        let tab = Tab {
            id: 9,
            title: "Notes".into(),
            tile: TileContent::LocalApp {
                app_id: "org.example.Notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        strip.window_groups = vec![WindowGroup {
            id: 1,
            tabs: vec![tab],
            active_tab: 9,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        workspace.strips = vec![strip];
        (
            DesktopTree {
                workspaces: vec![workspace],
                active: WorkspaceSlot(1),
                ..DesktopTree::default()
            },
            9,
        )
    }

    /// The view and the pins a build before this one kept beside the desk are read into the desk's
    /// own windows once, and then gone.
    #[test]
    fn the_marks_a_build_before_this_one_kept_move_into_the_desks_windows() {
        let (tree, tab) = desk();
        let mut doc = AutoCommit::new();
        desktop_doc::seed(&mut doc, &tree);
        doc.put(
            ROOT,
            MARKS_KEY,
            format!(r#"{{"views":{{"{tab}":"editor"}},"pins":[{tab}]}}"#).as_str(),
        )
        .expect("a document that takes the marks");
        doc.commit();

        adopt_marks(&mut doc);
        let mut back = DesktopTree::default();
        desktop_doc::rebuild(&doc, &mut back);
        let shown = &back.workspaces[0].strips[0].window_groups[0].tabs[0];
        assert_eq!(shown.view, "editor", "the window shows the view it did");
        assert!(shown.pinned, "and stays where the person pinned it");
        assert!(
            doc.get(ROOT, MARKS_KEY).ok().flatten().is_none(),
            "the marks they were read from are gone"
        );

        // What is kept now is the desk's own: read again, it says the same.
        let saved = AutoCommit::load(&doc.save()).expect("the desk's document");
        let mut again = DesktopTree::default();
        desktop_doc::rebuild(&saved, &mut again);
        let shown = &again.workspaces[0].strips[0].window_groups[0].tabs[0];
        assert_eq!(shown.view, "editor", "the view survives the document");
        assert!(shown.pinned, "and the pin");
    }
}
