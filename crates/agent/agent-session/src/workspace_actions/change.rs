//! What a watch of a workspace follows, and where it is woken: a write landing in the
//! workspace's replica answers each waiting watch whose caller may read what it landed in
//! ([`Worker::wake_watchers`]), and who is in the workspace moves with its
//! governance, which is one of its documents. A caller asks for such a wait across the wire
//! ([`tilo_actions::remote::ActionProvider::changed`]); the device that holds the workspace holds its
//! answer here until a change comes or the wait runs out.
use super::{
    ActionCallError, Arc, Caller, Completer, DocumentRoute, Event, HashSet, Instant,
    MAX_REMOTE_WAIT, Needs, Worker, Workspace, WorkspaceDocuments, WorkspaceId,
};

/// One caller waiting for a change of what it follows in a workspace ([`Worker::watch`]).
struct ChangeWait {
    until: Instant,
    /// Who waits: a write wakes them only where it landed in what they may read.
    caller: Caller,
    done: Completer<Result<(), ActionCallError>>,
}

/// Who waits for a change of one workspace, and when each wait ends ([`MAX_REMOTE_WAIT`] after it
/// began). One landing answers both what a watch reads of the documents and of who is in the
/// workspace: the roster its governance names is one of its records, and the caller reads what it
/// follows again either way. Whom a landing answers is decided where the workspace's policy is
/// at hand ([`Worker::wake_watchers`]): until then, where the writes landed is kept here.
#[derive(Default)]
pub(super) struct Watchers {
    waiting: Vec<ChangeWait>,
    landed: Vec<DocumentRoute>,
}

impl Watchers {
    /// Keep where writes landed for those who wait; with nobody waiting nothing is kept.
    fn note(&mut self, routes: &[DocumentRoute]) {
        if self.waiting.is_empty() {
            return;
        }
        for route in routes {
            if !self.landed.contains(route) {
                self.landed.push(route.clone());
            }
        }
    }

    /// Answer the waits past their bound, and those whose caller went away, so none is held for
    /// ever: a change nothing signalled costs an answer on time, not a stalled caller.
    ///
    /// Answers whether the last of them ended here: nobody waits for this workspace any more.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let waited = !self.waiting.is_empty();
        self.waiting.retain(|waiting| {
            if waiting.done.is_abandoned() || waiting.until <= now {
                return false;
            }
            true
        });
        if self.waiting.is_empty() {
            self.landed.clear();
        }
        waited && self.waiting.is_empty()
    }

    /// Whether someone waits for a change of this workspace.
    pub(super) fn any(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// Hold `done` until a change `caller` may read lands or `until` passes.
    fn wait(
        &mut self,
        until: Instant,
        caller: Caller,
        done: Completer<Result<(), ActionCallError>>,
    ) {
        self.waiting.push(ChangeWait {
            until,
            caller,
            done,
        });
    }

    /// When the earliest wait ends.
    pub(super) fn due(&self) -> Option<Instant> {
        self.waiting.iter().map(|waiting| waiting.until).min()
    }
}

impl Workspace {
    pub(super) fn pump(&mut self, workspace_id: WorkspaceId) {
        // The records a run of writes landed in, once each. What a landing
        // reads is the record as it stands at the read, so one per record
        // answers for all of a run's landings — and a busy workspace's writes
        // otherwise cost a read for every write since the last request.
        let mut landed: Vec<DocumentRoute> = Vec::new();
        // Of those, the documents a write landed in that this service did not make.
        let mut from_others: HashSet<DocumentRoute> = HashSet::new();
        let own = self.peer.engine().identity().device_cert.device_public_key;
        for event in self.member.take(workspace_id) {
            match event {
                Event::HostReady {
                    ws: ready,
                    documents_when_opened,
                    ..
                } if ready == workspace_id => {
                    self.ready = true;
                    self.when_opened = documents_when_opened;
                }
                Event::DocumentOpened {
                    ws: opened,
                    route: Some(route),
                    answer,
                    ..
                } if opened == workspace_id => {
                    tracing::debug!(
                        app = %route.app_id,
                        record = %route.document,
                        ?answer,
                        asked = self.asked.contains(&route),
                        "workspace actions: the workspace answered for a record"
                    );
                    // The host sent all it holds of a record this member
                    // asked for, and sends its changes from now on: none
                    // means it holds none of it. Another person's own record
                    // is answered as theirs: not absent, never founded here.
                    if self.asked.remove(&route) {
                        match answer {
                            tilo_session_client::DocumentAnswer::Sent { frames: 0 } => {
                                self.absent.insert(route.clone());
                            }
                            tilo_session_client::DocumentAnswer::NotYours => {
                                self.not_yours.insert(route.clone());
                            }
                            tilo_session_client::DocumentAnswer::Sent { .. } => {}
                        }
                        self.answered.insert(route);
                    }
                }
                Event::Landed { route, author } => {
                    if WorkspaceId::parse(&route.cluster_id) != Some(workspace_id) {
                        continue;
                    }
                    // One of this service's own writes landing, counted off what it handed
                    // ([`Workspace::unlanded`]); a write too large for one frame lands a
                    // segment at a time, each its own. A count of zero names no write of ours
                    // still to land: the next landing there is merged like any other's. The
                    // entry waits for a refresh to take it, so reading a zero as ours would
                    // swallow the server's own writes to that document until then.
                    let ours = author == Some(own)
                        && self.unlanded.get_mut(&route).is_some_and(|handed| {
                            let ours = *handed > 0;
                            *handed = handed.saturating_sub(1);
                            ours
                        });
                    if !ours {
                        from_others.insert(route.clone());
                    }
                    if !landed.contains(&route) {
                        landed.push(route);
                    }
                }
                _ => {}
            }
        }
        // What a watch reads is read again once a write landed in what its caller may read: a
        // write to any document, its governance among them, is what both it follows and who is in
        // the workspace are kept in. Whom these wake is decided by the workspace's policy
        // ([`Worker::wake_watchers`]).
        self.watchers.note(&landed);
        for route in landed {
            let engine = self.peer.engine();
            // Undecided: the replica holds the document and cannot read it yet. Keys that
            // arrived for a document it holds nothing of leave nothing undecided. A document only
            // this service wrote since it was last decided is one it read to write: reading a
            // sealed part out to learn only that cost most of a second for every action that
            // wrote one. Another's write is asked without reading the document out, as a write
            // lands for every post; this device's own writes not taken in yet leave only a
            // read to decide.
            let undecided = from_others.contains(&route) && {
                super::count_read();
                let reads = engine
                    .document_revision(&route.cluster_id, &route.app_id, &route.document)
                    .and_then(|revision| match revision {
                        Some(_) => Ok(true),
                        None => engine
                            .record(&route.cluster_id, &route.app_id, &route.document)
                            .map(|read| read.is_some()),
                    });
                match reads {
                    Ok(reads) => !reads,
                    Err(_) => engine.documents(&route.cluster_id).is_ok_and(|records| {
                        records
                            .iter()
                            .any(|(app, record)| *app == route.app_id && *record == route.document)
                    }),
                }
            };
            if undecided {
                self.undecided.insert(route.clone());
            } else if from_others.contains(&route) {
                self.undecided.remove(&route);
            }
            self.documents
                .get_or_insert_with(|| {
                    WorkspaceDocuments::new(&route.cluster_id, Arc::clone(self.peer.engine()))
                })
                .landed(&route);
            // A document the folder is made of changed: the files it indexes are read again. Any
            // write at all can move what a caller may read, so what a find kept of that is let
            // go here rather than answered stale.
            if route.app_id == fs_protocol::tree::FILES_APP {
                self.files.landed(&route.document);
            }
            self.readers.clear();
            self.listings.clear();
            self.named.clear();
        }
        self.landed_from_others.extend(from_others);
    }
}

impl Worker {
    /// Hold `done` until what a watch follows changes in `workspace_id`, or its wait runs out
    /// ([`MAX_REMOTE_WAIT`]): a write landing in the workspace wakes it, and who is in the
    /// workspace is kept in its records' governance, which is one of them.
    pub(super) fn watch(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        done: Completer<Result<(), ActionCallError>>,
    ) {
        if !self.admits_member(workspace_id, caller) {
            done.complete(Err(ActionCallError::NotPermitted));
            return;
        }
        // Everything that landed so far is in what the caller reads next: taken in before the
        // wait, so a write landing in between still wakes it.
        if let Err(error) = self.refresh(workspace_id, Needs::Workspace) {
            done.complete(Err(error));
            return;
        }
        // What landed before this wait began wakes who waited for it, and not this caller.
        self.wake_watchers();
        let until = Instant::now()
            .checked_add(MAX_REMOTE_WAIT)
            .unwrap_or_else(Instant::now);
        match self.membership(workspace_id) {
            Ok(workspace) => {
                workspace.watchers.wait(until, caller.clone(), done);
                // The wait is woken by a change landing in the workspace, as a request held for
                // one is: the session hands this thread every change of a workspace it is asked
                // to be woken for, and the wait answers on it ([`Job::Joined`]) rather than only
                // on its bound.
                self.awaiting(workspace_id, true);
            }
            Err(error) => done.complete(Err(error)),
        }
    }

    /// Answer who waits for a change of a workspace a write landed in, each only where it
    /// landed in something they may read whole ([`Self::reads_whole`]): a write to what the
    /// workspace keeps from a caller is no word to them, and their wait goes on to its bound.
    pub(super) fn wake_watchers(&mut self) {
        // A workspace someone waits for is read here for what landed: the change that woke this
        // thread was no request, so nothing else reads it. Its next change is asked for before
        // the read, because the session hands a change over once: one that landed between the
        // read and the ask would wake nobody. A workspace nobody waits for is not read and
        // not asked for.
        let waited: Vec<WorkspaceId> = self
            .workspaces
            .iter()
            .filter(|(_, workspace)| workspace.watchers.any())
            .map(|(workspace_id, _)| *workspace_id)
            .collect();
        for workspace_id in waited {
            self.awaiting(workspace_id, true);
            if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
                workspace.pump(workspace_id);
            }
        }
        let landed: Vec<(WorkspaceId, Vec<DocumentRoute>, Vec<ChangeWait>)> = self
            .workspaces
            .iter_mut()
            .filter(|(_, workspace)| !workspace.watchers.landed.is_empty())
            .map(|(workspace_id, workspace)| {
                (
                    *workspace_id,
                    std::mem::take(&mut workspace.watchers.landed),
                    std::mem::take(&mut workspace.watchers.waiting),
                )
            })
            .collect();
        for (workspace_id, routes, waiting) in landed {
            let mut held = Vec::new();
            for wait in waiting {
                if routes
                    .iter()
                    .any(|route| self.reads_whole(workspace_id, &wait.caller, route))
                {
                    wait.done.complete(Ok(()));
                } else {
                    held.push(wait);
                }
            }
            if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
                workspace.watchers.waiting.append(&mut held);
            }
            // Where everyone who waited was answered, the workspace's changes wake this thread
            // no longer.
            self.stop_waiting_if_idle(workspace_id);
        }
    }

    pub(super) fn expire_watchers(&mut self) {
        let now = Instant::now();
        let mut ended = Vec::new();
        for (workspace_id, workspace) in &mut self.workspaces {
            if workspace.watchers.expire(now) {
                ended.push(*workspace_id);
            }
        }
        // Nobody waits there any more: its changes wake this thread no longer.
        for workspace_id in ended {
            self.stop_waiting_if_idle(workspace_id);
        }
    }

    /// When the earliest wait for a change of any workspace ends.
    pub(super) fn watchers_until(&self) -> Option<Instant> {
        self.workspaces
            .values()
            .filter_map(|workspace| workspace.watchers.due())
            .min()
    }
}
