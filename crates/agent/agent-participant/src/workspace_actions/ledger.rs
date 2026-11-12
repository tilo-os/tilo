//! The invocations the host's action service remembers: what a retry under
//! the same id, `tilo_status` and history read back.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tilo_actions::remote::{Caller, InvokeRequest};
use tilo_actions::{InvocationId, InvocationUpdate};

use tilo_actions::remote::ActorPlace;

/// Invocations remembered for retries, status and history.
pub(super) const REMEMBERED: usize = 1024;

pub(super) struct Entry {
    pub(super) caller: Caller,
    pub(super) request: InvokeRequest,
    pub(super) update: InvocationUpdate,
    /// Where the invocation left its actor in the record it acted on.
    pub(super) left_at: Option<ActorPlace>,
    /// Set while the host's shell runs this invocation: what a cancel of it,
    /// or a Ctrl+C in the session it runs in, stops.
    pub(super) stop: Option<Arc<AtomicBool>>,
    /// The person was asked to allow it ([`super::bounded`]): it was answered
    /// accepted, and settles as failed rather than being forgotten.
    pub(super) asked: bool,
    /// The token naming the person's answer, once they allowed it.
    pub(super) witness: Option<String>,
    /// What the person is asked about it, once it waits for their answer
    /// ([`super::bounded::About`]).
    pub(super) about: Option<Box<super::bounded::About>>,
}

#[derive(Default)]
pub(super) struct Ledger {
    pub(super) entries: HashMap<InvocationId, Entry>,
    pub(super) order: VecDeque<InvocationId>,
    pub(super) running: Option<InvocationId>,
}

impl Ledger {
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
                stop: None,
                asked: false,
                witness: None,
                about: None,
            },
        );
        true
    }

    pub(super) fn forget(&mut self, id: &InvocationId) {
        self.entries.remove(id);
        self.order.retain(|candidate| candidate != id);
    }

    pub(super) fn owned(&self, caller: &Caller, id: &InvocationId) -> Option<&Entry> {
        self.entries.get(id).filter(|entry| entry.caller == *caller)
    }
}
