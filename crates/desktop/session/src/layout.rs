//! A workspace's layout: its strips, groups and windows, which every member shows and changes
//! ([`tilo_desktop_model::shared_resource`]), and each person's view of it, where they are looking
//! and how wide their windows are ([`tilo_desktop_model::person_view`]).
//! The layout is the workspace's, so it is where the live layout is kept: the tree this session draws
//! is what the layout and the person's view say, and every change a window makes is written there
//! ([`Self::capture`]) and read back from it ([`Self::follow`]). Undo and redo of a layout change
//! are taken from its own history ([`Self::step`]).
//! Both documents are read from and written to the workspace's own copy in this device's workspace
//! service: what the workspace admitted, what it held before this device joined included, and what
//! this device writes there as its signed, sealed writes. Nothing here decides whether a write may
//! land: that is the workspace's, decided where the service takes it in. A document that did not
//! move since the last look is neither read nor written again, and what is written is published off
//! the loop ([`crate::engine_write`]).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use automerge::ChangeHash;
use automerge::transaction::Transactable as _;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, OpenedBy};
use tilo_desktop_model::person_view::{self, PersonView};
use tilo_desktop_model::shared_resource::{self, device};
use tilo_desktop_model::{
    Layout, Strip, TileContent, Workspace, WorkspaceId, WorkspaceLayout, peer_salt_of,
};
use ui::permissions::MemberId;
use ui::state::UndoLog;

use tilo_user_identity::DeviceCert;

use crate::workspace_view::WorkspaceView;
use neighbour_wire::RefusalReason;
use tilo_desktop_surface::DesktopHandle;
use tilo_peer_state::owner::WriteRefusal;

/// The person `person`, as a history names them.
fn user(person: &str) -> MemberId {
    MemberId::person(person.to_owned())
}

/// A layout's history as this session drives it: every change of it a person made, on any of
/// their devices, is one step of theirs.
fn history(doc: &mut automerge::AutoCommit) -> UndoLog {
    UndoLog::new(doc).on_any_device()
}

/// The actor `person`'s changes of a layout are written as on this device: their handle and this
/// device's key, so two of their devices writing at once never number a change the same
/// (`ui::history_source::authors::device_actor`).
fn actor(certificate: &DeviceCert, person: &str) -> automerge::ActorId {
    automerge::ActorId::from(ui::history_source::authors::device_actor(
        person.as_bytes(),
        &automerge::ActorId::from(certificate.device_public_key.to_vec()),
    ))
}

/// A record of a workspace: the app it belongs to and its name there.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Route {
    app: &'static str,
    document: String,
}

impl Route {
    /// The workspace's layout.
    fn layout() -> Self {
        Self {
            app: DESKTOP_APP,
            document: DESKTOP_DOCUMENT.to_owned(),
        }
    }

    /// `person`'s view of the workspace's layout.
    fn view(person: &str) -> Self {
        Self {
            app: DESKTOP_APP,
            document: person_view::resource_of(person),
        }
    }
}

/// A record of a workspace this device reads and writes.
type Key = (WorkspaceId, Route);

/// What a record held when this device last read or wrote it, as its route compares it: a look
/// that finds it again writes nothing.
#[derive(Clone, PartialEq, Eq)]
enum Written {
    /// The layout revision it was written from, and the record each window showed.
    Layout(u64, Vec<(u64, String)>),
    /// The view written.
    View(PersonView),
}

/// This device's part in one record of one workspace.
#[derive(Default)]
struct Held {
    /// The heads the layout was last folded into the tree at.
    folded: Option<Vec<ChangeHash>>,
    /// The revision the record was last read at: the next read asks for it against this, so a
    /// record that did not move is answered from the admitted heads rather than captured again.
    revision: Option<[u8; 32]>,
    /// The record as this device last wrote it and the workspace took it: what the next write is
    /// made on. The daemon's copy reaches a member after the workspace admits the write, so it can
    /// lag the write this device already made; writing on that lagging copy would number a second
    /// change the same as the first, and the workspace would refuse it.
    kept: Option<automerge::AutoCommit>,
    /// The revision `kept` holds.
    based: Option<[u8; 32]>,
    /// What the record held when this device last read or wrote it.
    written: Option<Written>,
    /// The person's view as this device shows it from the record alone, with nothing changed
    /// here: a front tab that differs from it is the person's change here; one that differs from
    /// the record only because this device does not hold the record's tab is not.
    seen: Option<PersonView>,
    /// The view that changed since the last write, and did not move on: written on the look after
    /// the one that found it, so a drag or a burst of switches is one write when it is over.
    pending: Option<PersonView>,
    /// The layout's history, as this session drives it: each change this device made of it is one
    /// step of the person's, and the next undo takes back all of the last one.
    undo: Option<UndoLog>,
    /// The write of the record under way.
    writing: Option<Going>,
    /// A step of the person's the layout asked for while its write was under way: it
    /// is taken on the look that finds that write done.
    pending_step: Option<bool>,
    /// What this device changed of the layout while it could not be written, in the order it
    /// changed: each is written as its own change, so a step taken then is taken back on its own.
    pending_steps: PendingSteps<LayoutStep>,
    /// The changes this device made of the person's view while it could not be written, in the
    /// order it made them: each is written as its own change, so a step taken then is taken back
    /// on its own.
    pending_view: PendingSteps<PersonView>,
    /// Whether the record was read here.
    read: bool,
    /// Whether this device's own store was asked for the layout at least once, whether it held
    /// one or not: a write under way or one that moved leaves [`Self::read`] unset again, but
    /// what the local store held was read, and kept windows wait for nothing more.
    read_once: bool,
    /// Whether a look found the view unreadable: what it holds is not known here yet, so no view
    /// is written of it until a look reads it.
    unreadable: bool,
    /// Why the layout could not be read at [`Self::read_at`]: it fails again until the workspace
    /// moves, so the looks in between have nothing to read and nothing to say about it again.
    failed: Option<String>,
    /// How the person's workspaces stood ([`Layouts::standing`]) when this record was last read,
    /// or found not to have moved: a look that skips the record (its write under way) or a write
    /// of it that finished leaves it behind, so the record is read again however the rest of the
    /// workspace moved meanwhile.
    read_at: Option<u64>,
    /// The revision the record was read at when the workspace answered that a write of it waits
    /// for changes it does not hold yet: the write is made again once the record moves past it,
    /// not on every look.
    waits: Option<Waits>,
}

/// Where a record stood when the workspace answered that this device's write of it waits.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Waits {
    /// The revision the record was read at then; `None` when nothing of it was read yet.
    at: Option<[u8; 32]>,
}

/// A record a step may be taken in: the record as this device writes it, its history and the
/// revision it was read at.
struct Candidate {
    key: Key,
    doc: automerge::AutoCommit,
    log: UndoLog,
    revision: Option<[u8; 32]>,
}

/// What an arranging action did to a layout: the change it makes names who asked and what they did,
/// and the windows it placed show the records they name.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Acted {
    message: String,
    shows: Vec<(tilo_desktop_model::TabId, String)>,
    /// The person's view of the workspace before the action, where it changed their view: the
    /// step takes the view back to it.
    was: Option<PersonView>,
}

/// A workspace's layout as this device arranged it while the layout could not be written, and the
/// step it is of the person's, named where they made it so it orders among their steps elsewhere.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct LayoutStep {
    workspace: Workspace,
    step: String,
}

/// A change this device made while what it changes could not be written, and the arranging action
/// that named it where one made it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PendingStep<T> {
    changed: T,
    acted: Option<Acted>,
}

/// What this device changed of a workspace while it could not write it: the layout, and the
/// person's view of it, each change as its own step.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Pending {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    layout: Vec<PendingStep<LayoutStep>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    view: Vec<PendingStep<PersonView>>,
}

/// The changes this device made while that could not be written, in the order it made them.
struct PendingSteps<T>(Vec<PendingStep<T>>);

impl<T> Default for PendingSteps<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> PendingSteps<T> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn last(&self) -> Option<&PendingStep<T>> {
        self.0.last()
    }

    fn push(&mut self, changed: T, acted: Option<Acted>) {
        self.0.push(PendingStep { changed, acted });
    }

    fn take(&mut self) -> Vec<PendingStep<T>> {
        std::mem::take(&mut self.0)
    }

    /// Keep the changes a write that was not made held, before what changed since.
    fn restore(&mut self, earlier: Vec<PendingStep<T>>) {
        self.0.splice(..0, earlier);
    }
}

/// A write of a record under way. Boxed: each holds a document, and the state beside it is small.
enum Going {
    /// The view's, a step's or an edit's: made here, and published off the loop.
    View(Box<Publishing>),
    /// The layout's: made and written off the loop, since folding the record into this session and
    /// writing the two together is what a save of it costs.
    Layout(Box<Laying>),
}

/// A write of a record under way, with what [`Layouts::published`] takes from it.
struct Publishing {
    writing: crate::engine_write::Writing,
    candidate: automerge::AutoCommit,
    replaced: Option<Written>,
    revision: Option<[u8; 32]>,
}

/// The layout's write under way, and what the look that made it holds about it.
struct Laying {
    writing: crate::engine_write::Polled<Laid>,
    /// The tree revision and the record each window showed, as the write was made from them.
    own: u64,
    notes: Vec<(u64, String)>,
    revision: Option<[u8; 32]>,
    /// The person the change is theirs, whose step of the layout's history it is.
    person: String,
    /// The steps the write holds, kept for the next one if it is not made.
    pending: Vec<PendingStep<LayoutStep>>,
}

/// What a layout's write came to, off the loop.
enum Laid {
    /// The record held this session already: nothing written.
    Held {
        record: Box<automerge::AutoCommit>,
        /// The windows of this device the record holds.
        windows: BTreeSet<u64>,
    },
    /// What the write came to.
    Wrote(Box<Taken>),
    /// The write was not made and why: folding the record into this session, or writing them together,
    /// failed. The record to keep as this device's copy, where it still has one.
    Failed(String, Option<Box<automerge::AutoCommit>>),
}

/// What a write that did not answer before its session ended held, kept for the session after: a
/// desktop that ends cannot wait for its last write.
enum Unanswered {
    /// The steps of a layout's write, in the order the person made them.
    Layout(Vec<PendingStep<LayoutStep>>),
    /// The person's view a write of theirs was making.
    View(PersonView),
}

/// What a look writes of the person's view of a workspace's layout.
enum Viewed {
    /// The steps to write, the person's changes in the order they were made.
    Steps(Vec<PendingStep<PersonView>>),
    /// Nothing to write: the view did not change, or a write of it is under way.
    Nothing,
}

/// What this device wrote: its layout, the windows of it that write holds, the steps it writes,
/// and what the workspace did with it.
struct Taken {
    record: Box<automerge::AutoCommit>,
    windows: BTreeSet<u64>,
    /// The heads each step of the person's starts from and ends at, in the order the steps were
    /// made: each is taken back on its own.
    steps: Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
    /// The heads of the change itself, before what reached the record meanwhile was merged in: the
    /// record this session now shows.
    laid: Vec<ChangeHash>,
    took: crate::engine_write::Written,
}

/// What each window of a layout write shows, and the item it was opened on: the record its app
/// reports, by tab, and the item its launch named.
#[derive(Default)]
struct WindowNotes {
    shown: BTreeMap<u64, String>,
    items: BTreeMap<u64, String>,
}

impl WindowNotes {
    /// What a look hands a write: the records its windows' apps reported, and the items their
    /// launches named.
    fn of(notes: Vec<(u64, String)>, items: Vec<(u64, String)>) -> Self {
        Self {
            shown: notes.into_iter().collect(),
            items: items.into_iter().collect(),
        }
    }
}

/// What a look hands to the task that writes the layout: this device, and what it read of the
/// record.
struct Save {
    workspace_id: WorkspaceId,
    /// The workspace in front, as this session shows it.
    workspace: Workspace,
    /// The salt this device mints ids in.
    local: u16,
    /// The record as this device last wrote it and the workspace took it, and the workspace's own
    /// copy of it since.
    kept: Option<automerge::AutoCommit>,
    reached: Option<automerge::AutoCommit>,
    /// The heads of the record this session was laid out from.
    laid_at: Option<Vec<ChangeHash>>,
    /// The tree revision and the record each window showed, the arranging action's included.
    own: u64,
    notes: Vec<(u64, String)>,
    /// The item each of this device's windows was opened on, by tab.
    items: Vec<(u64, String)>,
    /// Whether a window of an app runs only on the device that opened it, as the app says
    /// ([`Layouts::hosting`]).
    runs_here: Option<fn(&str) -> bool>,
    /// What this device changed of the layout that its workspace does not hold yet, in the order it
    /// changed: each is written as its own change.
    pending: Vec<PendingStep<LayoutStep>>,
}

impl Save {
    /// Write it: the record this device writes on, with the record laid into this session's workspace
    /// and this device's own change on top, and the write of that to the workspace.
    async fn write(
        self,
        certificate: &DeviceCert,
        writes: &dyn tilo_peer_state::workspace_documents::DeviceWrites,
        person: &str,
        closed: &BTreeSet<u64>,
    ) -> Laid {
        let Self {
            workspace_id,
            workspace,
            local,
            kept,
            reached,
            laid_at,
            own: _,
            notes,
            items,
            runs_here,
            pending,
        } = self;
        let here = windows_of(&workspace);
        let mut base = base_of(workspace_id, kept, reached);
        let heads = base.committed_heads();
        // The layout is written against the record it was laid out from, and what reached the record
        // since is merged in after: what another member changed meanwhile, which this session does not
        // show yet, is theirs, and this write carries only what changed here.
        let mut candidate = match laid_on(&mut base, laid_at.as_deref()) {
            Ok(candidate) => candidate,
            Err(error) => return Laid::Failed(error, Some(Box::new(base))),
        };
        let laid_out = laid_into(&candidate, workspace, closed);
        // The windows this device's write holds that this session shows: one of the person's own
        // is never in a workspace's layout, so this device's write never holds it either
        // ([`tilo_desktop_model::personal`]).
        let windows: BTreeSet<u64> = laid_out
            .tabs()
            .filter(|tab| !tab.is_personal())
            .map(|tab| tab.id)
            .filter(|id| here.contains(id))
            .collect();
        // A change of a layout is the person's, written as this device: the record is every
        // member's, and another of their devices writing at the same instant must not number the
        // same change.
        candidate.set_actor(actor(certificate, person));
        let mut noted = WindowNotes::of(notes, items);
        let writing = Writing {
            certificate,
            local,
            only_here: runs_here,
            closed,
            person,
        };
        // A record nobody wrote yet is founded by this session: its strips stand in the record with
        // no window, so taking back the first window leaves the strip it stood in.
        let founding = heads.is_empty();
        if founding {
            let first = pending
                .first()
                .map_or(&laid_out, |step| &step.changed.workspace);
            let found = founding_change(&mut candidate, workspace_id, first, &writing, &noted);
            if let Err(error) = found {
                candidate.rollback();
                return Laid::Failed(error.to_string(), Some(Box::new(base)));
            }
        }
        let mut steps = Vec::new();
        let written = write_changes(
            &mut candidate,
            workspace_id,
            &laid_out,
            &pending,
            &writing,
            &mut noted,
            &mut steps,
        );
        let written = match written {
            Ok(written) => written,
            Err(error) => {
                candidate.rollback();
                return Laid::Failed(error.to_string(), Some(Box::new(base)));
            }
        };
        if !founding && !written {
            return Laid::Held {
                record: Box::new(base),
                windows,
            };
        }
        let laid = candidate.committed_heads();
        if let Err(error) = tilo_desktop_model::merge::merge_into(&mut candidate, &base) {
            return Laid::Failed(error.to_string(), Some(Box::new(base)));
        }
        let changes = candidate.save_after(&heads);
        // A write on no history is the whole record: the one that founds it.
        let took = writes
            .write(
                &workspace_id.to_string(),
                &changes,
                tilo_peer_state::workspace_documents::DocumentWriting {
                    app: DESKTOP_APP,
                    document: DESKTOP_DOCUMENT,
                    readers: None,
                    founding,
                    snapshot: founding,
                    sealed: None,
                },
            )
            .await;
        Laid::Wrote(Box::new(Taken {
            record: Box::new(candidate),
            windows,
            steps,
            laid,
            took,
        }))
    }
}

/// The document this write is made on: the record as it was laid out from, when this session still
/// holds that history, else the record as it stands. What reached the record since is merged in
/// after, so the write carries only what changed here.
fn laid_on(
    base: &mut automerge::AutoCommit,
    laid_at: Option<&[ChangeHash]>,
) -> Result<automerge::AutoCommit, String> {
    let Some(at) =
        laid_at.filter(|at| !at.is_empty() && at.iter().all(|head| base.has_change(head)))
    else {
        return Ok(base.clone());
    };
    base.fork_at(at).map_err(|error| error.to_string())
}

/// What every change of a workspace's layout is written with.
#[derive(Clone, Copy)]
struct Writing<'a> {
    certificate: &'a DeviceCert,
    local: u16,
    only_here: Option<fn(&str) -> bool>,
    closed: &'a BTreeSet<u64>,
    person: &'a str,
}

/// Stand the containers of a layout nobody wrote yet and this device's strips in `candidate`, with
/// no window: taking back the first window leaves the strip it stood in.
fn founding_change(
    candidate: &mut automerge::AutoCommit,
    workspace_id: WorkspaceId,
    workspace: &Workspace,
    writing: &Writing<'_>,
    windows: &WindowNotes,
) -> Result<(), automerge::AutomergeError> {
    let mine = |id: u64| peer_salt_of(id) == writing.local;
    let shows = |id: u64| windows.shown.get(&id).cloned();
    let items = |id: u64| windows.items.get(&id).cloned();
    let writer = device::Writer {
        person: Some(writing.person),
        mine: &mine,
        records: &shows,
        items: &items,
    };
    found(candidate, workspace_id, workspace, &writer)
}

/// Write what `pending` changed of this device's layout into `candidate`, in the order it changed,
/// each as its own change, and then the layout as it stands now: `laid_out`. Whether anything was
/// written. The heads each change of the person's starts from and ends at go to `steps`.
fn write_changes(
    candidate: &mut automerge::AutoCommit,
    workspace_id: WorkspaceId,
    laid_out: &Workspace,
    pending: &[PendingStep<LayoutStep>],
    writing: &Writing<'_>,
    windows: &mut WindowNotes,
    steps: &mut Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
) -> Result<bool, automerge::AutomergeError> {
    let mut wrote = false;
    for pending in pending {
        for (tab, shown) in pending.acted.iter().flat_map(|acted| &acted.shows) {
            windows.shown.entry(*tab).or_insert_with(|| shown.clone());
        }
        let acted = pending.acted.as_ref().map(|acted| acted.message.as_str());
        wrote |= write_change(
            candidate,
            workspace_id,
            &pending.changed.workspace,
            writing,
            acted,
            &pending.changed.step,
            windows,
            steps,
        )?;
    }
    let step = shared_resource::step_message(writing.person);
    wrote |= write_change(
        candidate,
        workspace_id,
        laid_out,
        writing,
        None,
        &step,
        windows,
        steps,
    )?;
    Ok(wrote)
}

/// Write `workspace`, this device's layout, into `candidate`, and commit what it changed as one
/// change: `acted`'s where an arranging action made it, `step`'s where the person arranged
/// something, and housekeeping where it arranges nothing. Whether anything was written.
fn write_change(
    candidate: &mut automerge::AutoCommit,
    workspace_id: WorkspaceId,
    workspace: &Workspace,
    writing: &Writing<'_>,
    acted: Option<&str>,
    step: &str,
    windows: &WindowNotes,
    steps: &mut Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
) -> Result<bool, automerge::AutomergeError> {
    let Writing {
        certificate,
        local,
        only_here,
        closed,
        person,
    } = *writing;
    let (laid, elsewhere) = written_here(
        laid_into(candidate, workspace.clone(), closed),
        certificate,
        local,
        only_here,
    );
    let mine = |id: u64| peer_salt_of(id) == local && !elsewhere.contains(&id);
    let shows = |id: u64| windows.shown.get(&id).cloned();
    let items = |id: u64| windows.items.get(&id).cloned();
    let writer = device::Writer {
        person: Some(person),
        mine: &mine,
        records: &shows,
        items: &items,
    };
    let was = arranged(candidate, workspace_id);
    let from = candidate.committed_heads();
    if !device::write(candidate, workspace_id, &laid, &writer)? {
        return Ok(false);
    }
    let theirs = acted.is_some() || arranged(candidate, workspace_id) != was;
    let message = match (acted, theirs) {
        (Some(acted), _) => acted.to_owned(),
        (None, true) => step.to_owned(),
        (None, false) => shared_resource::housekeeping_message(person),
    };
    candidate.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(message)
            .with_time(crate::activity::now_seconds()),
    );
    if theirs {
        steps.push((from, candidate.committed_heads()));
    }
    Ok(true)
}

/// This session's reading of the workspaces' records: the layout of each, and the view of it the
/// person here has.
#[derive(Default)]
pub(crate) struct Layouts {
    /// What each of this device's windows shows, as its app reports it: the record noted beside the
    /// window in the workspace's layout, so the person's other devices hold it and can continue the
    /// window there.
    notes: Arc<crate::continuations::Continuations>,
    /// The agent run that opened each window an agent opened, by workspace and tab, as each
    /// workspace's layout last said.
    opened_by: RefCell<HashMap<WorkspaceId, BTreeMap<u64, OpenedBy>>>,
    /// The windows each workspace's layout holds that this session showed, as it last wrote or
    /// laid them in: a window there that this session no longer holds is one this device closed,
    /// and its entry goes when the device writes again rather than the fold laying it back into
    /// the layout.
    written_windows: RefCell<HashMap<WorkspaceId, BTreeSet<u64>>>,
    /// The record each window an arranging action put on a record shows, as this device placed it:
    /// what the place shows from the moment it stands, before the layout's write of it lands.
    placed: RefCell<BTreeMap<u64, String>>,
    /// The windows each workspace's layout held when this session was last laid out from it: a window
    /// of this device's that is not among them is one it opened since, which the layout has not
    /// taken in yet.
    recorded: RefCell<HashMap<WorkspaceId, BTreeSet<u64>>>,
    /// Each workspace's layout as the last look left it: one that differs on the next look was
    /// changed here in between, in front or not, and is written.
    shown: RefCell<HashMap<WorkspaceId, (Vec<Strip>, WorkspaceLayout)>>,
    /// The workspaces whose change this device has not handed to a write yet: a window sent to a
    /// workspace behind, or a strip made in the turn the person switched away, is on a workspace
    /// the front's write never covers.
    owed: RefCell<BTreeSet<WorkspaceId>>,
    /// This session's own revision at the last look.
    seen: Cell<Option<u64>>,
    /// A failure to read or write a record that the person has not heard yet.
    refused: RefCell<Option<(WorkspaceId, RefusalReason)>>,
    /// The last failure the person heard: the same one again is not said twice.
    told: RefCell<Option<String>>,
    /// The session's loop, which a write under way wakes as it moves on.
    wake: tilo_service::LoopWake,
    /// This device's publisher, as the fold of a layout resolves the windows that run here.
    device: RefCell<String>,
    /// This device's certificate: whose key a change of a layout is numbered with, and the
    /// device a window running here names.
    certificate: RefCell<Option<Arc<DeviceCert>>>,
    /// How the person's workspaces stand on the device's daemon: a look reads the records again
    /// once that moved.
    view: RefCell<Option<Arc<WorkspaceView>>>,
    /// Where the workspaces' documents are read, once the session opened this device's service: a
    /// layout and its views are read from the workspace's own copy, as they are written to it.
    reads: RefCell<Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>>>,
    /// The reads of the layouts and views, made off the loop once the session opened its service.
    reading: RefCell<Option<reads::Reads>>,
    /// Where the service whose writes ask for those reads again is installed, until they start.
    notices: RefCell<Option<reads::Notices>>,
    /// What this device's workspace service writes into the workspaces' documents, once the
    /// session opened it: where a layout's write reaches the workspace.
    writes: RefCell<Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>>>,
    /// The workspace's own document as the device's daemon holds it, and the standing it was read
    /// at: its governance is read from there, never from a device's copy of it.
    governing: RefCell<HashMap<WorkspaceId, (u64, Option<automerge::AutoCommit>)>>,
    /// Moves whenever a layout is read or written here: what a history open on this device is
    /// served again for.
    moved: Cell<u64>,
    /// The person at this device, whose view of each workspace is theirs.
    person: RefCell<Option<String>>,
    /// Every record of every workspace this device reads and writes.
    held: RefCell<HashMap<Key, Held>>,
    /// Whether a window of an app runs only on the device that opened it, as the app says: a
    /// session that knows its apps' declarations says so ([`Self::hosting`]).
    runs_here: Option<fn(&str) -> bool>,
    /// Why each workspace's layout was last left unwritten on a look, said once each time it
    /// changes: what a change that never reached the workspace reads back.
    unwritten: RefCell<HashMap<WorkspaceId, &'static str>>,
    /// Where what this device could not write yet of its workspaces is kept, so a session that
    /// ends first leaves it for the one after.
    pending_at: Option<crate::kept::Kept>,
    /// What was last kept there: a look that keeps the same of it writes nothing.
    kept_pending: RefCell<Option<Vec<u8>>>,
    /// The item each window of this device's workspaces was opened on, by tab, as their layouts
    /// hold it and its own launches named it: what a window opens on again after a restart. Kept
    /// apart from the tab's view, which is the view a place shows.
    items: RefCell<BTreeMap<u64, String>>,
}

/// The layouts a session on `options` keeps, writing what its windows note of themselves (`notes`).
#[cfg(target_os = "linux")]
pub(crate) fn of_session(
    options: &crate::options::Options,
    notes: Arc<crate::continuations::Continuations>,
) -> Layouts {
    Layouts::new(
        notes,
        options.wake.clone(),
        options.person.clone(),
        options.state.as_deref(),
    )
    .hosting(crate::apps::runs_on_its_device)
}

impl Layouts {
    /// This session's records, writing what its windows note of themselves (`notes`), on the loop
    /// `wake` wakes, and each workspace's view kept for `person`. `state` is this device's state
    /// directory, where what a session could not write yet is kept for the one after.
    pub(crate) fn new(
        notes: Arc<crate::continuations::Continuations>,
        wake: tilo_service::LoopWake,
        person: String,
        state: Option<&std::path::Path>,
    ) -> Self {
        let layouts = Self {
            notes,
            wake,
            person: RefCell::new(Some(person)),
            pending_at: state
                .map(|state| crate::kept::Kept::open(state, crate::kept::PENDING_LAYOUT)),
            ..Self::default()
        };
        layouts.load_pending();
        layouts
    }

    /// Take up the item each window was opened on, as this session's shell knows it: what the
    /// layout holds and what a launch of this session named. An item never changes, so what is
    /// held first stands.
    pub(crate) fn note_items(&self, items: impl IntoIterator<Item = (u64, String)>) {
        let mut held = self.items.borrow_mut();
        for (tab, item) in items {
            held.entry(tab).or_insert(item);
        }
    }

    /// The item each window of the workspaces' layouts was opened on, by tab: what the restore
    /// opens a window on again.
    #[must_use]
    pub(crate) fn window_items(&self) -> BTreeMap<u64, String> {
        self.items.borrow().clone()
    }

    /// What this device could not write of its workspaces before the session before this one
    /// ended: each is written as its own step once its workspace can hold it.
    fn load_pending(&self) {
        let Some(at) = &self.pending_at else {
            return;
        };
        let bytes = match at.read() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "what this device could not write was not read");
                return;
            }
        };
        let kept: BTreeMap<String, Pending> = match serde_json::from_slice(&bytes) {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(%error, "what this device could not write was not read");
                return;
            }
        };
        let person = self.person.borrow().clone();
        for (workspace, pending) in kept {
            let Some(workspace) = WorkspaceId::parse(&workspace) else {
                continue;
            };
            if !pending.layout.is_empty() {
                self.held(&(workspace, Route::layout()), |held| {
                    held.pending_steps = PendingSteps(pending.layout);
                });
            }
            if let Some(person) = &person
                && !pending.view.is_empty()
            {
                self.held(&(workspace, Route::view(person)), |held| {
                    held.pending_view = PendingSteps(pending.view);
                });
            }
        }
    }

    /// Keep what this device has not written yet of its workspaces, for the session after this
    /// one: a desktop that ends before its workspaces can hold them leaves them here.
    fn keep_pending(&self) {
        let Some(at) = &self.pending_at else {
            return;
        };
        let view = self
            .person
            .borrow()
            .clone()
            .map(|person| Route::view(&person));
        let mut kept: BTreeMap<String, Pending> = BTreeMap::new();
        for ((workspace, route), held) in self.held.borrow().iter() {
            let pending = kept.entry(workspace.to_string()).or_default();
            if *route == Route::layout() {
                pending.layout.clone_from(&held.pending_steps.0);
            } else if Some(route) == view.as_ref() {
                pending.view.clone_from(&held.pending_view.0);
            }
        }
        kept.retain(|_, pending| !(pending.layout.is_empty() && pending.view.is_empty()));
        let Ok(bytes) = serde_json::to_vec(&kept) else {
            return;
        };
        if self.kept_pending.borrow().as_deref() == Some(bytes.as_slice()) {
            return;
        }
        *self.kept_pending.borrow_mut() = Some(bytes.clone());
        at.replace(bytes);
    }

    /// These records, of a session that hosts the windows that run only on this device:
    /// `runs_here` says whether a window of an app is one, as the app declares. Each is written
    /// into its workspace's layout as this device's window, which no other device opens.
    #[must_use]
    pub(crate) fn hosting(mut self, runs_here: fn(&str) -> bool) -> Self {
        self.runs_here = Some(runs_here);
        self
    }

    /// What this device holds of `key`, changed by `edit`.
    fn held<R>(&self, key: &Key, edit: impl FnOnce(&mut Held) -> R) -> R {
        edit(self.held.borrow_mut().entry(key.clone()).or_default())
    }

    /// Whether the layout of every workspace `tree` holds has been read at least once: what
    /// the workspaces held of this device's windows when the session started is laid out.
    ///
    /// A look that asked this device's own store counts, whether the store held the layout or
    /// not: a layout this device does not hold is one no local look can find, and what waits for
    /// it is the workspace's own governance, not this device. The windows kept across a restart are shown as
    /// soon as the layouts this device has are laid out, never after a wait for the network.
    pub(crate) fn all_read(&self, tree: &Layout) -> bool {
        tree.workspaces
            .iter()
            .filter(|workspace| !workspace.id().is_nil())
            .all(|workspace| self.held(&(workspace.id(), Route::layout()), |held| held.read_once))
    }

    /// Note that this look left `workspace_id`'s layout unwritten for `why`, said at debug when the
    /// reason changes; `None` once a write of it goes out.
    fn unwritten(&self, workspace_id: WorkspaceId, why: Option<&'static str>) {
        let mut unwritten = self.unwritten.borrow_mut();
        let was = match why {
            Some(why) => unwritten.insert(workspace_id, why),
            None => unwritten.remove(&workspace_id),
        };
        if was != why {
            tracing::debug!(
                %workspace_id,
                why = why.unwrap_or("written"),
                "a look leaves the workspace's layout as it stands here"
            );
        }
    }

    /// Every key this device holds a record of.
    fn keys(&self) -> Vec<Key> {
        self.held.borrow().keys().cloned().collect()
    }

    /// The windows this device has that the layout of `workspace_id` holds and this session no
    /// longer does: the close it made since it last wrote the layout.
    fn closed_since_written(
        &self,
        workspace_id: WorkspaceId,
        workspace: &Workspace,
    ) -> BTreeSet<u64> {
        let here = windows_of(workspace);
        self.written_windows
            .borrow()
            .get(&workspace_id)
            .map(|written| written.difference(&here).copied().collect())
            .unwrap_or_default()
    }

    /// Whether every record of every workspace `tree` holds was read here with the workspaces
    /// standing at `version` ([`Self::standing`]), and this session has not moved since: nothing of them can have changed, so no
    /// record is read or written again. Notes what was looked at.
    pub(crate) fn settled(&self, version: u64, tree: &Layout) -> bool {
        // A write under way is taken first, and while it is no record is read or written.
        if !self.settle_publishing() {
            return true;
        }
        // An answer that changed since the last look is read in, whatever else moved.
        let news = self
            .reading
            .borrow()
            .as_ref()
            .is_some_and(reads::Reads::take_news);
        let person = self.person.borrow().clone();
        let settled = !news
            && self.held.borrow().values().all(|held| {
                held.pending.is_none()
                    && held.pending_step.is_none()
                    && held.pending_view.is_empty()
                    && held.pending_steps.is_empty()
            })
            && self.seen.get() == Some(tree.external_revision)
            && tree
                .workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil())
                .all(|workspace| {
                    let workspace_id = workspace.id();
                    let current = |held: &mut Held| {
                        (held.read || held.failed.is_some()) && held.read_at == Some(version)
                    };
                    self.held(&(workspace_id, Route::layout()), current)
                        && person.as_deref().is_none_or(|person| {
                            self.held(&(workspace_id, Route::view(person)), current)
                        })
                });
        self.seen.set(Some(tree.external_revision));
        settled
    }

    /// `key`'s resource as the workspace's own copy in this device's service last answered it:
    /// what the workspace admitted, what it had before this device joined included. Asks for it
    /// to be read again off the loop, which the loop is woken for once that read answers
    /// something new. `Ok(None)` for one no one wrote yet, or one that did not move since `known`;
    /// [`NotRead::Waiting`] for one this device cannot read yet (its keys are on their way, or no
    /// read of it answered yet), which is no resource's absence.
    fn document(
        &self,
        key: &Key,
        known: Option<[u8; 32]>,
    ) -> Result<Option<(automerge::AutoCommit, [u8; 32])>, NotRead> {
        let Some(reading) = self.reading.borrow().clone() else {
            return Ok(None);
        };
        reading.read(key, known)
    }

    /// Lay every workspace's layout into `tree`, and each person's view of it: what the records say
    /// where, and what this device decides alone kept. `device` is this device's publisher, which
    /// resolves a window that runs here to the window it shows here. Returns whether anything
    /// changed.
    pub(crate) fn follow(&self, device: &str, tree: &mut Layout) -> bool {
        if !self.attached() {
            return false;
        }
        // The person's view of each workspace as the shell shows it before the layouts are laid
        // in: what they closed here is gone from it, and what a layout's fold takes off the shell
        // is not.
        let shown: HashMap<WorkspaceId, PersonView> = tree
            .workspaces
            .iter()
            .map(|workspace| (workspace.id(), PersonView::of(tree, workspace.id())))
            .collect();
        let changed = self.follow_layout(device, tree) | self.follow_view(tree, &shown);
        // What the workspaces' layouts laid in is theirs: only what changes here after it is
        // this device's to write.
        *self.shown.borrow_mut() = tree
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.id(),
                    (workspace.strips.clone(), workspace.layout.clone()),
                )
            })
            .collect();
        if self.device.borrow().as_str() != device {
            device.clone_into(&mut self.device.borrow_mut());
        }
        changed
    }

    /// Write what this device changed of `tree` and wait for it, for a session that is ending: a
    /// write under way would otherwise be cut with its task, and a change made since the last look
    /// — or held for the write before it — would not reach the workspace at all.
    pub(crate) fn finish_writes(&self, tree: &Layout, patience: std::time::Duration) {
        self.wait_writes(patience);
        self.read_through(patience);
        if let (Some(person), Some(certificate)) =
            (self.person.borrow().clone(), self.certificate())
        {
            let certificate = &certificate;
            self.capture_layouts(certificate, &person, tree);
            self.capture_view(certificate, &person, tree, true);
        }
        self.wait_writes(patience);
    }

    /// [`Self::finish_writes`] of the person's view alone: the layouts a login's end leaves as they
    /// were, and what the person sees of them is theirs to keep.
    pub(crate) fn finish_view(&self, tree: &Layout, patience: std::time::Duration) {
        self.wait_writes(patience);
        self.read_through(patience);
        if let (Some(person), Some(certificate)) =
            (self.person.borrow().clone(), self.certificate())
        {
            let certificate = &certificate;
            self.capture_view(certificate, &person, tree, true);
        }
        self.wait_writes(patience);
    }

    /// Wait for the writes under way, `patience` each.
    fn wait_writes(&self, patience: std::time::Duration) {
        for key in self.keys() {
            let Some(going) = self.held(&key, |held| held.writing.take()) else {
                continue;
            };
            match going {
                Going::View(mut publishing) => {
                    let Some(written) = publishing.writing.finish(patience) else {
                        // The write did not answer before the session ends: what it held is what
                        // the person sees, and the session after writes it as their step.
                        if let Some(Written::View(view)) = &publishing.replaced {
                            self.keep_for_next_session(&key, Unanswered::View(view.clone()));
                        }
                        continue;
                    };
                    let publishing = *publishing;
                    self.published(
                        &key,
                        written,
                        publishing.candidate,
                        publishing.replaced,
                        publishing.revision,
                    );
                }
                Going::Layout(mut laying) => {
                    let Some(laid) = laying.writing.finish(patience) else {
                        // The write did not answer before the session ends: the steps it held are
                        // the person's, and the session after writes them.
                        self.keep_for_next_session(
                            &key,
                            Unanswered::Layout(laying.pending.clone()),
                        );
                        continue;
                    };
                    self.laid(&key, laid, &laying);
                }
            }
        }
    }

    /// A number that moves whenever a layout is read or written here.
    pub(crate) fn moved(&self) -> u64 {
        self.moved.get()
    }

    /// Read the workspaces' documents through `reads`, this device's workspace service, and write
    /// the layouts through `writes`, its one commit path: what a look reads is what a write
    /// reaches.
    pub(crate) fn attach(
        &self,
        certificate: &DeviceCert,
        view: &Arc<WorkspaceView>,
        reads: &Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        writes: &Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    ) {
        *self.certificate.borrow_mut() = Some(Arc::new(certificate.clone()));
        *self.view.borrow_mut() = Some(Arc::clone(view));
        *self.reads.borrow_mut() = Some(Arc::clone(reads));
        *self.writes.borrow_mut() = Some(Arc::clone(writes));
        if self.reading.borrow().is_none() {
            *self.reading.borrow_mut() = Some(reads::Reads::start(
                Arc::clone(reads),
                self.wake.handle(),
                self.notices.borrow_mut().take(),
            ));
        }
    }

    /// Read a layout or a view again whenever the device's daemon takes a write to it, rather
    /// than only when the session looks.
    pub(crate) fn follow_writes(
        &self,
        writes: tilo_session_client::daemon_documents::DaemonWritesFeed,
    ) {
        *self.notices.borrow_mut() = Some(Box::new(move || Some(writes.follow())));
    }

    /// Read every layout and view followed again, and wait up to `patience` for the answers.
    fn read_through(&self, patience: std::time::Duration) {
        let reading = self.reading.borrow().clone();
        if let Some(reading) = reading {
            reading.read_through(patience);
        }
    }

    /// Where the workspaces' documents are read, once the session opened its service.
    fn reads(&self) -> Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>> {
        self.reads.borrow().clone()
    }

    /// The writes into the workspaces' documents, once the session opened its service.
    fn writes(&self) -> Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>> {
        self.writes.borrow().clone()
    }

    /// This device's certificate, once the session attached its documents.
    fn certificate(&self) -> Option<Arc<DeviceCert>> {
        self.certificate.borrow().clone()
    }

    /// Whether the session attached the documents its layouts are read and written through.
    pub(crate) fn attached(&self) -> bool {
        self.certificate.borrow().is_some()
    }

    /// How the person's workspaces stand on the device's daemon now: it moves whenever a
    /// workspace's people, policy or keys change there, and a record read before is read again.
    pub(crate) fn standing(&self) -> u64 {
        self.view
            .borrow()
            .as_ref()
            .map_or(0, |view| view.revision())
    }

    /// The layout of `workspace_id`, as this device last read it.
    pub(crate) fn layout(&self, workspace_id: WorkspaceId) -> Option<automerge::AutoCommit> {
        self.held(&(workspace_id, Route::layout()), |held| held.kept.clone())
    }

    /// The workspace's own document as this device holds it: its governance, and the layout it
    /// keeps at its root. `None` until the workspace is held here.
    pub(crate) fn governance(&self, workspace_id: WorkspaceId) -> Option<automerge::AutoCommit> {
        let view = self.view.borrow().clone()?;
        let revision = view.revision();
        let mut governing = self.governing.borrow_mut();
        let (read_at, document) = governing
            .entry(workspace_id)
            .or_insert((revision.wrapping_sub(1), None));
        if *read_at != revision {
            *document = view
                .workspace_document(&workspace_id.to_string())
                .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok());
            *read_at = revision;
        }
        document.clone()
    }

    /// A number of this device's workspaces as they stand, governance included: what a history
    /// open here is served again for.
    pub(crate) fn owner_revision(&self) -> u64 {
        self.standing()
    }

    /// Fold the layout of every workspace the device's daemon holds into `tree`.
    fn follow_layout(&self, device: &str, tree: &mut Layout) -> bool {
        let salt = tree.salt;
        let at = self.standing();
        let mut changed = false;
        for workspace in &mut tree.workspaces {
            let workspace_id = workspace.id();
            if workspace_id.is_nil() {
                continue;
            }
            let key = (workspace_id, Route::layout());
            // A write of this layout under way holds what the person just changed: the record has
            // not taken it yet, and folding the record over the shell would put it back. A write
            // that answered is taken first, so the next one never starts before the record is read.
            if !self.settle_one(&key) {
                continue;
            }
            let known = self.held(&key, |held| held.revision);
            let document = self.document(&key, known);
            // Read at this revision, whatever it held: a layout that fails to read fails again
            // until the workspace moves. One whose keys have not arrived is not read at all: the
            // restore and the kept windows wait for it, and every look asks again until it reads.
            let waiting = matches!(document, Err(NotRead::Waiting));
            let failed = match &document {
                Err(NotRead::Failed(error)) => Some(error.clone()),
                _ => None,
            };
            let told = self.held(&key, |held| {
                let told = held.failed.is_some() && held.failed == failed;
                held.read_at = Some(at);
                held.read_once |= !waiting;
                held.failed.clone_from(&failed);
                told
            });
            if let (Some(error), false) = (&failed, told) {
                tracing::warn!(%workspace_id, error, "a workspace's layout could not be read");
            }
            let Ok(document) = document else {
                changed |= self.owed(workspace_id, workspace);
                continue;
            };
            let Some((doc, revision)) = document else {
                // Read against a revision, nothing comes back while the record did not move: what
                // was read stands. A workspace with no layout yet is read as one: a layout that
                // arrives is told by the daemon, and the next pass reads it.
                self.held(&key, |held| held.read = true);
                changed |= self.owed(workspace_id, workspace);
                continue;
            };
            // The workspace's copy reaches this device after the workspace admitted this device's
            // own write, so it can lack it: what is shown is the workspace's copy with that write
            // in it, or the layout would take back what this device just did.
            let mut doc = base_of(
                workspace_id,
                self.held(&key, |held| held.kept.clone()),
                Some(doc),
            );
            // The layout's own history is what a step of it is taken back from: a session that
            // finds it here counts what it already holds as somebody's step, not as its own open
            // change.
            let folding = self.held(&key, |held| {
                held.read = true;
                held.revision = Some(revision);
                if held.undo.is_none() {
                    held.undo = Some(history(&mut doc));
                }
                held.kept = Some(doc.clone());
                held.based = Some(revision);
                let heads = doc.get_heads();
                if held.folded.as_ref() == Some(&heads) {
                    return None;
                }
                held.folded = Some(heads);
                Some(doc.clone())
            });
            let Some(doc) = folding else {
                continue;
            };
            let layout = shared_resource::read(&doc, None).unwrap_or_default();
            self.opened_by
                .borrow_mut()
                .insert(workspace_id, layout.opened_by);
            self.items.borrow_mut().extend(layout.items);
            changed |= self.fold_into(workspace_id, &doc, &layout.tree, workspace, salt, device);
            changed |= self.owed(workspace_id, workspace);
        }
        changed
    }

    /// Show in `workspace` what this device could not write of it before this session: its last
    /// step waits for the workspace to hold it, and the session shows what the person arranged
    /// until then. Whether anything changed.
    fn owed(&self, workspace_id: WorkspaceId, workspace: &mut Workspace) -> bool {
        let Some(mut owed) = self.held(&(workspace_id, Route::layout()), |held| {
            held.pending_steps
                .last()
                .map(|step| step.changed.workspace.clone())
        }) else {
            return false;
        };
        // A window of the person's own is their view's to place, never the layout's: the step
        // keeps the shell this device showed, and laying its windows in would put one back in the
        // workspace the person moved it from. The ones this shell shows stay as they are shown.
        tilo_desktop_model::personal::leave_out_of(&mut owed);
        keep_personal(workspace, &mut owed);
        // The step is where the windows stood, not what shows in them: a window that mapped
        // since the step was kept is shown by its live surface, and the step's older tile (the
        // app it waited for) would make it a window still to open.
        let shown: HashMap<u64, TileContent> = workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| (tab.id, tab.tile.clone()))
            .collect();
        for tab in owed
            .strips
            .iter_mut()
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
        {
            if let Some(tile) = shown.get(&tab.id) {
                tab.tile.clone_from(tile);
            }
        }
        if owed.strips == workspace.strips && owed.layout == workspace.layout {
            return false;
        }
        workspace.strips = owed.strips;
        workspace.layout = owed.layout;
        true
    }

    /// Show the layout `layout`, what the record `doc` reads as, in `workspace`, keeping what this
    /// device decides alone: a strip or a window this device made that the layout does not hold
    /// yet. Whether anything changed.
    fn fold_into(
        &self,
        workspace_id: WorkspaceId,
        doc: &automerge::AutoCommit,
        layout: &Layout,
        workspace: &mut Workspace,
        salt: u16,
        device: &str,
    ) -> bool {
        let mine = |id: u64| salt != 0 && peer_salt_of(id) == salt;
        let closed = self.closed_since_written(workspace_id, workspace);
        let recorded = self
            .recorded
            .borrow_mut()
            .insert(workspace_id, tabs_of(layout));
        let written = self
            .written_windows
            .borrow()
            .get(&workspace_id)
            .cloned()
            .unwrap_or_default();
        // The layout opens empty and takes its windows from the records, so a window of this
        // device's that the record has not held yet, read or written here, is one this session
        // opened. One it held is gone from it when a step takes it back.
        let opened_here = |id: u64| {
            mine(id)
                && !written.contains(&id)
                && recorded
                    .as_ref()
                    .is_none_or(|recorded| !recorded.contains(&id))
        };
        let kept = device::Kept {
            // A strip this device made and left empty stays on its own layout: the record never
            // holds an empty strip it made, and an empty row arriving here is nobody else's to
            // see.
            mine: &mine,
            // A window this device opened that the record does not hold yet stays: another
            // member's change that arrives before this device's own write comes back is no
            // close of it.
            private: &opened_here,
            // A window this device closed is not laid back into its layout from the record: the
            // write that follows takes its entry out instead.
            closed: &|id| closed.contains(&id),
            writing: false,
        };
        let changed = device::fold(doc, workspace, device, &kept);
        // What the layout laid in here, this session shows from now on: one of those windows gone
        // from it later (the person closed it, or the restore closed the tab of an app that ended
        // while the desktop was away) is a close this device writes, never a window the next
        // write lays back from the workspace's layout. Only windows that layout holds count: one
        // this device opened and has not written yet is not the layout's to take back.
        let holds = tabs_of(layout);
        self.written_windows
            .borrow_mut()
            .entry(workspace_id)
            .and_modify(|known| known.retain(|id| holds.contains(id)))
            .or_default()
            .extend(windows_of(workspace).intersection(&holds));
        changed
    }

    /// Show every workspace in `tree` as the person here last left it. `shown` is the person's view
    /// of each as the shell showed it before this look laid the layouts in.
    fn follow_view(&self, tree: &mut Layout, shown: &HashMap<WorkspaceId, PersonView>) -> bool {
        let Some(person) = self.person.borrow().clone() else {
            return false;
        };
        let route = Route::view(&person);
        let at = self.standing();
        let held: Vec<WorkspaceId> = tree.workspaces.iter().map(Workspace::id).collect();
        let mut changed = false;
        for workspace_id in held {
            if workspace_id.is_nil() {
                continue;
            }
            let key = (workspace_id, route.clone());
            // A write of this view under way holds what the person just changed: the record has not
            // taken it yet, and laying the record over the shell would put it back.
            if self.held(&key, |held| held.writing.is_some()) {
                continue;
            }
            // The windows of the person's own go on their layout wherever it is shown: the layout
            // a workspace shares never holds them, so they come from the view this device keeps of
            // it — the changes it made that the workspace does not hold yet, else what it read
            // last. A workspace that holds no view of them yet is no different: they are the
            // person's, and the session shows them before the workspace has one.
            // A window the person closed here since is gone, whatever the kept view still says of
            // it: until their view rests and is written again, it names the window, and laying it
            // back would open a window they closed.
            let kept = self.held(&key, |held| {
                let kept = held
                    .pending_view
                    .last()
                    .map(|step| step.changed.clone())
                    .or_else(|| match &held.written {
                        Some(Written::View(view)) => Some(view.clone()),
                        _ => None,
                    })?;
                Some(match shown.get(&workspace_id) {
                    Some(here) => kept.over(here, held.seen.as_ref()),
                    None => kept,
                })
            });
            if let Some(view) = kept
                && let Some(workspace) = tree
                    .workspaces
                    .iter_mut()
                    .find(|held| held.id() == workspace_id)
            {
                changed |= view.place_own(workspace);
            }
            let known = self.held(&key, |held| held.revision);
            let document = self.document(&key, known);
            self.held(&key, |held| held.read_at = Some(at));
            let (doc, revision) = match document {
                Ok(Some(read)) => read,
                // Read against a revision, nothing came back while the view did not move: what
                // was read stands.
                Ok(None) if known.is_some() => {
                    self.held(&key, |held| {
                        held.read = true;
                        held.unreadable = false;
                    });
                    continue;
                }
                // The workspace holds no view of this person yet: what it holds is no view at all,
                // and the view the person has here is written once it rests. One the workspace
                // holds and this look cannot read is nobody's to write over, so nothing counts as
                // written until a look reads it.
                failed => {
                    let waiting = matches!(failed, Err(NotRead::Waiting));
                    // A document the service holds nothing of reads as absent, whether it says so
                    // or answers nothing: the view here is written over no view at all. One this
                    // look cannot read yet is waited for, never written.
                    let absent = !waiting && self.nothing_written(workspace_id, &route);
                    self.held(&key, |held| {
                        held.read = !waiting;
                        held.unreadable = !absent;
                        if absent && held.written.is_none() {
                            held.written = Some(Written::View(PersonView::default()));
                        }
                    });
                    continue;
                }
            };
            let doc = base_of(
                workspace_id,
                self.held(&key, |held| held.kept.clone()),
                Some(doc),
            );
            let read = PersonView::read(&doc);
            // What this device wrote or read last is on the shell already, or changed since by the
            // person here: only a view another of their devices wrote is news. What the person
            // changed here since stays over it: another device's view holds its own front tabs as
            // it left them. The record holds what was read, so the change kept here differs from
            // it and a look writes it.
            let news = self.held(&key, |held| {
                held.written.as_ref() != Some(&Written::View(read.clone()))
            });
            let here = PersonView::of(tree, workspace_id);
            let mut alone = tree.clone();
            read.applied(&mut alone, workspace_id);
            let view = self.held(&key, |held| {
                let mut view = read.over(&here, held.seen.as_ref());
                // A change of the person's this device could not write is what they see until
                // the workspace holds it, as [`Self::owed`] shows a layout step: a window it moved
                // is not laid back where the older document has it.
                if let Some(pending) = held.pending_view.last() {
                    view.own.retain(|tab, _| {
                        here.own.contains_key(tab) || pending.changed.own.contains_key(tab)
                    });
                }
                held.seen = Some(PersonView::of(&alone, workspace_id));
                held.read = true;
                held.unreadable = false;
                held.revision = Some(revision);
                held.written = Some(Written::View(read));
                if held.undo.is_none() {
                    let mut from = doc.clone();
                    held.undo = Some(history(&mut from));
                }
                held.kept = Some(doc);
                held.based = Some(revision);
                view
            });
            if news {
                changed |= view.applied(tree, workspace_id);
            }
        }
        changed
    }

    /// The agent run that opened each window an agent opened, by tab, across the workspaces this
    /// device shows: what each window's bar names.
    pub(crate) fn opened_by(&self) -> BTreeMap<u64, OpenedBy> {
        self.opened_by
            .borrow()
            .values()
            .flat_map(|runs| runs.iter().map(|(tab, run)| (*tab, run.clone())))
            .collect()
    }

    /// Write what this device changed about the layout of each workspace into that workspace's
    /// layout, and the person's view of every workspace into theirs, and publish them: the strips
    /// and windows it makes and arranges, and where the person here is looking. Returns whether
    /// this look published the view.
    pub(crate) fn capture(&self, tree: &Layout) -> bool {
        let (Some(person), Some(certificate)) = (self.person.borrow().clone(), self.certificate())
        else {
            return false;
        };
        self.capture_layouts(&certificate, &person, tree);
        self.capture_view(&certificate, &person, tree, false)
    }

    /// Write the layout of the workspace in front, and of every other workspace this device
    /// changed since the last look or still owes a write: a window sent to a workspace behind
    /// changes two layouts, and a restart reads both.
    fn capture_layouts(&self, certificate: &Arc<DeviceCert>, person: &str, tree: &Layout) {
        let owed = {
            let shown = self.shown.borrow();
            let mut owed = self.owed.borrow_mut();
            owed.extend(tree.workspaces.iter().filter_map(|workspace| {
                let was = shown.get(&workspace.id())?;
                (was.0 != workspace.strips || was.1 != workspace.layout).then(|| workspace.id())
            }));
            owed.insert(tree.active);
            for workspace in &tree.workspaces {
                let pending = self.held(&(workspace.id(), Route::layout()), |held| {
                    !held.pending_steps.is_empty()
                });
                if pending {
                    owed.insert(workspace.id());
                }
            }
            owed.retain(|id| tree.workspaces.iter().any(|held| held.id() == *id));
            owed.clone()
        };
        for workspace in owed {
            if self.capture_layout(certificate, person, tree, workspace, None) {
                self.owed.borrow_mut().remove(&workspace);
            }
        }
    }

    /// Write this device's changes to the layout of `workspace` into that workspace's own, each
    /// change made while it could not write them as a step of its own, in the order it was made:
    /// `acted`'s where an arranging action just made one, else the person's own edit.
    /// Handing them to the task that writes them is all the loop does: folding the record into this
    /// layout's workspace and writing the two together is what a save costs, and none of it runs here.
    /// Whether this look is done with the workspace: what it could not hand yet is tried again.
    fn capture_layout(
        &self,
        certificate: &Arc<DeviceCert>,
        person: &str,
        tree: &Layout,
        workspace: WorkspaceId,
        acted: Option<Acted>,
    ) -> bool {
        let Some(workspace) = tree.workspaces.iter().find(|held| held.id() == workspace) else {
            return true;
        };
        let workspace_id = workspace.id();
        let local = tree.salt;
        if workspace_id.is_nil() || local == 0 {
            return true;
        }
        let key = (workspace_id, Route::layout());
        // What the person changed since the last look: a layout the workspace does not hold yet is
        // a step of theirs, and one this session has not shown before is the shell it opens, which
        // is no change of theirs.
        let changed = self
            .shown
            .borrow()
            .get(&workspace_id)
            .is_some_and(|(strips, layout)| {
                *strips != workspace.strips || *layout != workspace.layout
            });
        // What this device has to share: the strips it shows, and what stands in them. The
        // layout's shape is the workspace's layout, so a workspace with strips is one however few
        // windows it holds; a workspace with no strip at all is one this device shows nothing of.
        let shares = !workspace.strips.is_empty();
        // The shell this look shows is the person's to keep where it changed, and where the
        // workspace holds nothing of it here yet: a change that cannot be written now is kept for
        // the look that can write it, never dropped with the session.
        let kept_back = changed || (shares && !self.held(&key, |held| held.folded.is_some()));
        // The layout the person changes while a write of it is under way is written on the look after
        // the workspace answered that one, and one whose last write waits for the record on the look
        // after the record moved. An arranging action's change keeps its name until then: it is the
        // write that says what was done.
        if let Some(why) = self.held_back(&key) {
            self.unwritten(workspace_id, Some(why));
            if kept_back {
                self.remember(&key, workspace, acted, person);
            }
            return false;
        }
        let read = self.reached(&key);
        let (reached, revision, holds) = (read.doc, read.revision, read.holds);
        // A layout the workspace holds is laid out here before this device writes it: what the
        // layout shows before then is not what the person made of it, and one this device cannot
        // read yet (its keys are on their way) is never written over. A copy this device wrote is
        // what its next write is made on, with the workspace's merged in. A document the look read
        // as nothing may still be the workspace's: the service's copy is loaded as the store opens,
        // and its shell written over it meanwhile would take back every window it holds.
        if !holds
            && (read.waiting
                || (reached.is_some() || self.holds_layout(workspace_id))
                    && self.held(&key, |held| held.folded.is_none()))
        {
            self.unwritten(
                workspace_id,
                Some("the workspace's layout is not read here yet"),
            );
            // What this session shows before it read the layout is not a change of the person's:
            // the shell it opens with is not their step, and writing it would take back the
            // windows the workspace holds. The look after the read writes what they do change.
            return false;
        }
        // A record nobody wrote yet is founded by the first layout with a strip in it: a device with
        // nothing to show founds nothing, so the record stays one the devices that do show
        // something make.
        if reached.is_none() && !holds && !shares {
            self.unwritten(
                workspace_id,
                Some("nobody wrote it and this layout shows nothing"),
            );
            return true;
        }
        let mut notes = self.notes.of_workspace(workspace_id);
        // A layout that did not move since this device last wrote it has nothing to write: a
        // record that moved meanwhile is another member's change, which the look's follow lays into
        // this session, and a write of the layout as it stood would take that change back.
        let own = tree.external_revision;
        if acted.is_none()
            && self.held(&key, |held| {
                held.pending_steps.is_empty()
                    && held.written == Some(Written::Layout(own, notes.clone()))
            })
        {
            self.unwritten(
                workspace_id,
                Some("this layout did not move since it wrote it"),
            );
            return true;
        }
        if let Some(acted) = &acted {
            for (tab, record) in &acted.shows {
                if !notes.iter().any(|(held, _)| held == tab) {
                    notes.push((*tab, record.clone()));
                }
            }
        }
        if changed || acted.is_some() {
            self.remember(&key, workspace, acted, person);
        }
        let items = self.window_items_of(workspace);
        let pending = self.held(&key, |held| held.pending_steps.take());
        self.keep_pending();
        self.unwritten(workspace_id, None);
        self.lay(
            certificate,
            person,
            revision,
            Save {
                workspace_id,
                workspace: workspace.clone(),
                local,
                // The record is read while the write under way runs — the layout's records,
                // the pane's history — so the write takes a copy of it and this device keeps its.
                kept: self.held(&key, |held| held.kept.clone()),
                reached,
                laid_at: self.held(&key, |held| held.folded.clone()),
                own,
                notes,
                items,
                runs_here: self.runs_here,
                pending,
            },
        );
        true
    }

    /// The item each window of `workspace` was opened on, by tab.
    fn window_items_of(&self, workspace: &Workspace) -> Vec<(u64, String)> {
        let held = self.items.borrow();
        workspace
            .tabs()
            .filter_map(|tab| held.get(&tab.id).cloned().map(|item| (tab.id, item)))
            .collect()
    }

    /// Keep the change this look makes of `workspace` as a step of its own, written once it can be:
    /// the layout as it stands after it, the arranging action that named it, and the step it is of
    /// `person`'s, named here where they made it.
    fn remember(&self, key: &Key, workspace: &Workspace, acted: Option<Acted>, person: &str) {
        self.held(key, |held| {
            let moved = acted.is_some()
                || held.pending_steps.last().is_none_or(|last| {
                    last.changed.workspace.strips != workspace.strips
                        || last.changed.workspace.layout != workspace.layout
                });
            if moved {
                held.pending_steps.push(
                    LayoutStep {
                        workspace: workspace.clone(),
                        step: shared_resource::step_message(person),
                    },
                    acted,
                );
            }
        });
        self.keep_pending();
    }

    /// Hand `save` to the task that writes it, and take what it came to at once if it is there
    /// already.
    fn lay(
        &self,
        certificate: &Arc<DeviceCert>,
        person: &str,
        revision: Option<[u8; 32]>,
        save: Save,
    ) {
        let Some(writes) = self.writes() else {
            tracing::debug!(
                workspace = %save.workspace_id,
                "the layout has no service to write it, and is kept for the look after one opens"
            );
            self.unwritten(
                save.workspace_id,
                Some("its workspace service is not open yet"),
            );
            return;
        };
        let certificate = Arc::clone(certificate);
        let person = person.to_owned();
        let key = (save.workspace_id, Route::layout());
        let (own, notes) = (save.own, save.notes.clone());
        // The steps the write holds, kept for the next one if it is not made.
        let pending = save.pending.clone();
        let closed = self.closed_since_written(save.workspace_id, &save.workspace);
        let writing = crate::engine_write::Polled::new(
            self.wake.handle(),
            {
                let person = person.clone();
                async move {
                    Box::pin(save.write(&certificate, writes.as_ref(), &person, &closed)).await
                }
            },
            || {
                Laid::Failed(
                    "the layout's task stopped before it answered".to_owned(),
                    None,
                )
            },
        );
        let mut laying = Laying {
            writing,
            own,
            notes,
            revision,
            person,
            pending,
        };
        let Some(laid) = laying.writing.done() else {
            self.held(&key, |held| {
                held.writing = Some(Going::Layout(Box::new(laying)));
            });
            return;
        };
        self.laid(&key, laid, &laying);
    }

    /// What a layout's write came to: the record this device writes on next, the person's step of
    /// the layout's history it is, and what they hear of it.
    fn laid(&self, key: &Key, laid: Laid, laying: &Laying) {
        let workspace_id = key.0;
        // Whatever reached the record while the write was under way is read on the next look.
        self.held(key, |held| held.read_at = None);
        let written = || Written::Layout(laying.own, laying.notes.clone());
        match laid {
            Laid::Held { record, windows } => {
                tracing::debug!(%workspace_id, "the layout's write found nothing to write");
                // Nothing to write: what this device read is the copy its next write is made on.
                let written = written();
                self.written_windows
                    .borrow_mut()
                    .insert(workspace_id, windows);
                self.held(key, |held| held.written = Some(written));
                self.keep(key, *record, laying.revision);
            }
            Laid::Wrote(taken) => match taken.took {
                Ok(_) => {
                    tracing::debug!(%workspace_id, "the layout's write was taken");
                    let written = written();
                    self.written_windows
                        .borrow_mut()
                        .insert(workspace_id, taken.windows);
                    // Each change is a step of this person's in the layout's own history: an undo
                    // takes back the last one, whichever of their devices asks for it. A write can
                    // land before this device read the layout (the first window a session opens
                    // goes out on the pass before it is read): the history begins here, and the
                    // changes it makes are its first steps.
                    let steps = taken.steps.clone();
                    self.held(key, |held| {
                        held.written = Some(written);
                        held.folded = Some(taken.laid.clone());
                        if !steps.is_empty() {
                            let log = held.undo.get_or_insert_with(|| {
                                let mut from = (*taken.record).clone();
                                history(&mut from)
                            });
                            for (from, to) in steps {
                                log.record_committed_span(
                                    &taken.record,
                                    from,
                                    to,
                                    None,
                                    None,
                                    Some(user(&laying.person)),
                                    None,
                                );
                            }
                        }
                    });
                    self.keep(key, *taken.record, laying.revision);
                }
                Err(WriteRefusal::Waiting(_)) => {
                    // Nothing was refused: the workspace does not hold what the write builds on
                    // yet. The steps stay this device's, and are written once the record moves.
                    let steps = laying.pending.clone();
                    self.held(key, |held| {
                        held.written = None;
                        held.waits = Some(Waits { at: held.revision });
                        held.pending_steps.restore(steps);
                    });
                    self.keep_pending();
                }
                Err(error) => {
                    // The record is not the workspace's until it takes it: the next write is made
                    // on what the workspace holds, not on the write it refused. A history over the
                    // refused copy names changes the workspace does not hold. The same layout on the
                    // same record is not written again: the workspace would refuse it again.
                    self.held(key, |held| {
                        held.written = Some(written());
                        held.kept = None;
                        held.based = None;
                        held.undo = None;
                    });
                    self.say(
                        (error.reason(), &error.to_string()),
                        "the workspace's layout was not published",
                        workspace_id,
                    );
                }
            },
            Laid::Failed(error, record) => {
                // What was refused is not written, and the look that found it is not one that wrote
                // it: the next look tries again, with the steps this write held before whatever
                // changed since.
                let steps = laying.pending.clone();
                self.held(key, |held| {
                    held.written = None;
                    held.kept = record.map(|record| *record);
                    held.pending_steps.restore(steps);
                });
                self.keep_pending();
                self.say(
                    (RefusalReason::Other, &error),
                    "the workspace's layout was not kept",
                    workspace_id,
                );
            }
        }
    }

    /// The last step of `person`'s in `workspace_id` they can take back: who made it and what it
    /// did.
    pub(crate) fn last_step(
        &self,
        workspace_id: WorkspaceId,
        person: &str,
    ) -> Option<(tilo_actions::Actor, String)> {
        use tilo_actions::Did;
        // Of two steps made at no known time, the layout's (the later one compared wins).
        let (_, entry) = [Route::view(person), Route::layout()]
            .into_iter()
            .filter_map(|route| {
                self.held(&(workspace_id, route), |held| {
                    let (entry, at) =
                        next_step(held.undo.as_ref()?, held.kept.as_ref()?, person, false)?;
                    Some((at, entry))
                })
            })
            .max_by_key(|(at, _)| *at)?;
        let step = entry.step?;
        // Who asked for it: the invoker the step records, else the person whose layout it is.
        let who = step.invoker.map_or_else(
            || tilo_actions::MemberId::person(person).into(),
            |invoker| invoker.acting.by,
        );
        let what = match step.what {
            Some(Did::Verb(call)) => call.action.action,
            _ => String::new(),
        };
        Some((who, what))
    }

    /// Take back (`redo` false) or bring back `person`'s last step in `workspace_id` — of its
    /// layout, or of their own view of it, whichever they made last — show what it came to on this
    /// device's layout, and publish it. Whether a step was taken.
    pub(crate) fn step(
        &self,
        workspace_id: WorkspaceId,
        person: &str,
        redo: bool,
        handle: &DesktopHandle,
    ) -> bool {
        let Some(certificate) = self.certificate() else {
            return false;
        };
        let certificate = &certificate;
        let keys = [
            (workspace_id, Route::layout()),
            (workspace_id, Route::view(person)),
        ];
        let busy = keys.iter().fold(false, |busy, key| {
            !self.settle_one(key)
                | self.held(key, |held| {
                    !(held.pending_view.is_empty() && held.pending_steps.is_empty())
                })
                | busy
        });
        if busy {
            // A layout the step chooses from is being written, or its steps are not written yet: it
            // is taken on the look that finds them written, on what the workspace then holds. The
            // person asked, and it is theirs to take — their gesture is not refused because a write
            // of the same layout was quicker.
            self.held(&keys[0], |held| held.pending_step = Some(redo));
            return true;
        }
        // The person's last step: the newest of either record's, and both where one arranging
        // action changed both (its gesture is the same in each).
        let mut candidates = Vec::new();
        for key in keys {
            let Some((mut doc, revision)) = self.copy(&key) else {
                continue;
            };
            doc.set_actor(actor(certificate, person));
            let log = self
                .held(&key, |held| held.undo.take())
                .unwrap_or_else(|| history(&mut doc));
            let at = next_step(&log, &doc, person, redo).map(|(_, at)| at);
            candidates.push((
                at,
                Candidate {
                    key,
                    doc,
                    log,
                    revision,
                },
            ));
        }
        // Of two steps made at no known time, the layout's.
        let newest = candidates
            .iter()
            .filter_map(|(at, _)| *at)
            .max()
            .unwrap_or_default();
        let mut taken = Vec::new();
        for (at, candidate) in candidates {
            let take = at.is_some_and(|at| at == newest) && (newest.is_some() || taken.is_empty());
            if take {
                taken.push(candidate);
            } else {
                self.held(&candidate.key, |held| held.undo = Some(candidate.log));
            }
        }
        let mut stepped = false;
        for candidate in taken {
            stepped |= self.take_step(workspace_id, person, redo, handle, candidate);
        }
        stepped
    }

    /// Take `person`'s step of `candidate`'s record, show what it came to on this device,
    /// and publish it. Whether there was one.
    fn take_step(
        &self,
        workspace_id: WorkspaceId,
        person: &str,
        redo: bool,
        handle: &DesktopHandle,
        candidate: Candidate,
    ) -> bool {
        let Candidate {
            key,
            mut doc,
            mut log,
            revision,
        } = candidate;
        let before = doc.get_heads();
        let stepped = if redo {
            log.redo_as(&mut doc, Some(&user(person)), None, None)
        } else {
            log.undo_as(&mut doc, Some(&user(person)), None, None)
        };
        if stepped.is_none() {
            self.held(&key, |held| held.undo = Some(log));
            return false;
        }
        // What the step came to is what this device shows, as any other change of the workspace's
        // records is.
        let mut tree = handle.read(Clone::clone);
        let written = if key.1 == Route::layout() {
            let device = self.device.borrow().clone();
            let salt = tree.salt;
            if let Some(workspace) = tree
                .workspaces
                .iter_mut()
                .find(|held| held.id() == workspace_id)
            {
                let layout = shared_resource::read(&doc, None).unwrap_or_default();
                self.fold_into(workspace_id, &doc, &layout.tree, workspace, salt, &device);
            }
            handle.replace_tree(tree);
            let own = handle.read(|tree| tree.external_revision);
            let heads = doc.committed_heads();
            self.held(&key, |held| held.folded = Some(heads));
            Written::Layout(own, self.notes.of_workspace(workspace_id))
        } else {
            let view = PersonView::read(&doc);
            view.applied(&mut tree, workspace_id);
            handle.replace_tree(tree);
            Written::View(view)
        };
        self.held(&key, |held| held.undo = Some(log));
        self.keep(&key, doc.clone(), revision);
        self.publish(&key, doc, Some(written), &before, revision);
        true
    }

    /// Make `asked`'s arranging `action` of `tree`'s workspace in front one change of that
    /// workspace's layout, named by who asked and what they did: what a step of the person's is
    /// taken back from, and the windows it placed show the records they name.
    pub(crate) fn performed(
        &self,
        before: &Layout,
        tree: &Layout,
        asked: &tilo_actions::Actor,
        action: &tilo_session_core::layout::LayoutAction,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) {
        self.placed.borrow_mut().extend(shows.iter().cloned());
        let Some(certificate) = self.certificate() else {
            return;
        };
        let Some(person) = self.person.borrow().clone() else {
            return;
        };
        // What the person arranged of this workspace before the action is a step of its own, kept
        // under their own name and named before the action's: the action's step carries only what
        // the action changed, so taking it back never takes a window they opened earlier with it.
        if let Some((id, earlier)) = tree
            .active_workspace()
            .filter(|workspace| !workspace.id().is_nil())
            .and_then(|workspace| {
                let id = workspace.id();
                before
                    .workspaces
                    .iter()
                    .find(|held| held.id() == id)
                    .map(|held| (id, held))
            })
        {
            self.remember(&(id, Route::layout()), earlier, None, &person);
        }
        let message = tilo_session_core::layout::step_context(asked, action)
            .change_message()
            .unwrap_or_else(|| {
                shared_resource::change_message(
                    asked
                        .member_id()
                        .and_then(tilo_actions::MemberId::person_id),
                )
            });
        // What the action did to the person's own view of the workspace (a width, a window made
        // as large as its display) is a step of theirs in that view, written in its turn.
        if let Some(workspace) = tree.active_workspace()
            && !workspace.id().is_nil()
        {
            let id = workspace.id();
            let changed = PersonView::of(tree, id);
            let was = PersonView::of(before, id);
            if changed != was {
                let key = (id, Route::view(&person));
                self.held(&key, |held| {
                    held.pending_view.push(
                        changed,
                        Some(Acted {
                            message: message.clone(),
                            shows: Vec::new(),
                            was: Some(was),
                        }),
                    );
                });
            }
        }
        self.capture_layout(
            &certificate,
            &person,
            tree,
            tree.active,
            Some(Acted {
                message,
                shows: shows.to_vec(),
                was: None,
            }),
        );
        self.keep_pending();
    }

    /// A step of a layout the person asked for while that layout's write was under way: the
    /// workspace it belongs to and whether it is a redo. Taken once the write is done.
    pub(crate) fn take_pending_step(&self) -> Option<(WorkspaceId, bool)> {
        let mut pending = None;
        for (key, held) in self.held.borrow_mut().iter_mut() {
            if held.writing.is_some() {
                continue;
            }
            if let Some(redo) = held.pending_step.take() {
                pending = Some((key.0, redo));
                break;
            }
        }
        pending
    }

    /// Write what `edit` changes in `workspace_id`'s layout — what a change of it was, a name over
    /// a run of them — and publish it. `own` is the tree revision the layout stands at. Whether
    /// anything was written.
    pub(crate) fn edit(
        &self,
        workspace_id: WorkspaceId,
        person: &str,
        own: u64,
        edit: impl FnOnce(&mut automerge::AutoCommit) -> bool,
    ) -> bool {
        let Some(certificate) = self.certificate() else {
            return false;
        };
        let certificate = &certificate;
        let key = (workspace_id, Route::layout());
        if !self.settle_one(&key) {
            return false;
        }
        let Some((mut doc, revision)) = self.copy(&key) else {
            return false;
        };
        doc.set_actor(actor(certificate, person));
        let before = doc.get_heads();
        if !edit(&mut doc) {
            doc.rollback();
            return false;
        }
        if doc.pending_ops() == 0 {
            return false;
        }
        shared_resource::commit_change(&mut doc, person, crate::activity::now_seconds(), None);
        let heads = doc.committed_heads();
        let notes = self.notes.of_workspace(workspace_id);
        self.held(&key, |held| held.folded = Some(heads.clone()));
        self.keep(&key, doc.clone(), revision);
        self.publish(
            &key,
            doc,
            Some(Written::Layout(own, notes)),
            &before,
            revision,
        );
        true
    }

    /// Keep what a write of `key` that did not answer before this session ends held, for the
    /// session after: its steps are the person's, and the one after writes them.
    fn keep_for_next_session(&self, key: &Key, kept: Unanswered) {
        self.held(key, |held| match kept {
            Unanswered::Layout(steps) => held.pending_steps.restore(steps),
            Unanswered::View(view) => {
                if held.pending_view.last().map(|step| &step.changed) != Some(&view) {
                    held.pending_view.push(view, None);
                }
            }
        });
        self.keep_pending();
    }

    /// What this look writes of the person's view `view` of `key`: the changes made while it could
    /// not be written, in the order they were made, and then the view as it stands: written at
    /// once where an arranging action made the last of them, else once it rests.
    fn view_steps(&self, key: &Key, view: &PersonView, now: bool) -> Viewed {
        // What this device last wrote of this view: one that differs is the person's still.
        let unwritten = self.held(key, |held| {
            held.written.as_ref() != Some(&Written::View(view.clone()))
        });
        let keep = |looks: &Self| {
            if now && unwritten {
                // A session that ends before it can write its view keeps it for the next one, as
                // the layout's steps are, the person's own windows with it.
                looks.keep_for_next_session(key, Unanswered::View(view.clone()));
            }
        };
        if self.held(key, |held| held.writing.is_some()) {
            // The write under way carries what the person changed.
            return Viewed::Nothing;
        }
        if !self.held(key, |held| held.read) {
            // A view this device has not read is not written over.
            keep(self);
            return Viewed::Nothing;
        }
        let mut steps = self.held(key, |held| held.pending_view.take());
        match steps.last().map(|step| step.changed == *view) {
            Some(true) => {}
            Some(false) => steps.push(PendingStep {
                changed: view.clone(),
                acted: None,
            }),
            None => {
                let rested = self.held(key, |held| rested(held, view, now));
                let Some(rested) = rested else {
                    keep(self);
                    return Viewed::Nothing;
                };
                steps.push(PendingStep {
                    changed: rested,
                    acted: None,
                });
            }
        }
        Viewed::Steps(steps)
    }

    /// Write the person here's view of every workspace whose view they changed into that
    /// workspace's record of it.
    /// `now`: the view is written as it stands, without waiting for it to rest.
    fn capture_view(
        &self,
        certificate: &Arc<DeviceCert>,
        person: &str,
        tree: &Layout,
        now: bool,
    ) -> bool {
        let route = Route::view(person);
        let mut wrote = false;
        for workspace in &tree.workspaces {
            let workspace_id = workspace.id();
            if workspace_id.is_nil() {
                continue;
            }
            let key = (workspace_id, route.clone());
            // What the person changed here, over what the record holds. A session ending writes the
            // view as the person has it here: there is no look after this one to write what they
            // changed, and what only this device decides — the widths they chose and what they made
            // large — is theirs to keep.
            let here = PersonView::of(tree, workspace.id());
            let view = if now {
                here.clone()
            } else {
                self.held(&key, |held| match (&held.written, &held.seen) {
                    (Some(Written::View(read)), Some(seen)) => read.over(&here, Some(seen)),
                    _ => here.clone(),
                })
            };
            let Viewed::Steps(steps) = self.view_steps(&key, &view, now) else {
                continue;
            };
            let written = steps
                .last()
                .map_or_else(|| view.clone(), |step| step.changed.clone());
            self.held(&key, |held| {
                held.written = Some(Written::View(written.clone()));
                held.seen = Some(here);
                held.pending = None;
            });
            if self.waits(&key) {
                // The record has not moved since the workspace said the last write waits for it.
                self.held(&key, |held| {
                    held.written = None;
                    held.pending = None;
                    held.pending_view.restore(steps);
                });
                continue;
            }
            let Some((mut held, revision)) = self.copy(&key) else {
                // The view the workspace keeps cannot be read here yet: the steps wait for the look
                // that reads it.
                self.held(&key, |held| {
                    held.read = false;
                    held.written = None;
                    held.pending = None;
                    held.pending_view.restore(steps);
                });
                continue;
            };
            let from = held.get_heads();
            let mut log = self.held(&key, |kept| kept.undo.take());
            let mut refused = None;
            for step in &steps {
                if let Err(error) = view_step(&mut held, step, certificate, person, &mut log) {
                    refused = Some(error);
                    break;
                }
            }
            self.held(&key, |kept| {
                // A step whose change is dropped names nothing the workspace holds: the history is
                // made again from what it does hold.
                kept.undo = if refused.is_some() { None } else { log };
            });
            if let Some(error) = refused {
                self.say(
                    (RefusalReason::Other, &error.to_string()),
                    "the view of the layout was not kept",
                    workspace_id,
                );
                continue;
            }
            if held.pending_ops() == 0 && held.get_heads() == from {
                self.keep(&key, held, revision);
                continue;
            }
            wrote |= self.publish(&key, held, Some(Written::View(written)), &from, revision);
        }
        self.keep_pending();
        wrote
    }

    /// Publish `candidate`, a change of a record this device built on the loop — its view of a
    /// layout, a step taken from a layout's history, or what a model said a change was — as the
    /// workspace's own copy of it: what the workspace takes is what the next look writes on, and
    /// what it refuses is dropped, so the next look writes it again on what the workspace holds.
    /// The device's daemon writes it the way the workspace keeps its records, sealing it where
    /// they are its members' sealed channels: a write on no history founds the record whole.
    fn publish(
        &self,
        key: &Key,
        mut candidate: automerge::AutoCommit,
        replaced: Option<Written>,
        heads: &[ChangeHash],
        revision: Option<[u8; 32]>,
    ) -> bool {
        let (workspace_id, route) = key.clone();
        if replaced.is_none() {
            return false;
        }
        let Some(writes) = self.writes() else {
            tracing::debug!(
                %workspace_id,
                "a document of the layout has no service to write it"
            );
            self.unwritten(workspace_id, Some("its workspace service is not open yet"));
            return false;
        };
        // Every change a device signs says what it is and who made it: the edit of the record this
        // person just made here. A step taken from the history is its own change already.
        if candidate.pending_ops() > 0 {
            let author = self.person.borrow().clone();
            candidate.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(shared_resource::change_message(author.as_deref())),
            );
        }
        let changes = candidate.save_after(heads);
        // A write on no history is the whole record: the one that founds it.
        let founding = heads.is_empty();
        let (document, app) = (route.document.clone(), route.app);
        let workspace = workspace_id.to_string();
        let mut writing = crate::engine_write::Writing::write(self.wake.handle(), async move {
            writes
                .write(
                    &workspace,
                    &changes,
                    tilo_peer_state::workspace_documents::DocumentWriting {
                        app,
                        document: &document,
                        readers: None,
                        founding,
                        snapshot: founding,
                        sealed: None,
                    },
                )
                .await
        });
        if let Some(written) = writing.done() {
            self.published(key, written, candidate, replaced, revision);
            return false;
        }
        self.held(key, |held| {
            held.writing = Some(Going::View(Box::new(Publishing {
                writing,
                candidate,
                replaced,
                revision,
            })));
        });
        false
    }

    /// Take what the writes under way came to, once they are done; whether none is under way now.
    fn settle_publishing(&self) -> bool {
        self.keys().iter().all(|key| self.settle_one(key))
    }

    /// Take what the write of `key`'s record came to, once it is done; whether none is under way
    /// now. A change the person makes between two looks is not held back by a write that answered
    /// since the last one.
    fn settle_one(&self, key: &Key) -> bool {
        let Some(going) = self.held(key, |held| held.writing.take()) else {
            return true;
        };
        let going = match going {
            Going::View(mut publishing) => match publishing.writing.done() {
                Some(written) => {
                    let publishing = *publishing;
                    self.published(
                        key,
                        written,
                        publishing.candidate,
                        publishing.replaced,
                        publishing.revision,
                    );
                    None
                }
                None => Some(Going::View(publishing)),
            },
            Going::Layout(mut laying) => match laying.writing.done() {
                Some(laid) => {
                    self.laid(key, laid, &laying);
                    None
                }
                None => Some(Going::Layout(laying)),
            },
        };
        // A look skips a record while its write is under way, and another device's write to it may
        // have been told before that look: a finished write leaves its record
        // unread at any revision ([`Self::laid`], [`Self::published`]), and the next look reads it.
        let Some(going) = going else {
            return true;
        };
        self.held(key, |held| held.writing = Some(going));
        false
    }

    /// What the workspace did with `candidate`, this device's write of `key`'s record: what it took
    /// is what the next look writes on, and what it refused is dropped.
    fn published(
        &self,
        key: &Key,
        written: crate::engine_write::Written,
        candidate: automerge::AutoCommit,
        replaced: Option<Written>,
        revision: Option<[u8; 32]>,
    ) {
        let workspace_id = key.0;
        // Whatever reached the record while the write was under way is read on the next look.
        self.held(key, |held| held.read_at = None);
        match written {
            Ok(_write) => {
                let Some(written) = replaced else {
                    return;
                };
                tracing::debug!(%workspace_id, "the view's write was taken");
                self.held(key, |held| held.written = Some(written));
                self.keep(key, candidate, revision);
            }
            Err(WriteRefusal::Waiting(_)) => {
                // Nothing was refused: the workspace does not hold what the write builds on yet.
                // The view stays this device's, and is written once the record moves.
                self.held(key, |held| {
                    held.written = None;
                    held.pending = None;
                    held.waits = Some(Waits { at: held.revision });
                });
            }
            Err(error) => {
                // The record is not the workspace's until it takes it: the next write is made on
                // what the workspace holds, not on the write it refused. A history over the refused
                // copy names changes the workspace does not hold. The same view is not written
                // again: the workspace would refuse it again.
                self.held(key, |held| {
                    held.written = replaced;
                    held.kept = None;
                    held.based = None;
                    held.undo = None;
                });
                self.say(
                    (error.reason(), &error.to_string()),
                    "the workspace's layout was not published",
                    workspace_id,
                );
            }
        }
    }

    /// Why this look does not write `key`'s record: its last write is under way, or waits for the
    /// record to move.
    fn held_back(&self, key: &Key) -> Option<&'static str> {
        if !self.settle_one(key) {
            Some("its last write is under way")
        } else if self.waits(key) {
            Some("its last write waits for the record")
        } else {
            None
        }
    }

    /// Whether the last write of `key`'s record waits for changes the workspace does not hold yet,
    /// and the record has not moved since: a write made now would wait the same way.
    fn waits(&self, key: &Key) -> bool {
        self.held(key, |held| match held.waits {
            Some(Waits { at }) if at == held.revision => true,
            Some(_) => {
                held.waits = None;
                false
            }
            None => false,
        })
    }

    /// Say why a workspace's record was not kept or published, for the caller to tell the person:
    /// once per reason, not on every frame.
    fn say(&self, (reason, error): (RefusalReason, &str), what: &str, workspace_id: WorkspaceId) {
        tracing::warn!(%error, ?reason, %workspace_id, "{what}");
        if self.told.borrow().as_deref() != Some(error) {
            *self.told.borrow_mut() = Some(error.to_owned());
            *self.refused.borrow_mut() = Some((workspace_id, reason));
        }
    }

    /// The refusal the person has not heard yet, once: why what they changed on a workspace's
    /// layout did not reach the workspace.
    pub(crate) fn take_refusal(&self) -> Option<(WorkspaceId, RefusalReason)> {
        self.refused.borrow_mut().take()
    }

    /// What the workspace's copy of `key`'s record holds here, read against the revision this
    /// device's own copy holds, the revision it was read at, and whether this device holds a copy.
    /// A record this device was never sent, or cannot read, is one it holds nothing of, and one
    /// that did not move since its copy is the copy.
    fn reached(&self, key: &Key) -> Reached {
        let (holds, known) = self.held(key, |held| (held.kept.is_some(), held.based));
        let known = holds.then_some(known).flatten();
        let (read, waiting) = match self.document(key, known) {
            Ok(read) => (read, false),
            Err(NotRead::Waiting) => (None, true),
            Err(NotRead::Failed(_)) => (None, false),
        };
        let revision = read.as_ref().map(|(_, revision)| *revision);
        Reached {
            doc: read.map(|(doc, _)| doc),
            revision,
            holds,
            waiting,
        }
    }

    /// The record of `key` this device writes on ([`base_of`]), and the revision the workspace's
    /// copy is at. `None` while the workspace holds one this device cannot read yet and holds no
    /// copy of its own: a write then would stand an empty one over what the workspace keeps.
    fn copy(&self, key: &Key) -> Option<(automerge::AutoCommit, Option<[u8; 32]>)> {
        let reached = self.reached(key);
        let kept = self.held(key, |held| held.kept.clone());
        if reached.waiting && kept.is_none() {
            return None;
        }
        Some((base_of(key.0, kept, reached.doc), reached.revision))
    }

    /// Keep `record` as this device's copy of `key`'s record, read at `revision` when this look
    /// read it; one read against an unchanged revision keeps the revision it already had.
    fn keep(&self, key: &Key, record: automerge::AutoCommit, revision: Option<[u8; 32]>) {
        self.moved.set(self.moved.get().wrapping_add(1));
        self.held(key, |held| {
            held.kept = Some(record);
            if revision.is_some() {
                held.based = revision;
            }
        });
    }
}

/// Why a read of a layout or a view found nothing to lay out.
enum NotRead {
    /// The workspace keeps it, and its keys have not reached this device yet.
    Waiting,
    /// It could not be read.
    Failed(String),
}

/// Put the windows of the person's own that `shown` shows into `into`, a shell laid over it,
/// where `shown` has them: in the same group, else a group of their own in the same strip.
fn keep_personal(shown: &Workspace, into: &mut Workspace) {
    for strip in &shown.strips {
        for group in &strip.window_groups {
            let personal: Vec<_> = group
                .tabs
                .iter()
                .filter(|tab| tab.is_personal())
                .cloned()
                .collect();
            if personal.is_empty() {
                continue;
            }
            if let Some(held) = into
                .strips
                .iter_mut()
                .flat_map(|strip| &mut strip.window_groups)
                .find(|held| held.id == group.id)
            {
                held.tabs.extend(personal);
                continue;
            }
            let mut alone = group.clone();
            if let Some(first) = personal.first()
                && !personal.iter().any(|tab| tab.id == alone.active_tab)
            {
                alone.active_tab = first.id;
            }
            alone.tabs = personal;
            if let Some(held) = into.strips.iter_mut().find(|held| held.id == strip.id) {
                held.window_groups.push(alone);
            } else {
                let mut kept = strip.clone();
                kept.window_groups = vec![alone];
                into.strips.push(kept);
            }
        }
    }
}

/// What [`Layouts::reached`] found of the workspace's copy.
struct Reached {
    doc: Option<automerge::AutoCommit>,
    revision: Option<[u8; 32]>,
    /// Whether this device holds a copy of its own.
    holds: bool,
    /// Whether the workspace's copy is one this device cannot read yet.
    waiting: bool,
}

/// Stand the record's containers, `workspace` and its strips in `doc`, a record nobody wrote yet,
/// with no window, as one change of housekeeping: taking back the first window leaves the strip it
/// stood in and what another member put in the record by then.
fn found(
    doc: &mut automerge::AutoCommit,
    workspace_id: WorkspaceId,
    workspace: &Workspace,
    writer: &device::Writer<'_>,
) -> Result<(), automerge::AutomergeError> {
    let mut strips = workspace.clone();
    for strip in &mut strips.strips {
        strip.window_groups.clear();
    }
    shared_resource::found(doc)?;
    device::write(doc, workspace_id, &strips, writer)?;
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(shared_resource::housekeeping_message(
                writer.person.unwrap_or_default(),
            ))
            .with_time(crate::activity::now_seconds()),
    );
    Ok(())
}

/// This device's copy of a record: the one it last wrote and the workspace took (`kept`), with
/// everything the workspace holds since (`reached`) laid over it. A copy of the workspace's that
/// holds this device's write already is that copy.
fn base_of(
    workspace_id: WorkspaceId,
    kept: Option<automerge::AutoCommit>,
    reached: Option<automerge::AutoCommit>,
) -> automerge::AutoCommit {
    match (kept, reached) {
        (Some(kept), Some(reached))
            if kept
                .committed_heads()
                .iter()
                .all(|head| automerge::ReadDoc::has_change(&reached, head)) =>
        {
            reached
        }
        (Some(mut base), Some(reached)) => {
            match tilo_desktop_model::merge::merge_into(&mut base, &reached) {
                Ok(_applied) => base,
                Err(error) => {
                    // What the workspace holds cannot be laid over our own write: start from it.
                    tracing::warn!(%error, %workspace_id, "the record was not laid together");
                    reached
                }
            }
        }
        (Some(base), None) => base,
        (None, reached) => reached.unwrap_or_default(),
    }
}

/// `person`'s next step to take back (`redo` false) or bring back in a record `log` drives, as `doc`
/// holds it, and the gesture that orders it among their steps of the workspace's other records: a
/// later gesture is a later step.
fn next_step(
    log: &UndoLog,
    doc: &automerge::AutoCommit,
    person: &str,
    redo: bool,
) -> Option<(ui::state::UndoEntry, Option<uuid::Uuid>)> {
    use automerge::ReadDoc as _;
    let author = user(person);
    let entry = if redo {
        log.next_redo_as(doc, Some(&author))
    } else {
        log.next_undo_as(doc, Some(&author))
    }?;
    let at = entry
        .after
        .first()
        .and_then(|newest| doc.get_change_by_hash(newest))
        .and_then(|change| {
            tilo_actions::ActionContext::from_change_message(change.message()?)?.gesture
        })
        .map(|gesture| gesture.0);
    Some((entry, at))
}

/// What a person arranged of `workspace_id` in the layout `doc`: its strips, the window groups in
/// each and the windows in each group, and where the layout puts them.
pub(crate) type Arranged = (
    Vec<(u64, String, Vec<(u64, Vec<u64>)>)>,
    tilo_desktop_model::WorkspaceLayout,
);

pub(crate) fn arranged(doc: &automerge::AutoCommit, workspace_id: WorkspaceId) -> Option<Arranged> {
    let workspace = device::layout_of(doc, workspace_id)?;
    let strips = workspace
        .strips
        .iter()
        .map(|strip| {
            let groups = strip
                .window_groups
                .iter()
                .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                .collect();
            (strip.id.0, strip.name.clone(), groups)
        })
        .collect();
    Some((strips, workspace.layout))
}

/// What this device's write lays down: `workspace`, the layout as this device has it, with the
/// windows of `record`, the record it was laid out from, that it does not show yet added where the
/// record has them, and the windows it closed since its last write left out.
fn laid_into(
    record: &automerge::AutoCommit,
    workspace: Workspace,
    closed: &BTreeSet<u64>,
) -> Workspace {
    let mut laid_out = workspace;
    let held = windows_of(&laid_out);
    let Some(mut their_workspace) = device::layout_of(record, laid_out.id()) else {
        return laid_out;
    };
    // Only what this session does not show, and does not hold closed: everything else is where this
    // device put it.
    for strip in &mut their_workspace.strips {
        for group in &mut strip.window_groups {
            group
                .tabs
                .retain(|tab| !held.contains(&tab.id) && !closed.contains(&tab.id));
        }
        strip.window_groups.retain(|group| !group.tabs.is_empty());
    }
    their_workspace
        .strips
        .retain(|strip| !strip.window_groups.is_empty());
    for strip in their_workspace.strips {
        match laid_out.strips.iter_mut().find(|held| held.id == strip.id) {
            Some(ours) => {
                for group in strip.window_groups {
                    match ours
                        .window_groups
                        .iter_mut()
                        .find(|held| held.id == group.id)
                    {
                        Some(mine) => mine.tabs.extend(group.tabs),
                        None => ours.window_groups.push(group),
                    }
                }
            }
            None => laid_out.strips.push(strip),
        }
    }
    laid_out
}

/// `layout`, this device, as it publishes it into the workspace's layout. A window it minted
/// (under `local`) whose app says it runs only on the device that opened it is written as this
/// device's window, `host` being its key and what its certificate calls it: the others hold it as
/// that, and open no instance of the app for it.
fn hosted_here(
    layout: Workspace,
    host: (&str, &str),
    local: u16,
    runs_here: Option<fn(&str) -> bool>,
) -> Workspace {
    let bound = bound_apps(&layout, local, runs_here);
    device::published(
        layout,
        &device::Host {
            device: host.0,
            name: host.1,
            runs_here: &|app| bound.contains(app),
        },
        &|id| peer_salt_of(id) == local,
    )
}

/// The layout `laid_out` as this device writes it into the workspace's layout ([`hosted_here`]),
/// and the tabs of it that stand for a window running on another device. Such a window is that
/// device's to add to the record, whoever minted the tab it stands in here: the tab this session
/// pictures a shared window in, beside its own view of that window, is this session's alone.
fn written_here(
    laid_out: Workspace,
    certificate: &DeviceCert,
    local: u16,
    runs_here: Option<fn(&str) -> bool>,
) -> (Workspace, BTreeSet<u64>) {
    let device = hex::encode(certificate.device_public_key);
    let laid_out = hosted_here(
        laid_out,
        (&device, &certificate.device_label),
        local,
        runs_here,
    );
    let elsewhere = laid_out
        .tabs()
        .filter(|tab| {
            matches!(
                &tab.tile,
                TileContent::HostedWindow { host, .. } if !host.eq_ignore_ascii_case(&device)
            )
        })
        .map(|tab| tab.id)
        .collect();
    (laid_out, elsewhere)
}

/// The apps among the windows of `layout` this device minted (under `local`) whose windows run only
/// on the device that opened them, each asked of `runs_here` once: what an app declares is read
/// from its package.
fn bound_apps(
    layout: &Workspace,
    local: u16,
    runs_here: Option<fn(&str) -> bool>,
) -> BTreeSet<String> {
    let Some(runs_here) = runs_here else {
        return BTreeSet::new();
    };
    let apps: BTreeSet<&str> = layout
        .tabs()
        .filter(|tab| peer_salt_of(tab.id) == local)
        .filter_map(|tab| match &tab.tile {
            TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
                Some(app_id.as_str())
            }
            _ => None,
        })
        .collect();
    apps.into_iter()
        .filter(|app| runs_here(app))
        .map(str::to_owned)
        .collect()
}

/// Whether this device's service holds `workspace_id` and it holds nothing of `route` yet: what a
/// view is written onto, rather than over one this look cannot read. A workspace the service does
/// not hold is one the session has no documents of at all, never one it may write a view over.
impl Layouts {
    fn nothing_written(&self, workspace_id: WorkspaceId, route: &Route) -> bool {
        self.reads().is_some_and(|reads| {
            reads
                .workspaces()
                .iter()
                .any(|held| held == &workspace_id.to_string())
                && !reads.holds(&workspace_id.to_string(), route.app, &route.document)
        })
    }

    /// Whether this device's service holds anything of the layout of `workspace_id`: the
    /// workspace's own copy that a session which has not read it yet is not written over.
    fn holds_layout(&self, workspace_id: WorkspaceId) -> bool {
        let route = Route::layout();
        self.reads()
            .is_some_and(|reads| reads.holds(&workspace_id.to_string(), route.app, &route.document))
    }
}

/// Write `step`, a change this device made of the person's view, into `doc` as one change, and
/// close it as a step of theirs where an arranging action named it. Where the action changed the
/// view, the view as it stood before it is committed first, so taking the step back takes back
/// the action and nothing else. Whether anything was written.
fn view_step(
    doc: &mut automerge::AutoCommit,
    step: &PendingStep<PersonView>,
    certificate: &DeviceCert,
    person: &str,
    log: &mut Option<UndoLog>,
) -> Result<bool, automerge::AutomergeError> {
    // A write of the view is this device's, never one of its other windows: every change it makes
    // numbers its own.
    doc.set_actor(automerge::ActorId::random());
    if let Some(was) = step.acted.as_ref().and_then(|acted| acted.was.as_ref())
        && PersonView::read(doc) != *was
        && was.write(doc)?
    {
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(shared_resource::change_message(Some(person))),
        );
    }
    // An arranging action's step is the person's, on whichever of their devices made it.
    if step.acted.is_some() {
        doc.set_actor(actor(certificate, person));
    }
    let before = doc.get_heads();
    if !step.changed.write(doc)? {
        return Ok(false);
    }
    let message = step.acted.as_ref().map_or_else(
        || shared_resource::change_message(Some(person)),
        |acted| acted.message.clone(),
    );
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(message)
            .with_time(crate::activity::now_seconds()),
    );
    if step.acted.is_some() {
        let after = doc.get_heads();
        let log = log.get_or_insert_with(|| {
            let mut from = doc.clone();
            history(&mut from)
        });
        log.record_committed_span(doc, before, after, None, None, Some(user(person)), None);
    }
    Ok(true)
}
/// The view to write into `key` now, if any: what a look writes only once it rests, and never
/// while a write of it is under way or before this device has read it. A view that did not change
/// is not written again.
fn rested(held: &mut Held, view: &PersonView, now: bool) -> Option<PersonView> {
    // A view this device has not read here is written on the look after it reads it: it writes
    // what the person changed, never a layout still being opened. One it could not read is
    // written once it can.
    if !held.read || held.unreadable || held.writing.is_some() {
        return None;
    }
    if held.written.as_ref() == Some(&Written::View(view.clone())) {
        held.pending = None;
        return None;
    }
    match held.pending.replace(view.clone()) {
        Some(pending) if pending == *view => Some(view.clone()),
        _ if now => Some(view.clone()),
        _ => None,
    }
}

/// Every window `layout`, a record's layout, holds.
fn tabs_of(layout: &Layout) -> BTreeSet<u64> {
    layout.workspaces.iter().flat_map(windows_of).collect()
}

/// Every window the layout of `workspace` shows.
fn windows_of(workspace: &Workspace) -> BTreeSet<u64> {
    workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.id)
        .collect()
}

/// The workspaces' layouts, as the pane reads the layout's own history: the record of each, and the
/// model's word for what its changes were.
pub(crate) struct LayoutHistory {
    layouts: Rc<Layouts>,
    person: String,
}

impl LayoutHistory {
    pub(crate) fn new(layouts: Rc<Layouts>, person: &str) -> Self {
        Self {
            layouts,
            person: person.to_owned(),
        }
    }

    fn workspace_of(tree: &Layout, workspace_id: WorkspaceId) -> Option<WorkspaceId> {
        (!workspace_id.is_nil() && tree.workspaces.iter().any(|held| held.id() == workspace_id))
            .then_some(workspace_id)
    }
}

impl crate::activity::History for LayoutHistory {
    fn history(
        &self,
        tree: &Layout,
        workspace_id: WorkspaceId,
    ) -> Option<tilo_session_core::desktop::activity::ActivitySnapshot> {
        Self::workspace_of(tree, workspace_id)?;
        // A workspace with no layout yet still has a history: what its devices answered.
        let snapshot = tilo_session_core::desktop::activity::ActivitySnapshot::of_layout(
            tree.clone(),
            workspace_id,
            self.layouts.layout(workspace_id).unwrap_or_default(),
        );
        Some(match self.layouts.governance(workspace_id) {
            Some(document) => snapshot
                .governing(document)
                .reading_governance(std::rc::Rc::new(crate::activity::GovernanceValues)),
            None => snapshot,
        })
    }

    fn documents(&self, tree: &Layout) -> Vec<(WorkspaceId, automerge::AutoCommit)> {
        tree.workspaces
            .iter()
            .filter_map(|workspace| Some((workspace.id(), self.layouts.layout(workspace.id())?)))
            .collect()
    }

    fn describe(
        &self,
        tree: &Layout,
        workspace_id: WorkspaceId,
        write: &mut dyn FnMut(&mut automerge::AutoCommit) -> bool,
    ) -> bool {
        if Self::workspace_of(tree, workspace_id).is_none() {
            return false;
        }
        self.layouts
            .edit(workspace_id, &self.person, tree.external_revision, |doc| {
                write(doc)
            })
    }

    fn moved(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.layouts.moved().hash(&mut hasher);
        self.layouts.owner_revision().hash(&mut hasher);
        hasher.finish()
    }

    fn attach(
        &self,
        certificate: &DeviceCert,
        view: &Arc<WorkspaceView>,
        reads: &Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        writes: &Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    ) {
        self.layouts.attach(certificate, view, reads, writes);
    }
}

/// The layout of the workspace in front, which every
/// member shows.
pub(crate) struct Steps {
    layouts: Rc<Layouts>,
    handle: DesktopHandle,
}

impl Steps {
    pub(crate) fn new(layouts: Rc<Layouts>, handle: DesktopHandle) -> Self {
        Self { layouts, handle }
    }

    fn workspace_id(&self) -> Option<WorkspaceId> {
        let workspace_id = self
            .handle
            .read(|tree| tree.active_workspace().map(Workspace::id))?;
        (!workspace_id.is_nil()).then_some(workspace_id)
    }
}

impl tilo_session_core::layout::Layouts for Steps {
    fn step(&self, person: &str, redo: bool) -> tilo_session_core::layout::Stepped {
        use tilo_session_core::layout::Stepped;
        let Some(workspace_id) = self.workspace_id() else {
            return Stepped::None;
        };
        if self.layouts.step(workspace_id, person, redo, &self.handle) {
            Stepped::Taken
        } else {
            Stepped::None
        }
    }

    fn last_step(&self, person: &str) -> Option<(tilo_actions::Actor, String)> {
        self.layouts.last_step(self.workspace_id()?, person)
    }

    fn acted(
        &self,
        before: &Layout,
        tree: &Layout,
        asked: &tilo_actions::Actor,
        action: &tilo_session_core::layout::LayoutAction,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) {
        self.layouts.performed(before, tree, asked, action, shows);
    }

    fn shown(&self) -> std::collections::BTreeMap<tilo_desktop_model::TabId, String> {
        let workspaces: Vec<WorkspaceId> =
            (self.handle).read(|tree| tree.workspaces.iter().map(Workspace::id).collect());
        let mut shown: BTreeMap<u64, String> = workspaces
            .into_iter()
            .filter_map(|workspace_id| self.layouts.layout(workspace_id))
            .filter_map(|doc| shared_resource::read(&doc, None))
            .flat_map(|layout| layout.records)
            .collect();
        for (tab, record) in self.layouts.placed.borrow().iter() {
            shown.entry(*tab).or_insert_with(|| record.clone());
        }
        shown
    }
}

mod reads;

#[cfg(test)]
mod tests;
