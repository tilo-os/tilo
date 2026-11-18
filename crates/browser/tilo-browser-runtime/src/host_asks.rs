//! What a sheet over the page asks the workspace's host, answered on a later pass: the host
//! answers from its own thread, and whoever asked hears it on the page's.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};

/// What the host answered, from its own thread.
pub(crate) type Reply = Result<WorkspacesReply, String>;

/// Who hears a host's answer.
type Then = Box<dyn FnOnce(Reply)>;

/// The asks of one sheet to the workspace host of peer `peer`.
pub(crate) struct HostAsks {
    peer: Option<u32>,
    /// Why nothing is asked where the page has no host.
    no_host: &'static str,
    /// Set when an answer arrives: the page follows its sheets again.
    woken: Arc<AtomicBool>,
    /// What the host answered, by the ask, from its own thread.
    replies: Arc<Mutex<Vec<(u64, Reply)>>>,
    /// Who hears each answer, by the ask.
    waiting: RefCell<BTreeMap<u64, Then>>,
    asks: Cell<u64>,
}

impl HostAsks {
    pub(crate) fn new(peer: Option<u32>, no_host: &'static str, woken: Arc<AtomicBool>) -> Self {
        Self {
            peer,
            no_host,
            woken,
            replies: Arc::default(),
            waiting: RefCell::default(),
            asks: Cell::new(0),
        }
    }

    /// Whether the page has a host to ask.
    pub(crate) fn hosted(&self) -> bool {
        self.peer.is_some()
    }

    /// Ask the host `request`; `then` hears its answer on a later pass, or why it was not asked.
    pub(crate) fn ask(
        &self,
        request: impl FnOnce(&str) -> WorkspacesRequest + 'static,
        then: impl FnOnce(Reply) + 'static,
    ) {
        let Some(peer) = self.peer else {
            return then(Err(self.no_host.to_owned()));
        };
        let id = self.asks.replace(self.asks.get().wrapping_add(1));
        let (replies, woken) = (Arc::clone(&self.replies), Arc::clone(&self.woken));
        let asked = crate::replicated::host_then(
            peer,
            Box::new(request),
            Box::new(move |reply| {
                replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((id, reply));
                woken.store(true, Ordering::Release);
            }),
        );
        match asked {
            Ok(()) => drop(self.waiting.borrow_mut().insert(id, Box::new(then))),
            Err(error) => then(Err(error)),
        }
    }

    /// Hand what the host answered since the last pass to whoever asked. Whether anything
    /// arrived.
    pub(crate) fn take_replies(&self) -> bool {
        let replies: Vec<(u64, Reply)> = std::mem::take(
            &mut *self
                .replies
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let arrived = !replies.is_empty();
        for (id, reply) in replies {
            let then = self.waiting.borrow_mut().remove(&id);
            if let Some(then) = then {
                then(reply);
            }
        }
        arrived
    }
}

/// What the host refused, in its own words, or what it answered instead of what was asked.
pub(crate) fn refused(reply: Reply) -> String {
    match reply {
        Ok(WorkspacesReply::Refused(reason)) => reason,
        Ok(_) => "the workspace answered something else than what was asked".to_owned(),
        Err(error) => error,
    }
}
