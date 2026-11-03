//! The workspace's shared desktop, as every device in it shows it: its
//! strips and windows are the workspace's, written by its members into one
//! record (`so.tilo.browser-desktop`'s `desktop`), and each device that
//! shows the workspace puts what the record holds on its own desk
//! ([`tilo_desktop_model::shared_record`]).
//!
//! The readers and the writer are a page's: a browser in the workspace folds
//! the record into its desk and writes the changes its person makes back.
//! A native session shows the same workspace through the same record, so a
//! person joining a workspace from a page and one joining it from a desktop
//! see one desk. What is this device's alone — which strip it shows, how
//! wide its windows are, which window is in front — is never written, and
//! folding keeps it ([`tilo_desktop_model::shared_record::device::Kept`]).
//!
//! The record is read from the engine's copy of the workspace, which admits
//! every member's writes as its governance lets them and holds what they
//! wrote before this device joined. Nothing here decides whether a write may
//! land: that is the workspace's, decided where the engine takes it in.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};

use automerge::ChangeHash;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
use tilo_desktop_model::shared_record::{self, device};
use tilo_desktop_model::{DesktopTree, WorkspaceId, peer_salt_of};

use tilo_action_broker::owner_commits::OwnerCustody as _;

use crate::engine::Engine;
use crate::sync::Client as Sync;

/// This session's reading of the workspaces' shared desktops: what each
/// record held when the desk was last laid out from it, and what this device
/// last wrote into it, so a record that did not move is not read or written
/// again.
#[derive(Default)]
pub(crate) struct SharedDesktops {
    /// The heads each workspace's record was folded in at, by workspace.
    folded: RefCell<HashMap<WorkspaceId, Vec<ChangeHash>>>,
    /// The heads each workspace's record had when this device last looked at
    /// it to write: a look that finds them again writes nothing.
    written: RefCell<HashMap<WorkspaceId, Vec<ChangeHash>>>,
    /// The windows this device's desk shows that the workspace's record does
    /// not hold yet: what it opened since the last write, and what the next
    /// write adds.
    unrecorded: RefCell<BTreeSet<u64>>,
    /// Every workspace whose record this device has read: while each of the
    /// workspaces it holds is among them and none of the engine's revision
    /// moved since, there is nothing to read again.
    read: RefCell<BTreeSet<WorkspaceId>>,
    /// The engine revision this device last read the records at.
    seen: Cell<Option<u64>>,
    /// A failure to read or write a record is said once, not on every frame.
    warned: Cell<bool>,
}

impl SharedDesktops {
    /// Whether every workspace `tree` holds has had its record read here, and
    /// the engine at revision `revision` has not published since: nothing of
    /// the workspaces' desks can have moved, so no record is read again.
    /// Notes `revision` as the one read at.
    pub(crate) fn settled(&self, revision: u64, tree: &DesktopTree) -> bool {
        let read = self.read.borrow();
        let settled = self.seen.get() == Some(revision)
            && tree
                .workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil())
                .all(|workspace| read.contains(&workspace.id()));
        self.seen.set(Some(revision));
        settled
    }

    /// `workspace`'s shared desktop record, read from the engine's copy of
    /// the workspace: what the workspace last admitted, what it held before
    /// this device joined included.
    fn record(engine: &Engine, workspace: WorkspaceId) -> Option<automerge::AutoCommit> {
        let bytes = engine
            .record(&workspace.to_string(), DESKTOP_APP, DESKTOP_RECORD)
            .ok()??;
        automerge::AutoCommit::load(&bytes).ok()
    }

    /// Fold every workspace's shared desktop into `tree`: its strips, groups
    /// and windows where the record says, and what this device decides alone
    /// kept. `device` is this device's publisher, which resolves a window that
    /// runs here to the window it shows here. Returns whether anything
    /// changed.
    pub(crate) fn follow(&self, engine: &Engine, device: &str, tree: &mut DesktopTree) -> bool {
        let local = tree.salt;
        let mut folded = self.folded.borrow_mut();
        let mut read = self.read.borrow_mut();
        let mut changed = false;
        for workspace in &mut tree.workspaces {
            let room = workspace.id();
            if room.is_nil() {
                continue;
            }
            let Some(mut doc) = Self::record(engine, room) else {
                // A workspace this device holds no record of (not its own,
                // or one still arriving) says nothing of its desk yet: its
                // record is read again next pass.
                read.remove(&room);
                continue;
            };
            read.insert(room);
            let heads = doc.get_heads();
            if folded.get(&room) == Some(&heads) {
                continue;
            }
            folded.insert(room, heads);
            if shared_record::read(&doc, None).is_none() {
                // A record that holds no desktop yet: nothing of it to show.
                continue;
            }
            let mine = |id: u64| local != 0 && peer_salt_of(id) == local;
            let kept = device::Kept {
                // A strip this device made and left empty stays on its own
                // desk: the record never holds an empty strip it made, and an
                // empty row arriving here is nobody else's to see.
                mine: &mine,
                private: &|_: u64| false,
            };
            changed |= device::fold(&doc, workspace, device, &kept);
        }
        changed
    }

    /// Write what this device changed about the workspace in front's desk into
    /// the workspace's shared desktop record, and publish it to the
    /// workspace's other devices: the strips and windows it makes and
    /// arranges, as the person here. A workspace this device has no person
    /// for, and one whose governor refuses the write, writes nothing.
    /// Returns whether it wrote.
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
        let Some(mut held) = Self::record(engine, room) else {
            return false;
        };
        let mut written = self.written.borrow_mut();
        let heads = held.get_heads();
        if written.get(&room) == Some(&heads) {
            return false;
        }
        // Fold the record into this desk's workspace, and lay this device's
        // own change on top: what the record holds and what this device's
        // desk holds are then one change, and nothing anyone else moved is
        // undone by it.
        let mut laid_out = workspace.clone();
        device::fold(&held, &mut laid_out, "", &device::Kept::NOTHING);
        let mine = |id: u64| peer_salt_of(id) == local || self.is_unrecorded(id);
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
            },
        );
        match wrote {
            Ok(false) => {
                written.insert(room, heads);
                false
            }
            Err(error) => {
                if !self.warned.replace(true) {
                    tracing::warn!(
                        %error,
                        %room,
                        "the workspace's shared desktop was not kept"
                    );
                }
                false
            }
            Ok(true) => {
                let changes = candidate.save_after(&heads);
                written.insert(room, candidate.get_heads());
                let key = engine.identity().device_signing_key.clone();
                let committed = engine.owner().commit_document_with_current(
                    room,
                    |view| {
                        let message = view.peer().clone().publication_mut().publish_incremental(
                            DESKTOP_APP,
                            DESKTOP_RECORD,
                            changes.clone(),
                            &key,
                            std::time::SystemTime::now(),
                        )?;
                        let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = message
                        else {
                            return Err("a desktop write is not a document write".to_owned());
                        };
                        Ok(tilo_peer_state::owner_commit::OwnerCommitRequest {
                            workspace: room.to_string(),
                            operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                                app: DESKTOP_APP.to_owned(),
                                record: DESKTOP_RECORD.to_owned(),
                                envelope,
                            },
                        })
                    },
                    std::time::SystemTime::now(),
                );
                match committed {
                    Ok(_receipt) => {
                        // What was just committed is this device's outgoing
                        // custody: the workspace's other devices read it as
                        // any member's change.
                        engine.schedule_publication(room);
                        true
                    }
                    Err(error) => {
                        if !self.warned.replace(true) {
                            tracing::warn!(
                                %error,
                                %room,
                                "the workspace's shared desktop was not published"
                            );
                        }
                        false
                    }
                }
            }
        }
    }

    /// Every window the desk of `tree` shows, this device's own, that a
    /// write made since the fold would add to the record: what
    /// [`tilo_desktop_model::shared_record::device::Writer::mine`] has to
    /// name as the record's next write.
    pub(crate) fn note_unrecorded(&self, engine: &Engine, tree: &DesktopTree) {
        let Some(workspace) = tree.workspaces.iter().find(|held| held.slot == tree.active) else {
            return;
        };
        let room = workspace.id();
        let Some(doc) = Self::record(engine, room) else {
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

    /// Whether `tab` is one this device opened and the record does not hold
    /// yet: what makes it this device's to add there.
    #[must_use]
    pub(crate) fn is_unrecorded(&self, tab: u64) -> bool {
        self.unrecorded.borrow().contains(&tab)
    }
}
