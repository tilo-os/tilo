//! What a watch of a workspace follows, and where it is woken: every write landing in the
//! workspace's replica answers a waiting watch, and who is in the workspace moves with its
//! governance, which is one of its records. A caller asks for such a wait across the wire
//! ([`tilo_actions::remote::ActionHost::changed`]); the device that holds the workspace holds its
//! answer here until a change comes or the wait runs out.
use super::{
    ActionHostError, Arc, Caller, Completer, DocumentRoute, Event, Instant, MAX_REMOTE_WAIT, Needs,
    Worker, Workspace, WorkspaceId, WorkspaceRecords,
};

/// One caller waiting for a change of what it follows in a workspace ([`Worker::watch`]).
struct ChangeWait {
    until: Instant,
    done: Completer<Result<(), ActionHostError>>,
}

/// Who waits for a change of one workspace, and when each wait ends ([`MAX_REMOTE_WAIT`] after it
/// began). One landing answers both what a watch reads of the records and of who is in the
/// workspace: the roster its governance names is one of its records, and the caller reads what it
/// follows again either way.
#[derive(Default)]
pub(super) struct Watchers {
    waiting: Vec<ChangeWait>,
}

impl Watchers {
    /// Answer everyone waiting for a change of this workspace: a landing is one.
    fn wake(&mut self) {
        for ChangeWait { done, .. } in std::mem::take(&mut self.waiting) {
            done.complete(Ok(()));
        }
    }

    /// Answer the waits past their bound, and those whose caller went away, so none is held for
    /// ever: a change nothing signalled costs an answer on time, not a stalled caller.
    pub(super) fn expire(&mut self, now: Instant) {
        self.waiting.retain(|waiting| {
            if waiting.done.is_abandoned() || waiting.until <= now {
                return false;
            }
            true
        });
    }

    /// Hold `done` until a change lands or `until` passes.
    fn wait(&mut self, until: Instant, done: Completer<Result<(), ActionHostError>>) {
        self.waiting.push(ChangeWait { until, done });
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
        for event in self.member.take(workspace_id) {
            match event {
                Event::HostReady {
                    ws: ready,
                    records_when_opened,
                    ..
                } if ready == workspace_id => {
                    self.ready = true;
                    self.when_opened = records_when_opened;
                }
                Event::RecordOpened {
                    ws: opened,
                    route: Some(route),
                    answer,
                    ..
                } if opened == workspace_id => {
                    tracing::debug!(
                        app = %route.app_id,
                        record = %route.record,
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
                            tilo_session_client::RecordAnswer::Sent { frames: 0 } => {
                                self.absent.insert(route.clone());
                            }
                            tilo_session_client::RecordAnswer::NotYours => {
                                self.not_yours.insert(route.clone());
                            }
                            tilo_session_client::RecordAnswer::Sent { .. } => {}
                        }
                        self.answered.insert(route);
                    }
                }
                Event::Landed { route, .. } => {
                    if WorkspaceId::parse(&route.cluster_id) != Some(workspace_id) {
                        continue;
                    }
                    if !landed.contains(&route) {
                        landed.push(route);
                    }
                }
                _ => {}
            }
        }
        let landed_any = !landed.is_empty();
        for route in landed {
            let engine = self.peer.engine();
            // Undecided: the replica holds the record and cannot read it yet. Keys that
            // arrived for a record it holds nothing of leave nothing undecided.
            // Asked without reading the record out, as a write lands for every post;
            // this device's own writes not taken in yet leave only a read to decide.
            let reads = engine
                .record_revision(&route.cluster_id, &route.app_id, &route.record)
                .and_then(|revision| match revision {
                    Some(_) => Ok(true),
                    None => engine
                        .record(&route.cluster_id, &route.app_id, &route.record)
                        .map(|read| read.is_some()),
                });
            let undecided = match reads {
                Ok(reads) => !reads,
                Err(_) => engine.records(&route.cluster_id).is_ok_and(|records| {
                    records
                        .iter()
                        .any(|(app, record)| *app == route.app_id && *record == route.record)
                }),
            };
            if undecided {
                self.undecided.insert(route.clone());
            } else {
                self.undecided.remove(&route);
            }
            self.records
                .get_or_insert_with(|| {
                    WorkspaceRecords::new(&route.cluster_id, Arc::clone(self.peer.engine()))
                })
                .landed(&route);
        }
        if landed_any {
            // What a watch reads is read again now: a write to any record, its governance among
            // them, is what both it follows and who is in the workspace are kept in.
            self.watchers.wake();
        }
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
        done: Completer<Result<(), ActionHostError>>,
    ) {
        if !self.admits_member(workspace_id, caller) {
            done.complete(Err(ActionHostError::NotPermitted));
            return;
        }
        // Everything that landed so far is in what the caller reads next: taken in before the
        // wait, so a write landing in between still wakes it.
        if let Err(error) = self.refresh(workspace_id, Needs::Workspace) {
            done.complete(Err(error));
            return;
        }
        let until = Instant::now()
            .checked_add(MAX_REMOTE_WAIT)
            .unwrap_or_else(Instant::now);
        match self.membership(workspace_id) {
            Ok(workspace) => workspace.watchers.wait(until, done),
            Err(error) => done.complete(Err(error)),
        }
    }

    /// Answer the waits for a change of any workspace that ran out, so none is held for ever.
    pub(super) fn expire_watchers(&mut self) {
        let now = Instant::now();
        for workspace in self.workspaces.values_mut() {
            workspace.watchers.expire(now);
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
