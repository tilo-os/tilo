//! The records of a workspace's layout: the layout itself — its strips, groups and windows, which
//! every member shows and changes ([`tilo_desktop_model::shared_record`]) — and each person's view
//! of it, where they are looking and how wide their windows are
//! ([`tilo_desktop_model::person_view`]).
//! Both are records of the workspace, read from the engine's copy of it, which holds what the
//! workspace admitted and what it held before this device joined. Nothing here decides whether a
//! write may land: that is the workspace's, decided where the engine takes it in. A record that did
//! not move since the last look is neither read nor written again, and a write runs off the loop
//! ([`crate::engine_write`]).

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
use std::sync::Arc;

use automerge::ChangeHash;
use automerge::transaction::Transactable as _;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, OpenedBy};
use tilo_desktop_model::person_view::{self, PersonView};
use tilo_desktop_model::shared_record::{self, device};
use tilo_desktop_model::{
    DesktopTree, TileContent, Workspace, WorkspaceId, WorkspaceSlot, peer_salt_of,
};

use tilo_broker_core::custody::OwnerCustody as _;

use crate::engine::{Engine, SealKeys};
use crate::sync::Client as Sync;

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
    /// The write of the record under way.
    writing: Option<Going>,
    /// Whether the record was read here.
    read: bool,
}

/// A write of a record under way. Boxed: each holds a document, and the state beside it is small.
enum Going {
    /// The view's: written here, and published off the loop.
    View(Box<Publishing>),
    /// The layout's: made and written off the loop, since folding the record and writing this
    /// device's desk into it is what a save of it costs.
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
    /// The write was not made and why: folding the record into this desk, or writing them
    /// together, failed. The record to keep as this device's copy, where it still has one.
    Failed(String, Option<Box<automerge::AutoCommit>>),
}

/// The record this device made, the windows of its desk that write holds, and what the workspace
/// did with it.
struct Taken {
    record: Box<automerge::AutoCommit>,
    windows: BTreeSet<u64>,
    took: crate::engine_write::Written,
}

/// What a look hands to the task that writes the layout: this device's desk, and what it read of
/// the record.
struct Save {
    room: WorkspaceId,
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
    /// The tree revision and the record each window showed.
    own: u64,
    notes: Vec<(u64, String)>,
    sealed: Sealing,
}

impl Save {
    /// Write it: the record this device writes on with the record laid into this desk's workspace
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
            room,
            workspace,
            local,
            kept,
            reached,
            written,
            own,
            notes,
            sealed,
        } = self;
        let here = windows_of(&workspace);
        let base = base_of(room, kept, reached);
        let laid_out = laid_into(&base, workspace, closed);
        let windows = &windows_of(&laid_out) & &here;
        let heads = base.committed_heads();
        let unchanged = written == Some(Written::Layout(heads.clone(), own, notes.clone()));
        if unchanged {
            return Laid::Held {
                record: Box::new(base),
                windows,
            };
        }
        // The record is every member's: what this device writes into it goes in as this person on
        // this device, never under the actor the copy here was last written by, which two devices
        // writing at once would number the same.
        let mut candidate = base;
        candidate.set_actor(automerge::ActorId::from(
            ui::history_source::authors::device_actor(
                person,
                &automerge::ActorId::from(engine.identity().device_cert.device_public_key.to_vec()),
            ),
        ));
        // A window that runs on another device is that device's to add to the record, whoever
        // minted the tab it stands in here: the tab this desk pictures a shared window in, beside
        // its own view of that window, is this desk's alone.
        let here = hex::encode(engine.identity().device_cert.device_public_key);
        let elsewhere: BTreeSet<u64> = laid_out
            .tabs()
            .filter(|tab| {
                matches!(
                    &tab.tile,
                    TileContent::HostedWindow { host, .. } if !host.eq_ignore_ascii_case(&here)
                )
            })
            .map(|tab| tab.id)
            .collect();
        let mine = |id: u64| peer_salt_of(id) == local && !elsewhere.contains(&id);
        let by_tab: BTreeMap<u64, String> = notes.into_iter().collect();
        let records = |id: u64| by_tab.get(&id).cloned();
        match device::write(
            &mut candidate,
            room,
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
        // Every change a device signs says what it is and who made it: the edit of the record this
        // person just made here.
        candidate.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(shared_record::change_message(Some(person))),
        );
        let changes = candidate.save_after(&heads);
        // A write on no history is the whole record: the one that founds it.
        let founding = heads.is_empty();
        let took = engine
            .write_record(
                room,
                (DESKTOP_APP, DESKTOP_RECORD),
                &changes,
                founding,
                sealed.keys(),
            )
            .await;
        Laid::Wrote(Box::new(Taken {
            record: Box::new(candidate),
            windows,
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
    /// The person at this device, whose view of each workspace is theirs.
    person: RefCell<Option<String>>,
    /// Every record of every workspace this device reads and writes.
    held: RefCell<HashMap<Key, Held>>,
}

impl SharedDesktops {
    /// This session's records, writing what its windows note of themselves (`notes`), on the loop
    /// `wake` wakes.
    pub(crate) fn new(
        notes: Arc<crate::continuations::Continuations>,
        wake: tilo_service::LoopWake,
    ) -> Self {
        Self {
            notes,
            wake,
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

    /// The windows this device's desk has that the layout of `room` holds and this desk no longer
    /// does: the close it made since it last wrote the layout.
    fn closed_since_written(&self, room: WorkspaceId, workspace: &Workspace) -> BTreeSet<u64> {
        let here = windows_of(workspace);
        self.written_windows
            .borrow()
            .get(&room)
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
        let settled = self
            .held
            .borrow()
            .values()
            .all(|held| held.pending.is_none())
            && self.seen.get() == Some((revision, tree.external_revision))
            && tree
                .workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil())
                .all(|workspace| {
                    let room = workspace.id();
                    self.held(&(room, Route::layout()), |held| held.read)
                        && person.as_deref().is_none_or(|person| {
                            self.held(&(room, Route::view(person)), |held| held.read)
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
        let (room, route) = key;
        let Some((bytes, revision)) =
            engine.record_since(&room.to_string(), route.app, &route.record, known)?
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
        self.follow_layout(engine, device, tree) | self.follow_view(engine, tree)
    }

    /// Fold the layout of every workspace in the engine's copy into `tree`.
    fn follow_layout(&self, engine: &Engine, device: &str, tree: &mut DesktopTree) -> bool {
        let local = tree.salt;
        let mut changed = false;
        for workspace in &mut tree.workspaces {
            let room = workspace.id();
            if room.is_nil() {
                continue;
            }
            let key = (room, Route::layout());
            let known = self.held(&key, |held| held.revision);
            let Some((mut doc, revision)) = Self::record(engine, &key, known).ok().flatten() else {
                // A workspace this device holds no record of (not its own, or one still arriving)
                // says nothing of its layout yet: its record is read again next pass.
                self.held(&key, |held| {
                    held.read = false;
                    held.revision = None;
                });
                continue;
            };
            self.held(&key, |held| {
                held.read = true;
                held.revision = Some(revision);
            });
            let heads = doc.get_heads();
            if self.held(&key, |held| held.folded.clone()).as_ref() == Some(&heads) {
                continue;
            }
            self.held(&key, |held| held.folded = Some(heads));
            let Some(desk) = shared_record::read(&doc, None) else {
                // A record that holds no layout yet: nothing of it to show.
                continue;
            };
            self.opened_by.borrow_mut().insert(room, desk.opened_by);
            let mine = |id: u64| local != 0 && peer_salt_of(id) == local;
            let closed = self.closed_since_written(room, workspace);
            let recorded = self.recorded.borrow_mut().insert(room, tabs_of(&desk.tree));
            // A desk this session has not laid out from the record yet keeps nothing apart: what it
            // holds that the record does not may be what someone closed while it was away.
            let opened_here = |id: u64| {
                mine(id)
                    && recorded
                        .as_ref()
                        .is_some_and(|recorded| !recorded.contains(&id))
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
            changed |= device::fold(&doc, workspace, device, &kept);
        }
        changed
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
            let Some(room) = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.slot == slot)
                .map(Workspace::id)
            else {
                continue;
            };
            if room.is_nil() {
                continue;
            }
            let key = (room, route.clone());
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
            let view = PersonView::read(&doc);
            self.held(&key, |held| {
                held.read = true;
                held.revision = Some(revision);
                held.written = Some(Written::View(view.clone()));
            });
            changed |= view.applied(tree, slot);
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
    /// whether this look published the view, the layout's write being handed to the task that
    /// makes it.
    pub(crate) fn capture(
        &self,
        engine: &Arc<Engine>,
        sync: Option<&Sync>,
        tree: &DesktopTree,
    ) -> bool {
        let Some(person) = sync.and_then(|sync| sync.own().user) else {
            return false;
        };
        *self.person.borrow_mut() = Some(person.clone());
        self.capture_layout(engine, &person, tree);
        self.capture_view(engine, &person, tree)
    }

    /// Write this device's changes to the workspace in front's layout into the workspace's record.
    /// Handing them to the task that writes them is all the loop does: folding the record into this
    /// desk's workspace and writing the two together is what a save costs, and none of it runs
    /// here.
    fn capture_layout(&self, engine: &Arc<Engine>, person: &str, tree: &DesktopTree) {
        let Some(workspace) = tree.workspaces.iter().find(|held| held.slot == tree.active) else {
            return;
        };
        let room = workspace.id();
        let local = tree.salt;
        if room.is_nil() || local == 0 {
            return;
        }
        let key = (room, Route::layout());
        // The desk the person changes while a write of it is under way is written on the look after
        // the workspace answered that one.
        if self.held(&key, |held| held.writing.is_some()) {
            return;
        }
        // What this device's desk has to share: its windows. A workspace it shows with none is a
        // desk it keeps to itself.
        let shares = workspace
            .strips
            .iter()
            .any(|strip| !strip.window_groups.is_empty());
        let Some(sealed) = self.sealed(engine, &key) else {
            return;
        };
        let (reached, revision, holds) = self.reached(engine, &key);
        // A record nobody wrote yet is founded by the first desk with a window in it: a device with
        // nothing to show founds nothing, so the record stays one the devices that do show
        // something make.
        if reached.is_none() && !holds && !shares {
            return;
        }
        self.lay(
            engine,
            person,
            revision,
            Save {
                room,
                workspace: workspace.clone(),
                local,
                kept: self.held(&key, |held| held.kept.take()),
                reached,
                written: self.held(&key, |held| held.written.clone()),
                own: tree.external_revision,
                notes: self.notes.of_workspace(room),
                sealed,
            },
        );
    }

    /// Hand `save` to the task that writes it, and take what it came to at once if it is there
    /// already.
    fn lay(&self, engine: &Arc<Engine>, person: &str, revision: Option<[u8; 32]>, save: Save) {
        let engine = Arc::clone(engine);
        let person = person.to_owned();
        let key = (save.room, Route::layout());
        let (own, notes) = (save.own, save.notes.clone());
        let closed = self.closed_since_written(save.room, &save.workspace);
        let writing = crate::engine_write::Polled::new(
            self.wake.handle(),
            async move { Box::pin(save.write(&engine, &person, &closed)).await },
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
        };
        let Some(laid) = laying.writing.done() else {
            self.held(&key, |held| {
                held.writing = Some(Going::Layout(Box::new(laying)));
            });
            return;
        };
        self.laid(&key, laid, &laying);
    }

    /// What a layout's write came to: the record this device writes on next, and what the person
    /// hears of it.
    fn laid(&self, key: &Key, laid: Laid, laying: &Laying) {
        let room = key.0;
        let written = |record: &automerge::AutoCommit| {
            Written::Layout(record.committed_heads(), laying.own, laying.notes.clone())
        };
        match laid {
            Laid::Held { record, windows } => {
                // Nothing to write: what this device read is the copy its next write is made on.
                let written = written(&record);
                self.written_windows.borrow_mut().insert(room, windows);
                self.held(key, |held| held.written = Some(written));
                self.keep(key, *record, laying.revision);
            }
            Laid::Wrote(taken) => match taken.took {
                Ok(_) => {
                    let written = written(&taken.record);
                    self.written_windows
                        .borrow_mut()
                        .insert(room, taken.windows);
                    self.held(key, |held| held.written = Some(written));
                    self.keep(key, *taken.record, laying.revision);
                }
                Err(error) => {
                    // The record is not the workspace's until it takes it: the next look writes it
                    // again, on what the workspace holds and not on the write it refused.
                    self.held(key, |held| {
                        held.written = None;
                        held.kept = None;
                        held.based = None;
                    });
                    self.say(&error, "the workspace's layout was not published", room);
                }
            },
            Laid::Failed(error, record) => {
                // What was refused is not written, and the look that found it is not one that wrote
                // it: the next look tries again.
                self.held(key, |held| {
                    held.written = None;
                    held.kept = record.map(|record| *record);
                });
                self.say(&error, "the workspace's layout was not kept", room);
            }
        }
    }

    /// Write the person here's view of every workspace whose view they changed into that
    /// workspace's record of it.
    fn capture_view(&self, engine: &Arc<Engine>, person: &str, tree: &DesktopTree) -> bool {
        let route = Route::view(person);
        let mut wrote = false;
        for workspace in &tree.workspaces {
            let room = workspace.id();
            if room.is_nil() {
                continue;
            }
            let key = (room, route.clone());
            let view = PersonView::of(tree, workspace.slot);
            let Some(view) = self.held(&key, |held| rested(held, &view)) else {
                continue;
            };
            self.held(&key, |held| {
                held.written = Some(Written::View(view.clone()));
                held.pending = None;
            });
            let Some(sealed) = self.sealed(engine, &key) else {
                // The record's keys are not here yet: it is written on the look after they arrive.
                self.held(&key, |held| {
                    held.read = false;
                    held.written = None;
                    held.pending = None;
                });
                continue;
            };
            let (reached, revision, _) = self.reached(engine, &key);
            let mut held = self.base(&key, reached);
            let from = held.get_heads();
            if let Err(error) = view.write(&mut held) {
                self.say(
                    &error.to_string(),
                    "the view of the layout was not kept",
                    room,
                );
                continue;
            }
            if held.pending_ops() == 0 {
                self.keep(&key, held, revision);
                continue;
            }
            wrote |= self.publish_view(
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
        let (room, route) = key;
        if engine.commits_documents(*room) {
            // Its host takes part in no channels: its records are documents the owner commits,
            // written in the clear.
            return Some(Sealing::Clear);
        }
        match engine.seal_keys(*room, route.app, &route.record) {
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
                self.say(&error, "the workspace's layout was not written", *room);
                None
            }
        }
    }

    /// Publish the view this device just wrote to the workspace, as the workspace's own copy of it:
    /// what the workspace takes is the view the next look writes on, and what it refuses is
    /// dropped, so the next look writes it again on what the workspace holds.
    /// A record of the workspace is written the way the workspace keeps them: sealed under the
    /// record channel's keys where they are its members' sealed channels, and in the clear where
    /// the workspace's device commits its documents. A sealed record starts sealed: the write that
    /// first holds anything of it is a checkpoint of the whole of it, and every write after it a
    /// sealed change. Members read a sealed record from its sealed history alone, so a record
    /// founded in the clear would leave them a genesis they cannot account for and a record they
    /// could never read.
    fn publish_view(
        &self,
        engine: &Arc<Engine>,
        key: &Key,
        mut candidate: automerge::AutoCommit,
        replaced: Option<Written>,
        heads: &[ChangeHash],
        revision: Option<[u8; 32]>,
        sealed: Sealing,
    ) -> bool {
        let (room, route) = key.clone();
        if replaced.is_none() {
            return false;
        }
        // Every change a device signs says what it is and who made it: the edit of the record this
        // person just made here.
        let author = self.person.borrow().clone();
        candidate.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(shared_record::change_message(author.as_deref())),
        );
        let changes = candidate.save_after(heads);
        // A write on no history is the whole record: the one that founds it.
        let founding = heads.is_empty();
        let (engine, record) = (Arc::clone(engine), route.record.clone());
        let mut writing = crate::engine_write::Writing::write(self.wake.handle(), async move {
            engine
                .write_record(
                    room,
                    (route.app, &record),
                    &changes,
                    founding,
                    sealed.keys(),
                )
                .await
        });
        if let Some(written) = writing.done() {
            self.view_published(key, written, candidate, replaced, revision);
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
        let mut idle = true;
        for key in self.keys() {
            let Some(going) = self.held(&key, |held| held.writing.take()) else {
                continue;
            };
            let going = match going {
                Going::View(mut publishing) => match publishing.writing.done() {
                    Some(written) => {
                        let publishing = *publishing;
                        self.view_published(
                            &key,
                            written,
                            publishing.candidate,
                            publishing.replaced,
                            publishing.revision,
                        );
                        continue;
                    }
                    None => Going::View(publishing),
                },
                Going::Layout(mut laying) => match laying.writing.done() {
                    Some(laid) => {
                        self.laid(&key, laid, &laying);
                        continue;
                    }
                    None => Going::Layout(laying),
                },
            };
            self.held(&key, |held| held.writing = Some(going));
            idle = false;
        }
        idle
    }

    /// What the workspace did with `candidate`, this device's write of the person's view of `key`'s
    /// record: what it took is the view the next look writes on, and what it refused is dropped.
    fn view_published(
        &self,
        key: &Key,
        written: crate::engine_write::Written,
        candidate: automerge::AutoCommit,
        replaced: Option<Written>,
        revision: Option<[u8; 32]>,
    ) {
        let room = key.0;
        match written {
            Ok(_write) => {
                let Some(written @ Written::View(_)) = replaced else {
                    return;
                };
                self.held(key, |held| held.written = Some(written));
                self.keep(key, candidate, revision);
            }
            Err(error) => {
                // The record is not the workspace's until it takes it: the next look writes it
                // again, on what the workspace holds and not on the write it refused.
                self.held(key, |held| {
                    held.written = None;
                    held.kept = None;
                    held.based = None;
                });
                self.say(&error, "the workspace's layout was not published", room);
            }
        }
    }

    /// Say why a workspace's record was not kept or published, for the caller to tell the person:
    /// once per reason, not on every frame.
    fn say(&self, error: &str, what: &str, room: WorkspaceId) {
        tracing::warn!(%error, %room, "{what}");
        if self.told.borrow().as_deref() != Some(error) {
            *self.refused.borrow_mut() = Some((room, error.to_owned()));
        }
    }

    /// The refusal the person has not heard yet, once: why what they changed on a workspace's
    /// layout did not reach the workspace.
    pub(crate) fn take_refusal(&self) -> Option<(WorkspaceId, String)> {
        let (room, refused) = self.refused.borrow_mut().take()?;
        *self.told.borrow_mut() = Some(refused.clone());
        Some((room, refused))
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
        self.held(key, |held| {
            held.kept = Some(record);
            if revision.is_some() {
                held.based = revision;
            }
        });
    }

    /// The record this device makes its write on: the one it last wrote and the workspace took,
    /// with what `reached` — the workspace's own copy of the record — holds merged in. A device
    /// writes a record the workspace has not handed back to it yet all the same, and its next
    /// change continues the one it made instead of numbering it again.
    fn base(&self, key: &Key, reached: Option<automerge::AutoCommit>) -> automerge::AutoCommit {
        let kept = self.held(key, |held| held.kept.clone());
        let Some(mut base) = kept else {
            return reached.unwrap_or_default();
        };
        if let Some(reached) = reached
            && let Err(error) = tilo_desktop_model::merge::merge_into(&mut base, &reached)
        {
            // What the workspace holds cannot be laid over our own write: start from it, and say so
            // once.
            self.say(
                &error.to_string(),
                "the record was not laid together",
                key.0,
            );
            return reached;
        }
        base
    }
}

/// What this device writes the layout on: the record as it last wrote and the workspace took it,
/// with everything the workspace holds since laid over it.
fn base_of(
    room: WorkspaceId,
    kept: Option<automerge::AutoCommit>,
    reached: Option<automerge::AutoCommit>,
) -> automerge::AutoCommit {
    match (kept, reached) {
        (Some(mut base), Some(reached)) => {
            match tilo_desktop_model::merge::merge_into(&mut base, &reached) {
                Ok(_applied) => base,
                Err(error) => {
                    // What the workspace holds cannot be laid over our own write: start from it.
                    tracing::warn!(%error, %room, "the record was not laid together");
                    reached
                }
            }
        }
        (Some(base), None) => base,
        (None, reached) => reached.unwrap_or_default(),
    }
}

/// The record laid into `workspace`, this device's desk, with the windows it closed since its last
/// write left out: what this device's write holds and what the layout takes from it.
fn laid_into(
    record: &automerge::AutoCommit,
    workspace: Workspace,
    closed: &BTreeSet<u64>,
) -> Workspace {
    let mut laid_out = workspace;
    device::fold(
        record,
        &mut laid_out,
        "",
        &device::Kept {
            mine: &|_| true,
            private: &|_| true,
            closed: &|id| closed.contains(&id),
            writing: true,
        },
    );
    laid_out
}

/// The view to write into `key`'s record now, if any: what a look writes only once it rests, and
/// never while a write of it is under way or before this device has read the record. A view that
/// did not change is not written again.
fn rested(held: &mut Held, view: &PersonView) -> Option<PersonView> {
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

#[cfg(test)]
mod tests {
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
        assert_eq!(rested(&mut held, &written), None);
    }

    /// A view is written once it rests, not on the look that found it move: a drag or a burst of
    /// switches changes it on every look and is one write when it is over.
    #[test]
    fn a_view_is_written_once_it_rests() {
        let mut held = Held {
            read: true,
            ..Held::default()
        };
        assert_eq!(rested(&mut held, &view(11)), None, "the look that found it");
        assert_eq!(
            rested(&mut held, &view(11)),
            Some(view(11)),
            "the look after it, unchanged"
        );
        assert_eq!(
            rested(&mut held, &view(12)),
            None,
            "a look that moved it again writes nothing yet"
        );
        assert_eq!(rested(&mut held, &view(12)), Some(view(12)));
    }

    /// A write of the view under way holds the look back, and a record this device has not read is
    /// not written over.
    #[test]
    fn a_view_is_written_only_between_writes_and_after_a_read() {
        let mut held = Held::default();
        assert_eq!(
            rested(&mut held, &view(11)),
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
        assert_eq!(rested(&mut held, &view(11)), None, "a write is under way");
        held.writing = None;
        let _ = rested(&mut held, &view(11));
        assert_eq!(rested(&mut held, &view(11)), Some(view(11)));
    }

    /// The record and what a look of the layout holds about it.
    fn laying() -> Laying {
        let unused = || Laid::Failed("the test writes nothing".to_owned(), None);
        Laying {
            writing: crate::engine_write::Polled::new(
                std::sync::Arc::new(|| {}) as tilo_service::Wake,
                async { Laid::Failed("the test writes nothing".to_owned(), None) },
                unused,
            ),
            own: 7,
            notes: vec![(3, "so.tilo.notes".to_owned())],
            revision: Some([1; 32]),
        }
    }

    fn room() -> WorkspaceId {
        WorkspaceId::parse("00000000-0000-4000-8000-000000000042").expect("an id")
    }

    /// A record that differs from this desk keeps the record the task built and the windows of this
    /// desk it holds, so the look after it writes on that record.
    #[test]
    fn a_written_layout_becomes_the_record_the_next_look_writes_on() {
        let desktops = SharedDesktops::default();
        let key = (room(), Route::layout());
        let mut record = automerge::AutoCommit::new();
        record
            .put(automerge::ROOT, "corner", 1u64)
            .expect("a record");
        record.commit();
        let heads = record.committed_heads();
        desktops.laid(
            &key,
            Laid::Held {
                record: Box::new(record.clone()),
                windows: [3, 4].into_iter().collect(),
            },
            &laying(),
        );
        let written = desktops.held(&key, |held| held.written.clone());
        assert!(
            written
                == Some(Written::Layout(
                    heads,
                    7,
                    vec![(3, "so.tilo.notes".to_owned())]
                )),
            "the look that found nothing to write is a look that wrote this record"
        );
        assert_eq!(
            desktops.written_windows.borrow().get(&room()),
            Some(&[3, 4].into_iter().collect()),
            "the windows of this desk the record holds"
        );
        assert!(desktops.held(&key, |held| held.kept.is_some()));
    }

    /// A save that failed is written again by the next look, on the record it read, and the person
    /// hears why once.
    #[test]
    fn a_layout_that_could_not_be_written_is_written_again() {
        let desktops = SharedDesktops::default();
        let key = (room(), Route::layout());
        desktops.laid(
            &key,
            Laid::Failed(
                "the record refused the desk".to_owned(),
                Some(Box::new(automerge::AutoCommit::new())),
            ),
            &laying(),
        );
        assert!(
            desktops.held(&key, |held| held.written.clone()).is_none(),
            "what was not written is written again"
        );
        assert!(desktops.held(&key, |held| held.kept.is_some()));
        assert_eq!(
            desktops.take_refusal(),
            Some((room(), "the record refused the desk".to_owned()))
        );
    }

    /// A write of the layout under way holds the session open, and the look that finds it under way
    /// starts no second one: the desk the person changes meanwhile is written after it.
    #[test]
    fn a_layout_write_under_way_holds_the_next_one_back() {
        let desktops = SharedDesktops::default();
        let key = (room(), Route::layout());
        desktops.held(&key, |held| {
            held.read = true;
            held.writing = Some(Going::Layout(Box::new(Laying {
                writing: crate::engine_write::Polled::new(
                    std::sync::Arc::new(|| {}) as tilo_service::Wake,
                    async { std::future::pending::<Laid>().await },
                    || Laid::Failed("the test's write never answers".to_owned(), None),
                ),
                own: 7,
                notes: Vec::new(),
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
}
