//! The workspace's shared desktop, as every device in it shows it: its strips and windows are the
//! workspace's, written by its members into one record (`so.tilo.browser-desktop`'s `desktop` ),
//! and each device that shows the workspace puts what the record holds on its own desk
//! ([`tilo_desktop_model::shared_record`]).
//! The readers and the writer are a page's: a browser in the workspace folds the record into its
//! desk and writes the changes its person makes back. A native session shows the same workspace
//! through the same record, so a person joining a workspace from a page and one joining it from a
//! desktop see one desk. What is this device's alone — which strip it shows, how wide its windows
//! are, which window is in front — is never written, and folding keeps it
//! ([`tilo_desktop_model::shared_record::device::Kept`]).

// Nothing compiled for the web reads this module's roles yet: the page's session wires them with
// its own host.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page's session reads these with its own wiring"
    )
)]
//! The record is read from the engine's copy of the workspace, which admits every member's writes
//! as its governance lets them and holds what they wrote before this device joined. Nothing here
//! decides whether a write may land: that is the workspace's, decided where the engine takes it in.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::ChangeHash;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
use tilo_desktop_model::shared_record::{self, device};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, peer_salt_of};

use tilo_broker_core::custody::OwnerCustody as _;

use crate::engine::{Engine, SealKeys};
use crate::sync::Client as Sync;

/// What a workspace's record held when this device last wrote it, how far this desk had moved by
/// then, and what its windows showed: a look that finds all three again writes nothing.
type Written = (Vec<ChangeHash>, u64, Vec<(u64, String)>);

/// This session's reading of the workspaces' shared desktops: what each record held when the desk
/// was last laid out from it, and what this device last wrote into it, so a record that did not
/// move is not read or written again.
#[derive(Default)]
pub(crate) struct SharedDesktops {
    /// What each of this device's windows shows, as its app reports it: the record noted beside the
    /// window in the workspace's layout, so the person's other devices hold it and can continue the
    /// window there.
    notes: std::sync::Arc<crate::continuations::Continuations>,
    /// The heads each workspace's record was folded in at, by workspace.
    /// The agent run that opened each window an agent opened, by workspace and tab, as each
    /// workspace's record last said ([`Self::opened_by`]).
    opened_by:
        RefCell<HashMap<WorkspaceId, BTreeMap<u64, tilo_desktop_model::ownership::OpenedBy>>>,
    folded: RefCell<HashMap<WorkspaceId, Vec<ChangeHash>>>,
    /// The revision each workspace's record was last captured at: the next read asks for the record
    /// against it, so a record that did not move between passes is answered from the admitted heads
    /// rather than captured and saved again. A desk is read every pass, and a document saved for
    /// each read cost what the workspace's whole history weighs, every time.
    revision: RefCell<HashMap<WorkspaceId, [u8; 32]>>,
    /// What each workspace's record held when this device last wrote it, how far this desk had
    /// moved by then, and what its windows showed: a look that finds all three again writes
    /// nothing, and one that finds any of them moved writes.
    written: RefCell<HashMap<WorkspaceId, Written>>,
    /// Each workspace's record as this device last wrote it and the workspace took it: what the
    /// next write is made on. The engine's copy of the record reaches a member after the workspace
    /// admits the write, so it can lag the write this device already made; writing on that lagging
    /// copy would number a second change the same as the first, and the workspace would refuse it.
    kept: RefCell<HashMap<WorkspaceId, automerge::AutoCommit>>,
    /// The revision of the workspace's record each `kept` copy holds: the next write reads the
    /// record against it, so a record that did not move since is answered from the admitted heads.
    /// A device that keeps no copy (a session started again) reads the record whole: writing on
    /// nothing would found the record again and number its writer's sequence from the start, which
    /// the workspace refuses as a sequence it already holds.
    based: RefCell<HashMap<WorkspaceId, [u8; 32]>>,
    /// The windows each workspace's record holds of this device's desk, as it last wrote them: a
    /// window there that this desk no longer holds is one this device closed, and its entry goes
    /// when the device writes again rather than the fold laying it back into the desk, where no
    /// window fills it.
    written_windows: RefCell<HashMap<WorkspaceId, BTreeSet<u64>>>,
    /// The windows this device's desk shows that the workspace's record does not hold yet: what it
    /// opened since the last write, and what the next write adds.
    unrecorded: RefCell<BTreeSet<u64>>,
    /// Every workspace whose record this device has read: while each of the workspaces it holds is
    /// among them and neither the engine's revision nor this desk's own moved since, there is
    /// nothing to read again.
    read: RefCell<BTreeSet<WorkspaceId>>,
    /// The engine's revision and this desk's own at the last look: what this
    /// device changed on the desk itself moves nothing the engine published,
    /// and is written back on the pass that sees it.
    seen: Cell<Option<(u64, u64)>>,
    /// A failure to read or write a record that the person has not heard yet: the next look at the
    /// desk says it, once per reason, not on every frame.
    refused: RefCell<Option<(WorkspaceId, String)>>,
    /// The last failure the person heard: the same one again is not said twice.
    told: RefCell<Option<String>>,
}

impl SharedDesktops {
    /// This session's shared desktops, writing what its windows note of themselves (`notes`).
    pub(crate) fn new(notes: std::sync::Arc<crate::continuations::Continuations>) -> Self {
        Self {
            notes,
            ..Self::default()
        }
    }

    /// The windows this device's desk has that the record of `room` holds and this desk no longer
    /// does: the close it made since it last wrote the record.
    fn closed_since_written(&self, room: WorkspaceId, workspace: &Workspace) -> BTreeSet<u64> {
        let here = windows_of(workspace);
        self.written_windows
            .borrow()
            .get(&room)
            .map(|written| written.difference(&here).copied().collect())
            .unwrap_or_default()
    }

    /// The record `held` laid into this device's desk, with the windows it closed since its last
    /// write left out: what this device's write holds and what the record takes from it.
    fn laid_into(
        &self,
        room: WorkspaceId,
        held: &automerge::AutoCommit,
        workspace: &Workspace,
    ) -> Workspace {
        let closed = self.closed_since_written(room, workspace);
        let mut laid_out = workspace.clone();
        device::fold(
            held,
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

    /// Whether every workspace `tree` holds has had its record read here, and neither the engine at
    /// revision `revision` nor this desk has moved since: nothing of the workspaces' desks can have
    /// changed, so no record is read or written again. Notes what was looked at.
    pub(crate) fn settled(&self, revision: u64, tree: &DesktopTree) -> bool {
        let read = self.read.borrow();
        let settled = self.seen.get() == Some((revision, tree.external_revision))
            && tree
                .workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil())
                .all(|workspace| read.contains(&workspace.id()));
        self.seen.set(Some((revision, tree.external_revision)));
        settled
    }

    /// `workspace` 's shared desktop record, read from the engine's copy of the workspace: what the
    /// workspace last admitted, what it held before this device joined included. `Ok(None)` for a
    /// workspace no one wrote the desk of yet.
    fn record(
        engine: &Engine,
        workspace: WorkspaceId,
        known: Option<[u8; 32]>,
    ) -> Result<Option<(automerge::AutoCommit, [u8; 32])>, String> {
        let Some((bytes, revision)) =
            engine.record_since(&workspace.to_string(), DESKTOP_APP, DESKTOP_RECORD, known)?
        else {
            return Ok(None);
        };
        automerge::AutoCommit::load(&bytes)
            .map(|doc| Some((doc, revision)))
            .map_err(|error| error.to_string())
    }

    /// Fold every workspace's shared desktop into `tree` : its strips, groups and windows where the
    /// record says, and what this device decides alone kept. `device` is this device's publisher,
    /// which resolves a window that runs here to the window it shows here. Returns whether anything
    /// changed.
    pub(crate) fn follow(&self, engine: &Engine, device: &str, tree: &mut DesktopTree) -> bool {
        let local = tree.salt;
        let mut folded = self.folded.borrow_mut();
        let mut read = self.read.borrow_mut();
        let mut revisions = self.revision.borrow_mut();
        let mut changed = false;
        for workspace in &mut tree.workspaces {
            let room = workspace.id();
            if room.is_nil() {
                continue;
            }
            let Some((mut doc, revision)) =
                Self::record(engine, room, revisions.get(&room).copied())
                    .ok()
                    .flatten()
            else {
                // A workspace this device holds no record of (not its own,
                // or one still arriving) says nothing of its desk yet: its
                // record is read again next pass.
                read.remove(&room);
                revisions.remove(&room);
                continue;
            };
            read.insert(room);
            revisions.insert(room, revision);
            let heads = doc.get_heads();
            if folded.get(&room) == Some(&heads) {
                continue;
            }
            folded.insert(room, heads);
            let Some(desk) = shared_record::read(&doc, None) else {
                // A record that holds no desktop yet: nothing of it to show.
                continue;
            };
            self.opened_by.borrow_mut().insert(room, desk.opened_by);
            let mine = |id: u64| local != 0 && peer_salt_of(id) == local;
            let closed = self.closed_since_written(room, workspace);
            let kept = device::Kept {
                // A strip this device made and left empty stays on its own desk: the record never
                // holds an empty strip it made, and an empty row arriving here is nobody else's to
                // see.
                mine: &mine,
                private: &|_: u64| false,
                // A window this device closed is not laid back into its desk from the record: the
                // write that follows takes its entry out instead.
                closed: &|id| closed.contains(&id),
                writing: false,
            };
            changed |= device::fold(&doc, workspace, device, &kept);
        }
        changed
    }

    /// The agent run that opened each window an agent opened, by tab, across the workspaces this
    /// device shows: what each window's bar names ([`tilo_desktop_surface::DesktopHandle::set_opened_by`]).
    pub(crate) fn opened_by(&self) -> BTreeMap<u64, tilo_desktop_model::ownership::OpenedBy> {
        self.opened_by
            .borrow()
            .values()
            .flat_map(|runs| runs.iter().map(|(tab, run)| (*tab, run.clone())))
            .collect()
    }

    /// Write what this device changed about the workspace in front's desk into the workspace's
    /// shared desktop record, and publish it to the workspace's other devices: the strips and
    /// windows it makes and arranges, as the person here. A workspace this device has no person
    /// for, and one whose governor refuses the write, writes nothing. Returns whether it wrote.
    pub(crate) fn capture(
        &self,
        engine: &std::sync::Arc<Engine>,
        sync: Option<&Sync>,
        tree: &DesktopTree,
    ) -> bool {
        let Some(workspace) = tree.workspaces.iter().find(|held| held.slot == tree.active) else {
            return false;
        };
        let room = workspace.id();
        let local = tree.salt;
        if room.is_nil() || local == 0 {
            return false;
        }
        let Some(person) = sync.and_then(|sync| sync.own().user) else {
            return false;
        };
        // What this device's desk has to share: its windows. A workspace it shows with none is a
        // desk it keeps to itself.
        let shares = workspace
            .strips
            .iter()
            .any(|strip| !strip.window_groups.is_empty());
        // What this device seals the write with, decided before any of it is made: a workspace
        // whose records are its members' sealed channels takes a sealed write, and one whose
        // channel keys this device holds none of — because they are not here yet, or because this
        // device has no secret store at all — takes none. Neither is a workspace's verdict, so no
        // notice names a workspace as having refused a write it never saw.
        let sealed = if engine.commits_documents(room) {
            // Its host takes part in no channels: its records are documents the owner commits,
            // written in the clear.
            None
        } else {
            match engine.seal_keys(room, DESKTOP_APP, DESKTOP_RECORD) {
                Ok(SealKeys::Here(keys)) => Some(keys),
                // The record's channel keys are not here yet: the desk is written on the look after
                // they arrive, and this look leaves the workspace unread so that there is one.
                Ok(SealKeys::Waiting) => {
                    self.read.borrow_mut().remove(&room);
                    return false;
                }
                // This device holds no secret store, so it holds no channel
                // keys of any workspace: its desks stay the ones it keeps,
                // and the person hears nothing. Decided here, before the desk
                // is read, folded or written, so a look costs one lookup; a
                // look after a store opens here writes the desk as any other.
                Ok(SealKeys::NoSecretStore) => return false,
                Err(error) => {
                    self.say(
                        &error,
                        "the workspace's shared desktop was not written",
                        room,
                    );
                    return false;
                }
            }
        };
        let (reached, revision, holds) = self.reached(engine, room);
        // A record nobody wrote yet is founded by the first desk with a window in it: a device with
        // nothing to show founds nothing, so the record stays one the devices that do show
        // something make.
        if reached.is_none() && !holds && !shares {
            return false;
        }
        // What this device writes on: the record as it last wrote and the workspace took it, with
        // everything the workspace holds since laid over it. A workspace whose record nobody wrote
        // yet starts from the empty record, and this device's desk is what founds it.
        let mut held = self.base(room, reached);
        let heads = held.get_heads();
        let own = tree.external_revision;
        let notes = self.notes.of_workspace(room);
        let unchanged =
            self.written.borrow().get(&room) == Some(&(heads.clone(), own, notes.clone()));
        if unchanged {
            return false;
        }
        // The record is every member's: what this device writes into it goes in as this person on
        // this device, never under the actor the copy here was last written by, which two devices
        // writing at once would number the same.
        held.set_actor(automerge::ActorId::from(
            ui::history_source::authors::device_actor(
                &person,
                &automerge::ActorId::from(engine.identity().device_cert.device_public_key.to_vec()),
            ),
        ));
        // Fold the record into this desk's workspace, and lay this device's own change on top: what
        // the record holds and what this device's desk holds are then one change, and nothing
        // anyone else moved is undone by it. This device's own layout stays: it is what the write
        // adds, and a fold that dropped it would write nothing at all.
        let laid_out = self.laid_into(room, &held, workspace);
        let mine = |id: u64| peer_salt_of(id) == local || self.is_unrecorded(id);
        let by_tab: BTreeMap<u64, String> = notes.iter().cloned().collect();
        let records = |id: u64| by_tab.get(&id).cloned();
        let mut candidate = held.clone();
        let wrote = device::write(
            &mut candidate,
            room,
            &laid_out,
            &device::Writer {
                person: Some(&person),
                mine: &mine,
                retained: &|_: u64| false,
                carried: device::Writer::NOTHING_CARRIED,
                records: &records,
            },
        );
        if wrote.is_ok() {
            // What the record now holds of this desk: the windows it showed when it wrote. A window
            // only the record holds is not on this desk yet, and the fold lays it in rather than
            // reading it as one this device closed.
            let written = &windows_of(&laid_out) & &windows_of(workspace);
            self.written_windows.borrow_mut().insert(room, written);
        }
        match wrote {
            Ok(false) => {
                self.written.borrow_mut().insert(room, (heads, own, notes));
                // Nothing to write: what this device read is the copy its next write is made on.
                self.keep(room, held, revision);
                false
            }
            Err(error) => {
                // What the record refused is not written, and the look that found it is not one
                // that wrote it: the next look tries again.
                self.written.borrow_mut().remove(&room);
                self.say(
                    &error.to_string(),
                    "the workspace's shared desktop was not kept",
                    room,
                );
                false
            }
            Ok(true) => self.publish(
                engine,
                room,
                &person,
                candidate,
                ((heads, own, notes), revision),
                sealed.as_ref(),
            ),
        }
    }

    /// Publish the record this device just wrote to the workspace, as the workspace's own copy of
    /// it: what the workspace takes is the record the next look writes on, and what it refuses is
    /// dropped, so the next look writes it again on what the workspace holds. The heads, desk
    /// revision and notes are what this write replaced, as the look notes it, and the revision the
    /// record was read at is what the kept copy now holds.
    /// The desk is one of the workspace's records, so it is written the way the workspace keeps
    /// them: sealed under the record channel's keys where they are its members' sealed channels,
    /// and in the clear where the workspace's device commits its documents. A sealed record starts
    /// sealed: the write that first holds anything of the record is a checkpoint of the whole of
    /// it, and every write after it a sealed change. Members read a sealed record from its sealed
    /// history alone, so a record founded in the clear would leave them a genesis they cannot
    /// account for and a desk they could never read.
    fn publish(
        &self,
        engine: &Engine,
        room: WorkspaceId,
        person: &str,
        mut candidate: automerge::AutoCommit,
        ((heads, own, notes), revision): (Written, Option<[u8; 32]>),
        sealed: Option<&channel_crypto::ChannelContentKeys>,
    ) -> bool {
        // Every change a device signs says what it is and who made it: the edit of the record this
        // person just made here.
        candidate.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(shared_record::change_message(Some(person))),
        );
        let changes = candidate.save_after(&heads);
        // A write on no history is the whole record: the one that founds it.
        let committed = engine.write_record(
            room,
            (DESKTOP_APP, DESKTOP_RECORD),
            &changes,
            heads.is_empty(),
            sealed,
        );
        match committed {
            Ok(_write) => {
                // What the workspace took is the record this device wrote: the next look at the
                // heads it now holds, this desk where it stands, and these notes, writes nothing.
                self.written
                    .borrow_mut()
                    .insert(room, (candidate.get_heads(), own, notes));
                self.keep(room, candidate, revision);
                true
            }
            Err(error) => {
                // The record is not the workspace's until it takes it: the next look writes it
                // again, on what the workspace holds and not on the write it refused.
                self.written.borrow_mut().remove(&room);
                self.kept.borrow_mut().remove(&room);
                self.based.borrow_mut().remove(&room);
                self.say(
                    &error,
                    "the workspace's shared desktop was not published",
                    room,
                );
                false
            }
        }
    }

    /// Say why a workspace's shared desktop was not kept or published, for the caller to tell the
    /// person: once per reason, not on every frame.
    fn say(&self, error: &str, what: &str, room: WorkspaceId) {
        tracing::warn!(%error, %room, "{what}");
        if self.told.borrow().as_deref() != Some(error) {
            *self.refused.borrow_mut() = Some((room, error.to_owned()));
        }
    }

    /// The refusal the person has not heard yet, once: why what they changed on a workspace's
    /// shared desk did not reach the workspace.
    pub(crate) fn take_refusal(&self) -> Option<(WorkspaceId, String)> {
        let (room, refused) = self.refused.borrow_mut().take()?;
        *self.told.borrow_mut() = Some(refused.clone());
        Some((room, refused))
    }

    /// What the workspace's copy of `room`'s record holds here, read against the revision this
    /// device's own copy holds, the revision it was read at, and whether this device holds a copy.
    /// A record this device was never sent, or cannot read, is one it holds nothing of, and one
    /// that did not move since its copy is the copy. Not the revision the desk was last folded at:
    /// a record folded, but never copied here (a session started again), would read as unchanged
    /// and leave this device writing on nothing.
    fn reached(
        &self,
        engine: &Engine,
        room: WorkspaceId,
    ) -> (Option<automerge::AutoCommit>, Option<[u8; 32]>, bool) {
        let holds = self.kept.borrow().contains_key(&room);
        let known = holds
            .then(|| self.based.borrow().get(&room).copied())
            .flatten();
        let read = Self::record(engine, room, known).unwrap_or(None);
        let revision = read.as_ref().map(|(_, revision)| *revision);
        (read.map(|(doc, _)| doc), revision, holds)
    }

    /// Keep `record` as this device's copy of `room`'s record, read at `revision` when this look
    /// read it; one read against an unchanged revision keeps the revision it already had.
    fn keep(&self, room: WorkspaceId, record: automerge::AutoCommit, revision: Option<[u8; 32]>) {
        self.kept.borrow_mut().insert(room, record);
        if let Some(revision) = revision {
            self.based.borrow_mut().insert(room, revision);
        }
    }

    /// The record this device makes its write on: the one it last wrote and the workspace took,
    /// with what `reached` — the workspace's own copy of the record — holds merged in. A device
    /// writes a record the workspace has not handed back to it yet all the same, and its next
    /// change continues the one it made instead of numbering it again.
    fn base(
        &self,
        room: WorkspaceId,
        reached: Option<automerge::AutoCommit>,
    ) -> automerge::AutoCommit {
        let kept = self.kept.borrow().get(&room).cloned();
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
                "the shared desktop was not laid together",
                room,
            );
            return reached;
        }
        base
    }

    /// Every window the desk of `tree` shows, this device's own, that a write made since the fold
    /// would add to the record: what [`tilo_desktop_model::shared_record::device::Writer::mine`]
    /// has to name as the record's next write.
    pub(crate) fn note_unrecorded(&self, engine: &Engine, tree: &DesktopTree) {
        let Some(workspace) = tree.workspaces.iter().find(|held| held.slot == tree.active) else {
            return;
        };
        let room = workspace.id();
        let Some(doc) = Self::record(engine, room, self.revision.borrow().get(&room).copied())
            .ok()
            .flatten()
            .map(|(doc, _)| doc)
        else {
            return;
        };
        let Some(shared) = shared_record::read(&doc, None) else {
            return;
        };
        let held: BTreeSet<u64> = shared
            .tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        let mut unrecorded = self.unrecorded.borrow_mut();
        unrecorded.clear();
        for tab in tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
        {
            if !held.contains(&tab.id) && peer_salt_of(tab.id) == tree.salt {
                unrecorded.insert(tab.id);
            }
        }
    }

    /// Whether `tab` is one this device opened and the record does not hold yet: what makes it this
    /// device's to add there.
    #[must_use]
    pub(crate) fn is_unrecorded(&self, tab: u64) -> bool {
        self.unrecorded.borrow().contains(&tab)
    }
}

/// Every window the desk of `workspace` shows.
fn windows_of(workspace: &Workspace) -> BTreeSet<u64> {
    workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.id)
        .collect()
}
