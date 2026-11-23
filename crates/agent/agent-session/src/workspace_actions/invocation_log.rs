//! The invocations the host's action service remembers: what a retry under
//! the same id, `tilo_status` and history read back.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tilo_actions::remote::{Caller, InvokeRequest};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use tilo_actions::remote::ActorPlace;

/// Invocations remembered for retries, status and history.
pub(super) const REMEMBERED: usize = 1024;

pub(super) struct Entry {
    pub(super) caller: Caller,
    pub(super) request: InvokeRequest,
    pub(super) update: InvocationUpdate,
    /// Where the invocation left its actor in the record it acted on.
    pub(super) left_at: Option<ActorPlace>,
    /// The link that opens what it made or changed ([`super::Worker::changed_link`]).
    pub(super) link: Option<String>,
    /// Set while the host's shell runs this invocation: what a cancel of it,
    /// or a Ctrl+C in the session it runs in, stops.
    pub(super) stop: Option<Arc<AtomicBool>>,
    /// It waits in its asked record for the person's answer ([`super::asking`]):
    /// their approve admits it again under its id.
    pub(super) asked: bool,
    /// It ran, and is answered done once the host holds what it changed:
    /// too late to cancel.
    pub(super) saving: bool,
    /// What the person is asked about it, once it waits for their answer
    /// ([`super::bounded::About`]).
    pub(super) about: Option<Box<super::bounded::About>>,
    /// The workspace it was asked in, once admitted there: what a listing of
    /// that workspace's invocations reads it by ([`super::invocations`]).
    pub(super) workspace: Option<tilo_server_protocol::WorkspaceId>,
    /// It only reads: a read is no invocation of the workspace, whatever it
    /// answers, so a listing leaves it out.
    pub(super) reads: bool,
    /// When it was asked for, in milliseconds since the Unix epoch.
    pub(super) started_ms: i64,
    /// When the host first saw it ended ([`InvocationLog::stamp_ended`]).
    pub(super) ended_ms: Option<i64>,
    /// The changes it handed to the session, each by its record's app, the
    /// record's id and the change's short hash as History lists it: what its
    /// changes are found by where its step is not known ([`super::invocations`]).
    pub(super) written: Vec<(String, String, String)>,
}

#[derive(Default)]
pub(super) struct InvocationLog {
    pub(super) entries: HashMap<InvocationId, Entry>,
    pub(super) order: VecDeque<InvocationId>,
    /// The invocations running now, each on the thread serving its
    /// workspace.
    pub(super) running: HashSet<InvocationId>,
    /// Bumped whenever an invocation's state moves; a follow of one waits on
    /// it instead of asking again on a timer.
    moved: Arc<Moves>,
}

/// The log's moves: a generation a follow waits past, and what wakes it. Kept
/// apart from the log's own lock so a follow waits without holding it.
#[derive(Default)]
pub(super) struct Moves {
    at: std::sync::Mutex<u64>,
    changed: std::sync::Condvar,
}

impl Moves {
    /// The generation now: a follow reads it with the state it saw, then
    /// waits past it.
    pub(super) fn generation(&self) -> u64 {
        *self.at.lock_recover()
    }

    /// Wait, at most `bound`, until an invocation's state moves past
    /// `generation`.
    pub(super) fn wait(&self, generation: u64, bound: std::time::Duration) {
        let moved = self.at.lock_recover();
        let _ = self
            .changed
            .wait_timeout_while(moved, bound, |now| *now == generation);
    }

    /// Note that an invocation's state moved: what wakes the follows
    /// waiting on it.
    fn moved(&self) {
        let mut at = self.at.lock_recover();
        *at = at.saturating_add(1);
        drop(at);
        self.changed.notify_all();
    }
}

impl InvocationLog {
    /// Remember `request`, making room by forgetting the oldest invocation
    /// that settled. One still queued, running or waiting for the person is
    /// never forgotten: it would never run, or run and leave its caller no
    /// answer, and a retry under its id would run it twice. `false`, and
    /// nothing remembered, when every invocation held is still unsettled.
    #[must_use]
    pub(super) fn insert(&mut self, caller: &Caller, request: InvokeRequest) -> bool {
        while self.order.len() >= REMEMBERED {
            let settled = self.order.iter().position(|id| {
                self.entries
                    .get(id)
                    .is_none_or(|entry| entry.update.is_terminal())
            });
            let Some(at) = settled else {
                return false;
            };
            if let Some(oldest) = self.order.remove(at) {
                self.entries.remove(&oldest);
            }
        }
        let invocation = request.invocation;
        self.order.push_back(invocation);
        self.entries.insert(
            invocation,
            Entry {
                caller: caller.clone(),
                request,
                update: InvocationUpdate::Accepted { invocation },
                left_at: None,
                link: None,
                stop: None,
                asked: false,
                saving: false,
                about: None,
                workspace: None,
                reads: false,
                started_ms: now_ms(),
                ended_ms: None,
                written: Vec::new(),
            },
        );
        self.moved();
        true
    }

    /// Note, of every invocation that ended since the last look, that it
    /// ended now: what a listing of invocations says it ended at.
    pub(super) fn stamp_ended(&mut self) {
        let now = now_ms();
        for entry in self.entries.values_mut() {
            if entry.ended_ms.is_none() && entry.update.is_terminal() {
                entry.ended_ms = Some(now);
            }
        }
    }

    pub(super) fn forget(&mut self, id: &InvocationId) {
        self.entries.remove(id);
        self.order.retain(|candidate| candidate != id);
        self.moved();
    }

    /// The generation of the log's moves, read with the state it is read
    /// with.
    pub(super) fn generation(&self) -> u64 {
        self.moved.generation()
    }

    /// What a follow waits on, taken out so it waits without this log's lock.
    pub(super) fn moves(&self) -> Arc<Moves> {
        Arc::clone(&self.moved)
    }

    /// Note that an invocation's state moved: what wakes the follows
    /// waiting on it.
    pub(super) fn moved(&self) {
        self.moved.moved();
    }

    /// `invocation` ends with `update`, the record it acted on showing where
    /// it left its actor.
    pub(super) fn answered(
        &mut self,
        invocation: InvocationId,
        update: &InvocationUpdate,
        left_at: Option<ActorPlace>,
    ) {
        if let Some(entry) = self.entries.get_mut(&invocation) {
            entry.update = update.clone();
            entry.left_at = left_at;
        }
        self.moved();
    }

    pub(super) fn owned(&self, caller: &Caller, id: &InvocationId) -> Option<&Entry> {
        self.entries
            .get(id)
            .filter(|entry| caller.continues(&entry.caller))
    }
}

/// Now, in milliseconds since the Unix epoch.
pub(super) fn now_ms() -> i64 {
    tilo_clock::unix_now_ms_i64()
}

#[cfg(test)]
mod tests {
    use tilo_actions::api::ActionAddress;
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

    use super::*;

    #[test]
    fn invocation_timestamps_read_through_the_shared_clock() {
        let caller = Caller::local_person("ada");
        let invocation = InvocationId::new();
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: "tilo.notes:page".into(),
                address: ActionAddress::new("notes.page", "append"),
            },
            argument: InvokeArgument::default(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let mut log = InvocationLog::default();
        let inserted = tilo_clock::with_wall_ms(1_700_000_000_123, || log.insert(&caller, request));
        assert!(inserted);
        assert_eq!(
            log.entries.get(&invocation).map(|entry| entry.started_ms),
            Some(1_700_000_000_123)
        );
        log.answered(
            invocation,
            &InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            None,
        );
        tilo_clock::with_wall_ms(1_700_000_005_456, || log.stamp_ended());
        assert_eq!(
            log.entries
                .get(&invocation)
                .and_then(|entry| entry.ended_ms),
            Some(1_700_000_005_456)
        );
    }
}
