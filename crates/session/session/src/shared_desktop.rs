//! A workspace's layout: its strips, groups and windows, which every member shows and changes
//! ([`tilo_desktop_model::shared_record`]), and each person's view of it, where they are looking
//! and how wide their windows are ([`tilo_desktop_model::person_view`]).
//! The layout is the workspace's, so it is where the live desk is kept: the tree this session draws
//! is what the layout and the person's view say, and every change a window makes is written there
//! ([`Self::capture`]) and read back from it ([`Self::follow`]). Undo and redo of a layout change
//! are taken from its own history ([`Self::step`]).
//! Both documents are read from the engine's copy of the workspace, which holds what the workspace
//! admitted and what it held before this device joined. Nothing here decides whether a write may
//! land: that is the workspace's, decided where the engine takes it in. A record that did not move
//! since the last look is neither read nor written again, and what is written is published off the
//! loop ([`crate::engine_write`]).

// Nothing compiled for the web reads this module's roles yet: the page's session wires them with
// its own host.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page's session reads these with its own wiring"
    )
)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use automerge::ChangeHash;
use automerge::transaction::Transactable as _;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, OpenedBy};
use tilo_desktop_model::person_view::{self, PersonView};
use tilo_desktop_model::shared_record::{self, device};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot, peer_salt_of};
use ui::permissions::UserId;
use ui::state::UndoLog;

use tilo_broker_core::custody::OwnerCustody as _;

use crate::engine::{Engine, SealKeys};
use tilo_desktop_surface::DesktopHandle;

/// The person `person`, as a history names them.
fn user(person: &str) -> UserId {
    UserId(person.to_owned())
}

/// A layout's history as this session drives it: every change of it a person made, on any of
/// their devices, is one step of theirs.
fn history(doc: &mut automerge::AutoCommit) -> UndoLog {
    UndoLog::new(doc).on_any_device()
}

/// The actor `person`'s changes of a layout are written as on this device: their handle and this
/// device's key, so two of their devices writing at once never number a change the same
/// (`ui::history_source::authors::device_actor`).
fn actor(engine: &Engine, person: &str) -> automerge::ActorId {
    automerge::ActorId::from(ui::history_source::authors::device_actor(
        person,
        &automerge::ActorId::from(engine.identity().device_cert.device_public_key.to_vec()),
    ))
}

/// A record of a workspace: the app it belongs to and its name there.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Route {
    app: &'static str,
    record: String,
}

impl Route {
    /// The workspace's layout.
    fn layout() -> Self {
        Self {
            app: DESKTOP_APP,
            record: DESKTOP_RECORD.to_owned(),
        }
    }

    /// `person`'s view of the workspace's layout.
    fn view(person: &str) -> Self {
        Self {
            app: DESKTOP_APP,
            record: person_view::record_of(person),
        }
    }
}

/// A record of a workspace this device reads and writes.
type Key = (WorkspaceId, Route);

/// What a record held when this device last wrote it, as its route compares it: a look that finds
/// it again writes nothing.
#[derive(Clone, PartialEq, Eq)]
enum Written {
    /// The layout's heads, the desk revision it was written from, and the record each window showed.
    Layout(Vec<ChangeHash>, u64, Vec<(u64, String)>),
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
    /// made on. The engine's copy reaches a member after the workspace admits the write, so it can
    /// lag the write this device already made; writing on that lagging copy would number a second
    /// change the same as the first, and the workspace would refuse it.
    kept: Option<automerge::AutoCommit>,
    /// The revision `kept` holds.
    based: Option<[u8; 32]>,
    /// What the record held when this device last wrote it.
    written: Option<Written>,
    /// The view that changed since the last write, and did not move on: written on the look after
    /// the one that found it, so a drag or a burst of switches is one write when it is over.
    pending: Option<PersonView>,
    /// The layout's history, as this session drives it: each change this device made of it is one
    /// step of the person's, and the next undo takes back all of the last one.
    undo: Option<UndoLog>,
    /// The write of the record under way.
    writing: Option<Going>,
    /// A step of the person's the arrangement asked for while the layout's write was under way: it
    /// is taken on the look that finds that write done.
    pending_step: Option<bool>,
    /// What an arranging action did while the layout's write was under way: the change that names
    /// it is the look's, once the workspace answered the one before it.
    pending_acted: Option<Acted>,
    /// Whether the record was read here.
    read: bool,
}

/// A record a step may be taken in: the record as this device writes it, its history, the
/// revision it was read at and how a write of it is sealed.
struct Candidate {
    key: Key,
    doc: automerge::AutoCommit,
    log: UndoLog,
    revision: Option<[u8; 32]>,
    sealed: Sealing,
}

/// What an arranging action did to a layout: the change it makes names who asked and what they did,
/// and the windows it placed show the records they name.
struct Acted {
    message: String,
    shows: Vec<(tilo_desktop_model::TabId, String)>,
    /// The person's view of the workspace before the action, where it changed their view: the
    /// step takes the view back to it.
    was: Option<PersonView>,
}

/// A write of a record under way. Boxed: each holds a document, and the state beside it is small.
enum Going {
    /// The view's, a step's or an edit's: made here, and published off the loop.
    View(Box<Publishing>),
    /// The layout's: made and written off the loop, since folding the record into this desk and
    /// writing the two together is what a save of it costs.
    Layout(Box<Laying>),
}

/// What a record's write is sealed with, where this device can write it.
enum Sealing {
    /// Its workspace commits the documents of its records itself: the write goes in the clear.
    Clear,
    /// Its members' sealed channels: the write is sealed with their keys.
    Sealed(channel_crypto::ChannelContentKeys),
}

impl Sealing {
    /// The record channel's keys, where the write is sealed.
    fn keys(&self) -> Option<&channel_crypto::ChannelContentKeys> {
        match self {
            Self::Clear => None,
            Self::Sealed(keys) => Some(keys),
        }
    }
}

/// A write of a record under way, with what [`SharedDesktops::published`] takes from it.
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
}

/// What a layout's write came to, off the loop.
enum Laid {
    /// The record held this desk already: nothing written.
    Held {
        record: Box<automerge::AutoCommit>,
        /// The windows of this device's desk the record holds.
        windows: BTreeSet<u64>,
    },
    /// What the write came to.
    Wrote(Box<Taken>),
    /// The write was not made and why: folding the record into this desk, or writing them together,
    /// failed. The record to keep as this device's copy, where it still has one.
    Failed(String, Option<Box<automerge::AutoCommit>>),
}

/// The record this device made, the windows of its desk that write holds, the heads it was made on,
/// and what the workspace did with it.
struct Taken {
    record: Box<automerge::AutoCommit>,
    windows: BTreeSet<u64>,
    before: Vec<ChangeHash>,
    /// Whether the change is the person's step: what they arranged, not what the record notes of
    /// a window (its title, what it shows).
    step: bool,
    took: crate::engine_write::Written,
}

/// What a look hands to the task that writes the layout: this device's desk, and what it read of the
/// record.
struct Save {
    workspace_id: WorkspaceId,
    /// The workspace in front, as this desk shows it.
    workspace: Workspace,
    /// The salt this device mints ids in.
    local: u16,
    /// The record as this device last wrote it and the workspace took it, and the workspace's own
    /// copy of it since.
    kept: Option<automerge::AutoCommit>,
    reached: Option<automerge::AutoCommit>,
    /// What the record held when this device last wrote it.
    written: Option<Written>,
    /// The tree revision and the record each window showed, the arranging action's included.
    own: u64,
    notes: Vec<(u64, String)>,
    sealed: Sealing,
    /// The arranging action this write makes, where the desk changed because of one.
    acted: Option<Acted>,
}

impl Save {
    /// Write it: the record this device writes on, with the record laid into this desk's workspace
    /// and this device's own change on top, and the write of that to the workspace.
    #[cfg_attr(
        target_family = "wasm",
        expect(
            clippy::future_not_send,
            reason = "a page's engine holds its one thread's handles; the write runs on that thread"
        )
    )]
    async fn write(self, engine: &Engine, person: &str, closed: &BTreeSet<u64>) -> Laid {
        let Self {
            workspace_id,
            workspace,
            local,
            kept,
            reached,
            written,
            own,
            notes,
            sealed,
            acted,
        } = self;
        let here = windows_of(&workspace);
        let base = base_of(workspace_id, kept, reached);
        let laid_out = laid_into(&base, workspace, closed);
        let windows = &windows_of(&laid_out) & &here;
        let heads = base.committed_heads();
        // An arranging action's change is written whatever the revision says: it is the write that
        // names what was done, and the record's own answer is what says it held this desk already.
        if acted.is_none() && written == Some(Written::Layout(heads.clone(), own, notes.clone())) {
            // What the record holds of this desk: a write it refused holds none of it.
            let windows = shared_record::read(&base, None)
                .and_then(|desk| {
                    desk.tree
                        .workspaces
                        .into_iter()
                        .find(|held| held.id() == workspace_id)
                })
                .map(|held| &windows_of(&held) & &here)
                .unwrap_or_default();
            return Laid::Held {
                record: Box::new(base),
                windows,
            };
        }
        // A change of a layout is the person's, written as this device: the record is every
        // member's, and another of their devices writing at the same instant must not number the
        // same change.
        let was = arranged(&base, workspace_id);
        let mut candidate = base;
        candidate.set_actor(actor(engine, person));
        let mine = |id: u64| peer_salt_of(id) == local;
        let by_tab: BTreeMap<u64, String> = notes.into_iter().collect();
        let records = |id: u64| by_tab.get(&id).cloned();
        match device::write(
            &mut candidate,
            workspace_id,
            &laid_out,
            &device::Writer {
                person: Some(person),
                mine: &mine,
                retained: &|_: u64| false,
                carried: device::Writer::NOTHING_CARRIED,
                records: &records,
            },
        ) {
            Err(error) => {
                candidate.rollback();
                return Laid::Failed(error.to_string(), Some(Box::new(candidate)));
            }
            Ok(false) => {
                return Laid::Held {
                    record: Box::new(candidate),
                    windows,
                };
            }
            Ok(true) => {}
        }
        // Every change a device signs says what it is and who made it: what was done here, as the
        // arranging action that made it names it. A change that arranged nothing (a window's
        // title, what it shows) is housekeeping, nobody's step to take back.
        let step = acted.is_some() || arranged(&candidate, workspace_id) != was;
        let message = match acted {
            Some(acted) => acted.message,
            None if step => shared_record::step_message(person),
            None => shared_record::housekeeping_message(person),
        };
        candidate.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(message)
                .with_time(crate::activity::now_seconds()),
        );
        let changes = candidate.save_after(&heads);
        // A write on no history is the whole record: the one that founds it.
        let founding = heads.is_empty();
        let took = engine
            .write_record(
                workspace_id,
                (DESKTOP_APP, DESKTOP_RECORD),
                &changes,
                founding,
                sealed.keys(),
            )
            .await;
        Laid::Wrote(Box::new(Taken {
            record: Box::new(candidate),
            windows,
            before: heads,
            step,
            took,
        }))
    }
}

/// This session's reading of the workspaces' records: the layout of each, and the view of it the
/// person here has.
#[derive(Default)]
pub(crate) struct SharedDesktops {
    /// What each of this device's windows shows, as its app reports it: the record noted beside the
    /// window in the workspace's layout, so the person's other devices hold it and can continue the
    /// window there.
    notes: Arc<crate::continuations::Continuations>,
    /// The agent run that opened each window an agent opened, by workspace and tab, as each
    /// workspace's layout last said.
    opened_by: RefCell<HashMap<WorkspaceId, BTreeMap<u64, OpenedBy>>>,
    /// The windows each workspace's layout holds of this device's desk, as it last wrote them: a
    /// window there that this desk no longer holds is one this device closed, and its entry goes
    /// when the device writes again rather than the fold laying it back into the desk.
    written_windows: RefCell<HashMap<WorkspaceId, BTreeSet<u64>>>,
    /// The record each window an arranging action put on a record shows, as this device placed it:
    /// what the place shows from the moment it stands, before the layout's write of it lands.
    placed: RefCell<BTreeMap<u64, String>>,
    /// The windows each workspace's layout held when this desk was last laid out from it: a window
    /// of this device's that is not among them is one it opened since, which the layout has not
    /// taken in yet.
    recorded: RefCell<HashMap<WorkspaceId, BTreeSet<u64>>>,
    /// The engine's revision and this desk's own at the last look.
    seen: Cell<Option<(u64, u64)>>,
    /// A failure to read or write a record that the person has not heard yet.
    refused: RefCell<Option<(WorkspaceId, String)>>,
    /// The last failure the person heard: the same one again is not said twice.
    told: RefCell<Option<String>>,
    /// The session's loop, which a write under way wakes as it moves on.
    wake: tilo_service::LoopWake,
    /// This device's publisher, as the fold of a layout resolves the windows that run here.
    device: RefCell<String>,
    /// The engine this session keeps its workspaces in, once it opened them.
    engine: RefCell<Option<Arc<Engine>>>,
    /// The workspace's own document as this device holds it, and the engine revision it was read
    /// at: its governance is read from there, never from a device's copy of it.
    governing: RefCell<HashMap<WorkspaceId, (u64, Option<automerge::AutoCommit>)>>,
    /// Moves whenever a layout is read or written here: what a history open on this device is
    /// served again for.
    moved: Cell<u64>,
    /// The person at this device, whose view of each workspace is theirs.
    person: RefCell<Option<String>>,
    /// Every record of every workspace this device reads and writes.
    held: RefCell<HashMap<Key, Held>>,
}

impl SharedDesktops {
    /// This session's records, writing what its windows note of themselves (`notes`), on the loop
    /// `wake` wakes, and each workspace's view kept for `person`.
    pub(crate) fn new(
        notes: Arc<crate::continuations::Continuations>,
        wake: tilo_service::LoopWake,
        person: String,
    ) -> Self {
        Self {
            notes,
            wake,
            person: RefCell::new(Some(person)),
            ..Self::default()
        }
    }

    /// What this device holds of `key`, changed by `edit`.
    fn held<R>(&self, key: &Key, edit: impl FnOnce(&mut Held) -> R) -> R {
        edit(self.held.borrow_mut().entry(key.clone()).or_default())
    }

    /// Every key this device holds a record of.
    fn keys(&self) -> Vec<Key> {
        self.held.borrow().keys().cloned().collect()
    }

    /// The windows this device's desk has that the layout of `workspace_id` holds and this desk no
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

    /// Whether every workspace `tree` holds has had its records read here, and neither the engine at
    /// revision `revision` nor this desk has moved since: nothing of them can have changed, so no
    /// record is read or written again. Notes what was looked at.
    pub(crate) fn settled(&self, revision: u64, tree: &DesktopTree) -> bool {
        // A write under way is taken first, and while it is no record is read or written.
        if !self.settle_publishing() {
            return true;
        }
        let person = self.person.borrow().clone();
        let settled = self.held.borrow().values().all(|held| {
            held.pending.is_none() && held.pending_step.is_none() && held.pending_acted.is_none()
        }) && self.seen.get() == Some((revision, tree.external_revision))
            && tree
                .workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil())
                .all(|workspace| {
                    let workspace_id = workspace.id();
                    self.held(&(workspace_id, Route::layout()), |held| held.read)
                        && person.as_deref().is_none_or(|person| {
                            self.held(&(workspace_id, Route::view(person)), |held| held.read)
                        })
                });
        self.seen.set(Some((revision, tree.external_revision)));
        settled
    }

    /// `key`'s record, read from the engine's copy of the workspace: what the workspace last
    /// admitted, what it held before this device joined included. `Ok(None)` for a record no one
    /// wrote yet.
    fn record(
        engine: &Engine,
        key: &Key,
        known: Option<[u8; 32]>,
    ) -> Result<Option<(automerge::AutoCommit, [u8; 32])>, String> {
        let (workspace_id, route) = key;
        let Some((bytes, revision)) =
            engine.record_since(&workspace_id.to_string(), route.app, &route.record, known)?
        else {
            return Ok(None);
        };
        automerge::AutoCommit::load(&bytes)
            .map(|doc| Some((doc, revision)))
            .map_err(|error| error.to_string())
    }

    /// Lay every workspace's layout into `tree`, and each person's view of it: what the records say
    /// where, and what this device decides alone kept. `device` is this device's publisher, which
    /// resolves a window that runs here to the window it shows here. Returns whether anything
    /// changed.
    pub(crate) fn follow(&self, engine: &Engine, device: &str, tree: &mut DesktopTree) -> bool {
        let changed = self.follow_layout(engine, device, tree) | self.follow_view(engine, tree);
        if self.device.borrow().as_str() != device {
            device.clone_into(&mut self.device.borrow_mut());
        }
        changed
    }

    /// Write what this device changed of `tree` and wait for it, for a session that is ending: a
    /// write under way would otherwise be cut with its task, and a change made since the last look
    /// — or held for the write before it — would not reach the workspace at all.
    pub(crate) fn finish_writes(
        &self,
        engine: &Arc<Engine>,
        tree: &DesktopTree,
        patience: std::time::Duration,
    ) {
        self.wait_writes(patience);
        if let Some(person) = self.person.borrow().clone() {
            self.capture_layout(engine, &person, tree, None);
            self.capture_view(engine, &person, tree, true);
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

    /// Read and write the workspaces' layouts through `engine` from now on.
    pub(crate) fn attach_engine(&self, engine: &Arc<Engine>) {
        *self.engine.borrow_mut() = Some(Arc::clone(engine));
    }

    /// The engine this session keeps its workspaces in, once it opened them.
    fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.borrow().clone()
    }

    /// The layout of `workspace_id`, as this device last read it.
    pub(crate) fn layout(&self, workspace_id: WorkspaceId) -> Option<automerge::AutoCommit> {
        self.held(&(workspace_id, Route::layout()), |held| held.kept.clone())
    }

    /// The workspace's own document as this device holds it: its governance, and the layout it
    /// keeps at its root. `None` until the workspace is held here.
    pub(crate) fn governance(&self, workspace_id: WorkspaceId) -> Option<automerge::AutoCommit> {
        let engine = self.engine()?;
        let revision = engine.owner().revision();
        let mut governing = self.governing.borrow_mut();
        let (read_at, document) = governing.entry(workspace_id).or_insert((revision, None));
        if *read_at != revision {
            *document = engine
                .workspace_document(&workspace_id.to_string())
                .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok());
            *read_at = revision;
        }
        document.clone()
    }

    /// A number of this device's workspaces as they stand, governance included: what a history
    /// open here is served again for.
    pub(crate) fn owner_revision(&self) -> u64 {
        self.engine().map_or(0, |engine| engine.owner().revision())
    }

    /// Fold the layout of every workspace in the engine's copy into `tree`.
    fn follow_layout(&self, engine: &Engine, device: &str, tree: &mut DesktopTree) -> bool {
        let salt = tree.salt;
        let mut changed = false;
        for workspace in &mut tree.workspaces {
            let workspace_id = workspace.id();
            if workspace_id.is_nil() {
                continue;
            }
            let key = (workspace_id, Route::layout());
            // A write of this layout under way holds what the person just changed: the record has
            // not taken it yet, and folding the record over the desk would put it back. A write
            // that answered is taken first, so the next one never starts before the record is read.
            if !self.settle_one(&key) {
                continue;
            }
            let known = self.held(&key, |held| held.revision);
            let Some((doc, revision)) = Self::record(engine, &key, known).ok().flatten() else {
                // Read against a revision, nothing comes back while the record did not move: what
                // was read stands. A workspace this device holds no record of (not its own, or one
                // still arriving) says nothing of its layout yet: its record is read again next
                // pass.
                if known.is_none() {
                    self.held(&key, |held| held.read = false);
                }
                continue;
            };
            // The workspace's copy reaches this device after the workspace admitted this device's
            // own write, so it can lack it: what is shown is the workspace's copy with that write
            // in it, or the desk would take back what this device just did.
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
            let desk = shared_record::read(&doc, None).unwrap_or_default();
            self.opened_by
                .borrow_mut()
                .insert(workspace_id, desk.opened_by);
            changed |= self.fold_into(workspace_id, &doc, &desk.tree, workspace, salt, device);
        }
        changed
    }

    /// Show the layout `desk`, what the record `doc` reads as, in `workspace`, keeping what this
    /// device decides alone: a strip or a window this device made that the layout does not hold
    /// yet. Whether anything changed.
    fn fold_into(
        &self,
        workspace_id: WorkspaceId,
        doc: &automerge::AutoCommit,
        desk: &DesktopTree,
        workspace: &mut Workspace,
        salt: u16,
        device: &str,
    ) -> bool {
        let mine = |id: u64| salt != 0 && peer_salt_of(id) == salt;
        let closed = self.closed_since_written(workspace_id, workspace);
        let recorded = self
            .recorded
            .borrow_mut()
            .insert(workspace_id, tabs_of(desk));
        let written = self
            .written_windows
            .borrow()
            .get(&workspace_id)
            .cloned()
            .unwrap_or_default();
        // The desk opens empty and takes its windows from the records, so a window of this
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
            // A strip this device made and left empty stays on its own desk: the record never
            // holds an empty strip it made, and an empty row arriving here is nobody else's to
            // see.
            mine: &mine,
            // A window this device opened that the record does not hold yet stays: another
            // member's change that arrives before this device's own write comes back is no
            // close of it.
            private: &opened_here,
            // A window this device closed is not laid back into its desk from the record: the
            // write that follows takes its entry out instead.
            closed: &|id| closed.contains(&id),
            writing: false,
        };
        device::fold(doc, workspace, device, &kept)
    }

    /// Show every workspace in `tree` as the person here last left it.
    fn follow_view(&self, engine: &Engine, tree: &mut DesktopTree) -> bool {
        let Some(person) = self.person.borrow().clone() else {
            return false;
        };
        let route = Route::view(&person);
        let slots: Vec<WorkspaceSlot> = tree
            .workspaces
            .iter()
            .map(|workspace| workspace.slot)
            .collect();
        let mut changed = false;
        for slot in slots {
            let Some(workspace_id) = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.slot == slot)
                .map(Workspace::id)
            else {
                continue;
            };
            if workspace_id.is_nil() {
                continue;
            }
            let key = (workspace_id, route.clone());
            // A write of this view under way holds what the person just changed: the record has not
            // taken it yet, and laying the record over the desk would put it back.
            if self.held(&key, |held| held.writing.is_some()) {
                continue;
            }
            let known = self.held(&key, |held| held.revision);
            let Some((doc, revision)) = Self::record(engine, &key, known).ok().flatten() else {
                // Nothing written of this view here yet: the desk shows the person's view as it is.
                self.held(&key, |held| {
                    held.read = true;
                    if held.written.is_none() {
                        held.written = Some(Written::View(PersonView::of(tree, slot)));
                    }
                });
                continue;
            };
            let doc = base_of(
                workspace_id,
                self.held(&key, |held| held.kept.clone()),
                Some(doc),
            );
            let view = PersonView::read(&doc);
            // What this device wrote or read last is on the desk already, or changed since by the
            // person here: only a view another of their devices wrote is news.
            let news = self.held(&key, |held| {
                held.written.as_ref() != Some(&Written::View(view.clone()))
            });
            self.held(&key, |held| {
                held.read = true;
                held.revision = Some(revision);
                held.written = Some(Written::View(view.clone()));
                if held.undo.is_none() {
                    let mut from = doc.clone();
                    held.undo = Some(history(&mut from));
                }
                held.kept = Some(doc);
                held.based = Some(revision);
            });
            if news {
                changed |= view.applied(tree, slot);
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

    /// Write what this device changed about the layout of the workspace in front into that
    /// workspace's layout, and the person's view of every workspace into theirs, and publish them:
    /// the strips and windows it makes and arranges, and where the person here is looking. Returns
    /// whether this look published the view.
    pub(crate) fn capture(&self, engine: &Arc<Engine>, tree: &DesktopTree) -> bool {
        let Some(person) = self.person.borrow().clone() else {
            return false;
        };
        self.capture_layout(engine, &person, tree, None);
        self.capture_view(engine, &person, tree, false)
    }

    /// Write this device's changes to the workspace in front's layout into the workspace's record,
    /// as one change: `acted`'s if the arrangement just did something, else the person's own edit.
    /// Handing them to the task that writes them is all the loop does: folding the record into this
    /// desk's workspace and writing the two together is what a save costs, and none of it runs here.
    fn capture_layout(
        &self,
        engine: &Arc<Engine>,
        person: &str,
        tree: &DesktopTree,
        acted: Option<Acted>,
    ) {
        let Some(workspace) = tree.workspaces.iter().find(|held| held.slot == tree.active) else {
            return;
        };
        let workspace_id = workspace.id();
        let local = tree.salt;
        if workspace_id.is_nil() || local == 0 {
            return;
        }
        let key = (workspace_id, Route::layout());
        // The desk the person changes while a write of it is under way is written on the look after
        // the workspace answered that one. An arranging action's change keeps its name until then:
        // it is the write that says what was done.
        let acted = acted.or_else(|| self.held(&key, |held| held.pending_acted.take()));
        if !self.settle_one(&key) {
            if let Some(acted) = acted {
                self.held(&key, |held| held.pending_acted = Some(acted));
            }
            return;
        }
        // What this device's desk has to share: its windows. A workspace it shows with none is a
        // desk it keeps to itself.
        let shares = workspace
            .strips
            .iter()
            .any(|strip| !strip.window_groups.is_empty());
        let Some(sealed) = self.sealed(engine, &key) else {
            // An arranging action's change waits for the record's keys, and is written once they
            // are here.
            if let Some(acted) = acted
                && keys_awaited(engine, &key)
            {
                self.held(&key, |held| held.pending_acted = Some(acted));
            }
            return;
        };
        let (reached, revision, holds) = self.reached(engine, &key);
        // A layout the workspace holds is laid out here before this device writes it: what the
        // desk shows before then is not what the person made of it. A copy this device wrote is
        // what its next write is made on, with the workspace's merged in.
        if reached.is_some() && !holds && !self.held(&key, |held| held.read) {
            if let Some(acted) = acted {
                self.held(&key, |held| held.pending_acted = Some(acted));
            }
            return;
        }
        // A record nobody wrote yet is founded by the first desk with a window in it: a device with
        // nothing to show founds nothing, so the record stays one the devices that do show
        // something make.
        if reached.is_none() && !holds && !shares {
            return;
        }
        let mut notes = self.notes.of_workspace(workspace_id);
        if let Some(acted) = &acted {
            for (tab, record) in &acted.shows {
                if !notes.iter().any(|(held, _)| held == tab) {
                    notes.push((*tab, record.clone()));
                }
            }
        }
        self.lay(
            engine,
            person,
            revision,
            Save {
                workspace_id,
                workspace: workspace.clone(),
                local,
                // The record is read while the write under way runs — the arrangement's records,
                // the pane's history — so the write takes a copy of it and this device keeps its.
                kept: self.held(&key, |held| held.kept.clone()),
                reached,
                written: self.held(&key, |held| held.written.clone()),
                own: tree.external_revision,
                notes,
                sealed,
                acted,
            },
        );
    }

    /// Hand `save` to the task that writes it, and take what it came to at once if it is there
    /// already.
    fn lay(&self, engine: &Arc<Engine>, person: &str, revision: Option<[u8; 32]>, save: Save) {
        let engine = Arc::clone(engine);
        let person = person.to_owned();
        let key = (save.workspace_id, Route::layout());
        let (own, notes) = (save.own, save.notes.clone());
        let closed = self.closed_since_written(save.workspace_id, &save.workspace);
        let writing = crate::engine_write::Polled::new(
            self.wake.handle(),
            {
                let person = person.clone();
                async move { Box::pin(save.write(&engine, &person, &closed)).await }
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
        let written = |record: &automerge::AutoCommit| {
            Written::Layout(record.committed_heads(), laying.own, laying.notes.clone())
        };
        match laid {
            Laid::Held { record, windows } => {
                // Nothing to write: what this device read is the copy its next write is made on.
                let written = written(&record);
                self.written_windows
                    .borrow_mut()
                    .insert(workspace_id, windows);
                self.held(key, |held| held.written = Some(written));
                self.keep(key, *record, laying.revision);
            }
            Laid::Wrote(taken) => match taken.took {
                Ok(_) => {
                    let written = written(&taken.record);
                    self.written_windows
                        .borrow_mut()
                        .insert(workspace_id, taken.windows);
                    // The change is one step of this person's in the layout's own history: an undo
                    // takes it back, whichever of their devices asks for it. A write can land
                    // before this device read the layout (the first window a session opens goes out
                    // on the pass before the record is read): the history begins here, and the
                    // change it makes is its first step.
                    let before = taken.before.clone();
                    let after = taken.record.committed_heads();
                    self.held(key, |held| {
                        held.written = Some(written);
                        let log = held.undo.get_or_insert_with(|| {
                            let mut from = (*taken.record).clone();
                            history(&mut from)
                        });
                        if taken.step {
                            log.record_committed_span(
                                &taken.record,
                                before,
                                after,
                                None,
                                None,
                                Some(user(&laying.person)),
                                None,
                            );
                        }
                    });
                    self.keep(key, *taken.record, laying.revision);
                }
                Err(error) => {
                    // The record is not the workspace's until it takes it: the next write is made
                    // on what the workspace holds, not on the write it refused. A history over the
                    // refused copy names changes the workspace does not hold. The same desk on the
                    // same record is not written again: the workspace would refuse it again.
                    self.held(key, |held| {
                        held.written = Some(Written::Layout(
                            taken.before.clone(),
                            laying.own,
                            laying.notes.clone(),
                        ));
                        held.kept = None;
                        held.based = None;
                        held.undo = None;
                    });
                    self.say(
                        &error,
                        "the workspace's layout was not published",
                        workspace_id,
                    );
                }
            },
            Laid::Failed(error, record) => {
                // What was refused is not written, and the look that found it is not one that wrote
                // it: the next look tries again.
                self.held(key, |held| {
                    held.written = None;
                    held.kept = record.map(|record| *record);
                });
                self.say(&error, "the workspace's layout was not kept", workspace_id);
            }
        }
    }

    /// The last step of `person`'s in `workspace_id` they can take back: who made it and what it
    /// did.
    pub(crate) fn last_step(
        &self,
        workspace_id: WorkspaceId,
        person: &str,
    ) -> Option<(String, String)> {
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

    /// Take back (`redo` false) or bring back `person`'s last step in `workspace_id` — of its
    /// layout, or of their own view of it, whichever they made last — show what it came to on this
    /// device's desk, and publish it. Whether a step was taken.
    pub(crate) fn step(
        &self,
        workspace_id: WorkspaceId,
        person: &str,
        redo: bool,
        handle: &DesktopHandle,
    ) -> bool {
        let Some(engine) = self.engine() else {
            return false;
        };
        let engine = &engine;
        let keys = [
            (workspace_id, Route::layout()),
            (workspace_id, Route::view(person)),
        ];
        let busy = keys.iter().fold(false, |busy, key| {
            !self.settle_one(key) | self.held(key, |held| held.pending_acted.is_some()) | busy
        });
        if busy {
            // A record the step chooses from is being written: it is taken on the look that finds
            // that write done, on what the workspace then holds. The person asked, and it is theirs
            // to take — their gesture is not refused because a write of the same desk was quicker.
            self.held(&keys[0], |held| held.pending_step = Some(redo));
            return true;
        }
        // The person's last step: the newest of either record's, and both where one arranging
        // action changed both (its gesture is the same in each).
        let mut candidates = Vec::new();
        for key in keys {
            let Some(sealed) = self.sealed(engine, &key) else {
                continue;
            };
            let (reached, revision, _) = self.reached(engine, &key);
            let kept = self.held(&key, |held| held.kept.clone());
            let mut doc = base_of(workspace_id, kept, reached);
            doc.set_actor(actor(engine, person));
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
                    sealed,
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
            stepped |= self.take_step(engine, workspace_id, person, redo, handle, candidate);
        }
        stepped
    }

    /// Take `person`'s step of `candidate`'s record, show what it came to on this device's desk,
    /// and publish it. Whether there was one.
    fn take_step(
        &self,
        engine: &Arc<Engine>,
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
            sealed,
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
                let desk = shared_record::read(&doc, None).unwrap_or_default();
                self.fold_into(workspace_id, &doc, &desk.tree, workspace, salt, &device);
            }
            handle.replace_tree(tree);
            let own = handle.read(|tree| tree.external_revision);
            let heads = doc.committed_heads();
            self.held(&key, |held| held.folded = Some(heads.clone()));
            Written::Layout(heads, own, self.notes.of_workspace(workspace_id))
        } else {
            let view = PersonView::read(&doc);
            if let Some(slot) = tree
                .workspaces
                .iter()
                .find(|held| held.id() == workspace_id)
                .map(|held| held.slot)
            {
                view.applied(&mut tree, slot);
            }
            handle.replace_tree(tree);
            Written::View(view)
        };
        self.held(&key, |held| held.undo = Some(log));
        self.keep(&key, doc.clone(), revision);
        self.publish(engine, &key, doc, Some(written), &before, revision, sealed);
        true
    }

    /// Make `asked`'s arranging `action` of `tree`'s workspace in front one change of that
    /// workspace's layout, named by who asked and what they did: what a step of the person's is
    /// taken back from, and the windows it placed show the records they name.
    pub(crate) fn performed(
        &self,
        before: &DesktopTree,
        tree: &DesktopTree,
        asked: &str,
        action: &tilo_session_core::arrangement::ArrangementAction,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) {
        self.placed.borrow_mut().extend(shows.iter().cloned());
        let Some(engine) = self.engine() else {
            return;
        };
        let Some(person) = self.person.borrow().clone() else {
            return;
        };
        let message = tilo_session_core::arrangement::step_context(asked, action)
            .change_message()
            .unwrap_or_else(|| shared_record::change_message(Some(asked)));
        // What the action did to the person's own view of the workspace (a width, a window made
        // as large as its display) is a step of theirs in that view, written on the next look.
        if let Some(workspace) = tree.active_workspace()
            && !workspace.id().is_nil()
        {
            let key = (workspace.id(), Route::view(&person));
            let was = PersonView::of(before, workspace.slot);
            let acted = Acted {
                message: message.clone(),
                shows: Vec::new(),
                was: Some(was),
            };
            self.held(&key, |held| held.pending_acted = Some(acted));
        }
        self.capture_layout(
            &engine,
            &person,
            tree,
            Some(Acted {
                message,
                shows: shows.to_vec(),
                was: None,
            }),
        );
    }

    /// A step of a layout the arrangement asked for while that layout's write was under way: the
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
    /// a run of them — and publish it. `own` is the tree revision the desk stands at. Whether
    /// anything was written.
    pub(crate) fn edit(
        &self,
        workspace_id: WorkspaceId,
        person: &str,
        own: u64,
        edit: impl FnOnce(&mut automerge::AutoCommit) -> bool,
    ) -> bool {
        let Some(engine) = self.engine() else {
            return false;
        };
        let engine = &engine;
        let key = (workspace_id, Route::layout());
        if !self.settle_one(&key) {
            return false;
        }
        let Some(sealed) = self.sealed(engine, &key) else {
            return false;
        };
        let (reached, revision, _) = self.reached(engine, &key);
        let kept = self.held(&key, |held| held.kept.clone());
        let mut doc = base_of(workspace_id, kept, reached);
        doc.set_actor(actor(engine, person));
        let before = doc.get_heads();
        if !edit(&mut doc) {
            doc.rollback();
            return false;
        }
        if doc.pending_ops() == 0 {
            return false;
        }
        shared_record::commit_change(&mut doc, person, crate::activity::now_seconds(), None);
        let heads = doc.committed_heads();
        let notes = self.notes.of_workspace(workspace_id);
        self.held(&key, |held| held.folded = Some(heads.clone()));
        self.keep(&key, doc.clone(), revision);
        self.publish(
            engine,
            &key,
            doc,
            Some(Written::Layout(heads, own, notes)),
            &before,
            revision,
            sealed,
        );
        true
    }

    /// Write the person here's view of every workspace whose view they changed into that
    /// workspace's record of it.
    /// `now`: the view is written as it stands, without waiting for it to rest.
    fn capture_view(
        &self,
        engine: &Arc<Engine>,
        person: &str,
        tree: &DesktopTree,
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
            let view = PersonView::of(tree, workspace.slot);
            // An arranging action's change of the view is written at once, as a step of the
            // person's; any other change once it rests.
            let acted = self.held(&key, |held| {
                (held.read && held.writing.is_none())
                    .then(|| held.pending_acted.take())
                    .flatten()
            });
            let view = if acted.is_some() {
                view
            } else {
                let Some(view) = self.held(&key, |held| rested(held, &view, now)) else {
                    continue;
                };
                view
            };
            self.held(&key, |held| {
                held.written = Some(Written::View(view.clone()));
                held.pending = None;
            });
            let Some(sealed) = self.sealed(engine, &key) else {
                // The record's keys are not here yet: it is written on the look after they arrive,
                // an arranging action's step with it.
                let awaited = keys_awaited(engine, &key);
                self.held(&key, |held| {
                    held.read = false;
                    held.written = None;
                    held.pending = None;
                    if awaited {
                        held.pending_acted = acted;
                    }
                });
                continue;
            };
            let (reached, revision, _) = self.reached(engine, &key);
            let mut held = base_of(
                workspace_id,
                self.held(&key, |held| held.kept.clone()),
                reached,
            );
            let from = held.get_heads();
            let step = match view_written(&mut held, &view, acted.as_ref(), engine, person) {
                Ok(step) => step,
                Err(error) => {
                    self.say(
                        &error.to_string(),
                        "the view of the layout was not kept",
                        workspace_id,
                    );
                    continue;
                }
            };
            if held.pending_ops() == 0 {
                self.keep(&key, held, revision);
                continue;
            }
            if let Some(acted) = acted {
                held.commit_with(
                    automerge::transaction::CommitOptions::default()
                        .with_message(acted.message)
                        .with_time(crate::activity::now_seconds()),
                );
                let after = held.get_heads();
                self.held(&key, |kept| {
                    let log = kept.undo.get_or_insert_with(|| {
                        let mut from = held.clone();
                        history(&mut from)
                    });
                    log.record_committed_span(
                        &held,
                        step,
                        after,
                        None,
                        None,
                        Some(user(person)),
                        None,
                    );
                });
            }
            wrote |= self.publish(
                engine,
                &key,
                held,
                Some(Written::View(view)),
                &from,
                revision,
                sealed,
            );
        }
        wrote
    }

    /// What this device seals `key`'s writes with, and whether it writes at all now: a workspace
    /// whose records are its members' sealed channels takes a sealed write, and one whose channel
    /// keys this device holds none of — because they are not here yet, or because this device has
    /// no secret store at all — takes none. Neither is a workspace's verdict, so no notice names a
    /// workspace as having refused a write it never saw.
    fn sealed(&self, engine: &Engine, key: &Key) -> Option<Sealing> {
        let (workspace_id, route) = key;
        if engine.commits_documents(*workspace_id) {
            // Its host takes part in no channels: its records are documents the owner commits,
            // written in the clear.
            return Some(Sealing::Clear);
        }
        match engine.seal_keys(*workspace_id, route.app, &route.record) {
            Ok(SealKeys::Here(keys)) => Some(Sealing::Sealed(keys)),
            // The record's channel keys are not here yet: it is written on the look after they
            // arrive, and this look leaves the workspace unread so that there is one.
            Ok(SealKeys::Waiting) => {
                self.held(key, |held| held.read = false);
                None
            }
            // This device holds no secret store, so it holds no channel keys of any workspace: its
            // desks stay the ones it keeps, and the person hears nothing.
            Ok(SealKeys::NoSecretStore) => None,
            Err(error) => {
                self.say(
                    &error,
                    "the workspace's layout was not written",
                    *workspace_id,
                );
                None
            }
        }
    }

    /// Publish `candidate`, a change of a record this device built on the loop — its view of a
    /// layout, a step taken from a layout's history, or what a model said a change was — as the
    /// workspace's own copy of it: what the workspace takes is what the next look writes on, and
    /// what it refuses is dropped, so the next look writes it again on what the workspace holds.
    /// A record of the workspace is written the way the workspace keeps them: sealed under the
    /// record channel's keys where they are its members' sealed channels, and in the clear where
    /// the workspace's device commits its documents. A sealed record starts sealed: the write that
    /// first holds anything of it is a checkpoint of the whole of it, and every write after it a
    /// sealed change. Members read a sealed record from its sealed history alone, so a record
    /// founded in the clear would leave them a genesis they cannot account for and a record they
    /// could never read.
    fn publish(
        &self,
        engine: &Arc<Engine>,
        key: &Key,
        mut candidate: automerge::AutoCommit,
        replaced: Option<Written>,
        heads: &[ChangeHash],
        revision: Option<[u8; 32]>,
        sealed: Sealing,
    ) -> bool {
        let (workspace_id, route) = key.clone();
        if replaced.is_none() {
            return false;
        }
        // Every change a device signs says what it is and who made it: the edit of the record this
        // person just made here. A step taken from the history is its own change already.
        if candidate.pending_ops() > 0 {
            let author = self.person.borrow().clone();
            candidate.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(shared_record::change_message(author.as_deref())),
            );
        }
        let changes = candidate.save_after(heads);
        // A write on no history is the whole record: the one that founds it.
        let founding = heads.is_empty();
        let (engine, record) = (Arc::clone(engine), route.record.clone());
        let mut writing = crate::engine_write::Writing::write(self.wake.handle(), async move {
            engine
                .write_record(
                    workspace_id,
                    (route.app, &record),
                    &changes,
                    founding,
                    sealed.keys(),
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
                    return true;
                }
                None => Going::View(publishing),
            },
            Going::Layout(mut laying) => match laying.writing.done() {
                Some(laid) => {
                    self.laid(key, laid, &laying);
                    return true;
                }
                None => Going::Layout(laying),
            },
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
        match written {
            Ok(_write) => {
                let Some(written) = replaced else {
                    return;
                };
                self.held(key, |held| held.written = Some(written));
                self.keep(key, candidate, revision);
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
                    &error,
                    "the workspace's layout was not published",
                    workspace_id,
                );
            }
        }
    }

    /// Say why a workspace's record was not kept or published, for the caller to tell the person:
    /// once per reason, not on every frame.
    fn say(&self, error: &str, what: &str, workspace_id: WorkspaceId) {
        tracing::warn!(%error, %workspace_id, "{what}");
        if self.told.borrow().as_deref() != Some(error) {
            *self.refused.borrow_mut() = Some((workspace_id, error.to_owned()));
        }
    }

    /// The refusal the person has not heard yet, once: why what they changed on a workspace's
    /// layout did not reach the workspace.
    pub(crate) fn take_refusal(&self) -> Option<(WorkspaceId, String)> {
        let (workspace_id, refused) = self.refused.borrow_mut().take()?;
        *self.told.borrow_mut() = Some(refused.clone());
        Some((workspace_id, refused))
    }

    /// What the workspace's copy of `key`'s record holds here, read against the revision this
    /// device's own copy holds, the revision it was read at, and whether this device holds a copy.
    /// A record this device was never sent, or cannot read, is one it holds nothing of, and one
    /// that did not move since its copy is the copy.
    fn reached(
        &self,
        engine: &Engine,
        key: &Key,
    ) -> (Option<automerge::AutoCommit>, Option<[u8; 32]>, bool) {
        let (holds, known) = self.held(key, |held| (held.kept.is_some(), held.based));
        let known = holds.then_some(known).flatten();
        let read = Self::record(engine, key, known).unwrap_or(None);
        let revision = read.as_ref().map(|(_, revision)| *revision);
        (read.map(|(doc, _)| doc), revision, holds)
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
                .all(|head| automerge::ReadDoc::get_change_by_hash(&reached, head).is_some()) =>
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
/// each and the windows in each group, and where the arrangement puts them.
pub(crate) type Arranged = (
    Vec<(u64, String, Vec<(u64, Vec<u64>)>)>,
    tilo_desktop_model::arrangement::Arrangement,
);

pub(crate) fn arranged(doc: &automerge::AutoCommit, workspace_id: WorkspaceId) -> Option<Arranged> {
    let desk = shared_record::read(doc, None)?;
    let workspace = desk
        .tree
        .workspaces
        .into_iter()
        .find(|held| held.id() == workspace_id)?;
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
    Some((strips, workspace.arrangement))
}

/// What this device's write lays down: `workspace`, the desk as this device has it, with the
/// record's windows it does not show yet added where the record has them, and the windows it closed
/// since its last write left out. Where this desk put a window it holds is what the write says —
/// the record is what the workspace shows, and everything anyone moved here is this device's word
/// for it.
fn laid_into(
    record: &automerge::AutoCommit,
    workspace: Workspace,
    closed: &BTreeSet<u64>,
) -> Workspace {
    let mut laid_out = workspace;
    let held = windows_of(&laid_out);
    let Some(theirs) = shared_record::read(record, None) else {
        return laid_out;
    };
    let Some(mut their_workspace) = theirs
        .tree
        .workspaces
        .into_iter()
        .find(|held| held.id() == laid_out.id())
    else {
        return laid_out;
    };
    // Only what this desk does not show, and does not hold closed: everything else is where this
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

/// Whether `key`'s record waits for its channel keys to reach this device.
fn keys_awaited(engine: &Engine, key: &Key) -> bool {
    let (workspace_id, route) = key;
    matches!(
        engine.seal_keys(*workspace_id, route.app, &route.record),
        Ok(SealKeys::Waiting)
    )
}

/// Write `view` into `doc`, a person's view record. Where an arranging action changed it, the view
/// as it stood before the action is committed first and the change is written as `person`'s, on
/// whichever of their devices made it. The heads the action's step starts from.
fn view_written(
    doc: &mut automerge::AutoCommit,
    view: &PersonView,
    acted: Option<&Acted>,
    engine: &Engine,
    person: &str,
) -> Result<Vec<ChangeHash>, automerge::AutomergeError> {
    doc.set_actor(automerge::ActorId::random());
    if let Some(was) = acted.and_then(|acted| acted.was.as_ref())
        && PersonView::read(doc) != *was
        && was.write(doc)?
    {
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(shared_record::change_message(Some(person))),
        );
    }
    let step = doc.get_heads();
    if acted.is_some() {
        doc.set_actor(actor(engine, person));
    }
    view.write(doc)?;
    Ok(step)
}

/// The view to write into `key`'s record now, if any: what a look writes only once it rests, and
/// never while a write of it is under way or before this device has read the record. A view that
/// did not change is not written again.
fn rested(held: &mut Held, view: &PersonView, now: bool) -> Option<PersonView> {
    // A view this device has not read here is written on the look after it reads it: it writes
    // what the person changed, never a desk still being opened.
    if !held.read || held.writing.is_some() {
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

/// Every window `desk`, a record's layout, holds.
fn tabs_of(desk: &DesktopTree) -> BTreeSet<u64> {
    desk.workspaces.iter().flat_map(windows_of).collect()
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

/// The workspaces' layouts, as the pane reads the desk's own history: the record of each, and the
/// model's word for what its changes were.
pub(crate) struct LayoutHistory {
    layouts: Rc<SharedDesktops>,
    person: String,
}

impl LayoutHistory {
    pub(crate) fn new(layouts: Rc<SharedDesktops>, person: &str) -> Self {
        Self {
            layouts,
            person: person.to_owned(),
        }
    }

    fn workspace_of(tree: &DesktopTree, slot: WorkspaceSlot) -> Option<WorkspaceId> {
        let workspace_id = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.slot == slot)
            .map(Workspace::id)?;
        (!workspace_id.is_nil()).then_some(workspace_id)
    }
}

impl crate::activity::DeskHistory for LayoutHistory {
    fn history(
        &self,
        tree: &DesktopTree,
        slot: WorkspaceSlot,
    ) -> Option<tilo_desktop_surface::activity::ActivitySnapshot> {
        let workspace_id = Self::workspace_of(tree, slot)?;
        // A workspace with no layout yet still has a history: what its devices answered.
        let snapshot = tilo_desktop_surface::activity::ActivitySnapshot::of_layout(
            tree.clone(),
            slot,
            self.layouts.layout(workspace_id).unwrap_or_default(),
        );
        Some(match self.layouts.governance(workspace_id) {
            Some(document) => snapshot.governing(document),
            None => snapshot,
        })
    }

    fn documents(&self, tree: &DesktopTree) -> Vec<(WorkspaceSlot, automerge::AutoCommit)> {
        tree.workspaces
            .iter()
            .filter_map(|workspace| Some((workspace.slot, self.layouts.layout(workspace.id())?)))
            .collect()
    }

    fn describe(
        &self,
        tree: &DesktopTree,
        slot: WorkspaceSlot,
        write: &mut dyn FnMut(&mut automerge::AutoCommit) -> bool,
    ) -> bool {
        let Some(workspace_id) = Self::workspace_of(tree, slot) else {
            return false;
        };
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

    fn attach_engine(&self, engine: &Arc<Engine>) {
        self.layouts.attach_engine(engine);
    }
}

/// The workspaces `handle`'s desk shows, by id.
fn tree_workspaces(handle: &DesktopHandle) -> Vec<WorkspaceId> {
    handle.read(|tree| tree.workspaces.iter().map(Workspace::id).collect())
}

/// The desk's layout as the arrangement keeps it: the layout of the workspace in front, which every
/// member shows.
pub(crate) struct Steps {
    layouts: Rc<SharedDesktops>,
    handle: DesktopHandle,
}

impl Steps {
    pub(crate) fn new(layouts: Rc<SharedDesktops>, handle: DesktopHandle) -> Self {
        Self { layouts, handle }
    }

    fn workspace_id(&self) -> Option<WorkspaceId> {
        let workspace_id = self
            .handle
            .read(|tree| tree.active_workspace().map(Workspace::id))?;
        (!workspace_id.is_nil()).then_some(workspace_id)
    }
}

impl tilo_session_core::arrangement::Layouts for Steps {
    fn step(&self, person: &str, redo: bool) -> tilo_session_core::arrangement::Stepped {
        use tilo_session_core::arrangement::Stepped;
        let Some(workspace_id) = self.workspace_id() else {
            return Stepped::None;
        };
        if self.layouts.step(workspace_id, person, redo, &self.handle) {
            Stepped::Taken
        } else {
            Stepped::None
        }
    }

    fn last_step(&self, person: &str) -> Option<(String, String)> {
        self.layouts.last_step(self.workspace_id()?, person)
    }

    fn acted(
        &self,
        before: &DesktopTree,
        tree: &DesktopTree,
        asked: &str,
        action: &tilo_session_core::arrangement::ArrangementAction,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) {
        self.layouts.performed(before, tree, asked, action, shows);
    }

    fn shown(&self) -> std::collections::BTreeMap<tilo_desktop_model::TabId, String> {
        let mut shown: BTreeMap<u64, String> = tree_workspaces(&self.handle)
            .into_iter()
            .filter_map(|workspace_id| self.layouts.layout(workspace_id))
            .filter_map(|doc| shared_record::read(&doc, None))
            .flat_map(|desk| desk.records)
            .collect();
        for (tab, record) in self.layouts.placed.borrow().iter() {
            shown.entry(*tab).or_insert_with(|| record.clone());
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    tilo_automerge_isolate::test_worker!();

    use super::*;

    fn view(strip: u64) -> PersonView {
        PersonView {
            front_strip: Some(tilo_desktop_model::StripId(strip)),
            ..PersonView::default()
        }
    }

    /// A view that did not move is not written again: the look that finds it writes nothing.
    #[test]
    fn a_view_that_did_not_move_is_not_written() {
        let written = view(11);
        let mut held = Held {
            read: true,
            written: Some(Written::View(written.clone())),
            ..Held::default()
        };
        assert_eq!(rested(&mut held, &written, false), None);
    }

    /// A view is written once it rests, not on the look that found it move: a drag or a burst of
    /// switches changes it on every look and is one write when it is over.
    #[test]
    fn a_view_is_written_once_it_rests() {
        let mut held = Held {
            read: true,
            ..Held::default()
        };
        assert_eq!(
            rested(&mut held, &view(11), false),
            None,
            "the look that found it"
        );
        assert_eq!(
            rested(&mut held, &view(11), false),
            Some(view(11)),
            "the look after it, unchanged"
        );
        assert_eq!(
            rested(&mut held, &view(12), false),
            None,
            "a look that moved it again writes nothing yet"
        );
        assert_eq!(rested(&mut held, &view(12), false), Some(view(12)));
    }

    /// A write of the view under way holds the look back, and a record this device has not read is
    /// not written over.
    #[test]
    fn a_view_is_written_only_between_writes_and_after_a_read() {
        let mut held = Held::default();
        assert_eq!(
            rested(&mut held, &view(11), false),
            None,
            "the record was not read"
        );
        held.read = true;
        held.writing = Some(Going::View(Box::new(Publishing {
            writing: crate::engine_write::Writing::write(
                std::sync::Arc::new(|| {}) as tilo_service::Wake,
                async { Err("the test writes nothing".to_owned()) },
            ),
            candidate: automerge::AutoCommit::new(),
            replaced: None,
            revision: None,
        })));
        assert_eq!(
            rested(&mut held, &view(11), false),
            None,
            "a write is under way"
        );
        held.writing = None;
        let _ = rested(&mut held, &view(11), false);
        assert_eq!(rested(&mut held, &view(11), false), Some(view(11)));
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("00000000-0000-4000-8000-000000000042").expect("an id")
    }

    /// A write of the layout under way holds the session open.
    #[test]
    fn a_layout_write_under_way_holds_the_session_open() {
        let desktops = SharedDesktops::default();
        let key = (workspace_id(), Route::layout());
        desktops.held(&key, |held| {
            held.read = true;
            held.writing = Some(Going::View(Box::new(Publishing {
                writing: crate::engine_write::Writing::write(
                    std::sync::Arc::new(|| {}) as tilo_service::Wake,
                    async { std::future::pending::<crate::engine_write::Written>().await },
                ),
                candidate: automerge::AutoCommit::new(),
                replaced: None,
                revision: None,
            })));
        });
        assert!(
            !desktops.settle_publishing(),
            "a write that has not answered leaves the session unsettled"
        );
        assert!(
            desktops.held(&key, |held| held.writing.is_some()),
            "the write is still under way"
        );
    }

    /// A key whose signatures wait for the test to let them go: a write that signs through it is
    /// under way until then, whatever the machine's speed.
    #[derive(Default)]
    struct Gate {
        held: std::sync::atomic::AtomicBool,
        waiting: std::sync::Mutex<Vec<std::task::Waker>>,
    }

    impl Gate {
        fn held(&self) -> bool {
            self.held.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn hold(&self) {
            self.held.store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn open(&self) {
            self.held.store(false, std::sync::atomic::Ordering::SeqCst);
            let waiting = std::mem::take(&mut *self.waiting.lock().expect("the gate"));
            for waker in waiting {
                waker.wake();
            }
        }
    }

    /// A device key signing behind a [`Gate`].
    struct GatedSigner {
        key: ed25519_dalek::SigningKey,
        gate: Arc<Gate>,
    }

    impl collab_types::signer::DeviceSigner for GatedSigner {
        fn device_public_key(&self) -> [u8; 32] {
            self.key.verifying_key().to_bytes()
        }

        fn endpoint_secret(&self) -> Option<[u8; 32]> {
            Some(self.key.to_bytes())
        }

        fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
            let signature = ed25519_dalek::Signer::sign(&self.key, &message).to_bytes();
            let gate = Arc::clone(&self.gate);
            Box::pin(std::future::poll_fn(move |context| {
                if gate.held() {
                    gate.waiting
                        .lock()
                        .expect("the gate")
                        .push(context.waker().clone());
                    if gate.held() {
                        return std::task::Poll::Pending;
                    }
                }
                std::task::Poll::Ready(Ok(signature))
            }))
        }
    }

    /// The wake that says a record's write is done, as a channel.
    fn woken() -> (tilo_service::LoopWake, std::sync::mpsc::Receiver<()>) {
        struct Rings(std::sync::Mutex<std::sync::mpsc::Sender<()>>);
        impl std::task::Wake for Rings {
            fn wake(self: std::sync::Arc<Self>) {
                let _ = self.0.lock().map(|rings| rings.send(()));
            }
        }
        let (rings, heard) = std::sync::mpsc::channel();
        let wake = tilo_service::LoopWake::default();
        wake.bind(std::task::Waker::from(std::sync::Arc::new(Rings(
            std::sync::Mutex::new(rings),
        ))));
        (wake, heard)
    }

    /// A layout save runs off the loop: folding the record into the desk, committing this device's
    /// change and the workspace's answer are the task's; the look only hands the save over.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one save from the look that hands it over to the record the task wrote"
    )]
    fn a_layout_save_is_made_and_written_off_the_loop() {
        use ed25519_dalek::SigningKey;
        use tilo_desktop_model::{Strip, StripId, WindowGroup, WindowWidth};

        use crate::engine::Engine;

        let state = tempfile::tempdir().expect("a state directory");
        let root = SigningKey::from_bytes(&[0x30; 32]);
        let key = SigningKey::from_bytes(&[0x31; 32]);
        let cert = tilo_peer_state::owner::testing::certificate(&root, &key, "Laptop");
        let gate = Arc::new(Gate::default());
        let theirs = Arc::clone(&gate);
        let engine = Arc::new(
            Engine::open(
                tilo_identity_host::custody::HostIdentity::kept(
                    cert.clone(),
                    GatedSigner { key, gate: theirs },
                ),
                Some(state.path()),
                std::iter::empty(),
                crate::platform::custody::of_this_desk(),
            )
            .expect("this device's engine opens"),
        );
        let salt = tilo_desktop_model::ids::device_salt_of_key(&cert.device_public_key);
        let person = hex::encode(cert.user_public_key);
        let workspace = WorkspaceId::new();
        let slot = WorkspaceSlot(1);
        pollster::block_on(engine.found(slot, workspace, "Desk", None));
        assert!(engine.holds(workspace), "the desk's workspace is held here");

        // The desk: one workspace with one window of this device's.
        let mut ids = tilo_desktop_model::IdMinter::new(salt);
        let (strip, group, tab) = (ids.mint(), ids.mint(), ids.mint());
        let mut workspace_view = Workspace::in_workspace(slot, workspace, "Desk");
        let mut strip_view = Strip::new(StripId(strip), "Main");
        strip_view.window_groups.push(WindowGroup {
            id: group,
            tabs: vec![tilo_desktop_model::Tab {
                id: tab,
                title: "Notes".to_owned(),
                tile: tilo_desktop_model::TileContent::Stub {
                    kind: tilo_desktop_model::AppKind::Other("Notes".to_owned()),
                    label: "Notes".to_owned(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: tab,
            width: WindowWidth::Wide,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace_view.strips = vec![strip_view];
        let tree = DesktopTree {
            workspaces: vec![workspace_view],
            active: slot,
            salt,
            ..DesktopTree::default()
        };

        let (wake, heard) = woken();
        let desktops = SharedDesktops::new(
            Arc::new(crate::continuations::Continuations::default()),
            wake,
            person.clone(),
        );
        desktops.attach_engine(&engine);
        // The keys of the layout record's channel, as the channel service hands them to this device.
        let keys = channel_crypto::ChannelGroup::create(
            collab_types::workspace_scope(&workspace.to_string()),
            channel_crypto::MemberKey::generate("owner").expect("a member key"),
        )
        .expect("the record's channel")
        .content_keys()
        .expect("the record's keys");
        engine.hand_keys(
            workspace,
            collab_types::ChannelRoute::new(DESKTOP_APP, DESKTOP_RECORD),
            keys.for_unit(collab_types::document_channel_id(
                DESKTOP_APP,
                &workspace.to_string(),
                DESKTOP_RECORD,
            )),
        );
        let layout = (workspace, Route::layout());
        // The write's signature waits for the test: it is under way, so nothing of it can have been
        // taken in here.
        gate.hold();
        desktops.capture(&engine, &tree);
        assert!(
            desktops.held(&layout, |held| matches!(
                held.writing,
                Some(Going::Layout(_))
            )),
            "the look hands the save to the task that makes it"
        );
        assert!(
            desktops.held(&layout, |held| held.written.is_none()),
            "the loop decides nothing of what the save came to"
        );
        assert!(
            desktops.held(&layout, |held| held.kept.is_none() && held.undo.is_none()),
            "the loop folds no record and begins no history"
        );
        assert!(
            desktops.reached(&engine, &layout).0.is_none(),
            "the loop writes nothing into the workspace"
        );
        assert!(
            !desktops.settle_publishing(),
            "the save is the task's, and has not answered yet"
        );

        // The task answers, and the change it wrote is one step of the person's in the layout's
        // history, on any of their devices.
        gate.open();
        heard
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the write wakes the loop once it is done");
        assert!(desktops.settle_publishing(), "the write is done");
        assert!(
            desktops.held(&layout, |held| held.written.is_some()),
            "the look takes what the write came to"
        );
        let record = desktops
            .layout(workspace)
            .expect("the record this device holds");
        assert_eq!(
            shared_record::read(&record, None).map(|desk| windows_of(
                desk.tree
                    .workspaces
                    .first()
                    .expect("the record's workspace")
            )),
            Some(std::collections::BTreeSet::from([tab])),
            "the record holds the window this desk shows"
        );
        assert!(
            desktops.last_step(workspace, &person).is_some(),
            "the change the task wrote is one step of the person's"
        );
    }
}
