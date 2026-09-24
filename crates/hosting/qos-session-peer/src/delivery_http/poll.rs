//! Actor-owned continuations retain the original charged request, never a native guard.
use crate::https::{BufferedRequest, Route};
use std::collections::BTreeMap;
use tilo_relay::delivery_v3::{
    auth::{Binding, VerifiedSession},
    cursor::CursorToken,
    http_proof::{Request, VerifiedRequest},
    poll_chain::{ChainPoll, MAX_CHAIN_HELD},
};
use tokio::{sync::watch, time::Instant};

// A held poll cannot outnumber the actual parent registry's live-session bound
// times the bounded PIPELINED_POLL_V1 chain of one consumer.
const MAX_POLLS: usize = super::LIVE_LIMIT * MAX_CHAIN_HELD;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PollId(u64);
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PollError {
    Invalid,
    Capacity,
    Cancelled,
    Replaced,
}
/// Captured only while borrowing the original successful proof admission.
/// This is an immutable continuation binding, not current native authorization.
pub(super) struct AdmissionTicket {
    id: PollId,
    session: [u8; 16],
    binding: Binding,
    entry: u64,
    expires: u64,
    request_digest: [u8; 32],
    deadline: Instant,
    cancellation: watch::Receiver<bool>,
    cursor: Option<CursorToken>,
    chain: Option<ChainPoll>,
}
pub(super) struct PendingPoll {
    ticket: AdmissionTicket,
    request: BufferedRequest,
}
pub(super) struct Polls {
    next: u64,
    pending: BTreeMap<PollId, PendingPoll>,
}
fn request_digest(request: &Request<'_>) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-delivery-v3-held-poll-v1\0");
    // Only GET events passes prepare_poll; domain still binds the method explicitly.
    hash.update(b"GET\0");
    hash.update(&(request.target().len() as u64).to_le_bytes());
    hash.update(request.target().as_bytes());
    hash.update(blake3::hash(request.body()).as_bytes());
    *hash.finalize().as_bytes()
}
fn same_consumer(left: &AdmissionTicket, right: &AdmissionTicket) -> bool {
    left.binding.workspace == right.binding.workspace
        && left.binding.workspace_root == right.binding.workspace_root
        && left.binding.device == right.binding.device
}
impl Polls {
    pub(super) fn new() -> Self {
        Self {
            next: 0,
            pending: BTreeMap::new(),
        }
    }
    pub(super) fn capture(
        &mut self,
        admitted: &VerifiedRequest<'_>,
        original: &BufferedRequest,
        entry: u64,
    ) -> Result<AdmissionTicket, PollError> {
        if entry == 0 || original.route() != Route::EventsPoll {
            return Err(PollError::Invalid);
        }
        if original.is_cancelled() {
            return Err(PollError::Cancelled);
        }
        let (cursor, chain) =
            super::data::prepare_poll_chain(admitted.request()).map_err(|_| PollError::Invalid)?;
        // Chained polls exist only for sessions that negotiated them.
        if chain.is_some()
            && admitted.session().capabilities()
                & tilo_relay::delivery_v3::capabilities::PIPELINED_POLL_V1
                == 0
        {
            return Err(PollError::Invalid);
        }
        let original_request = Request::new(
            original.method().as_str(),
            original.target(),
            original.body(),
        )
        .map_err(|_| PollError::Invalid)?;
        super::data::prepare_poll(&original_request).map_err(|_| PollError::Invalid)?;
        if request_digest(admitted.request()) != request_digest(&original_request) {
            return Err(PollError::Invalid);
        }
        if self.pending.len() >= MAX_POLLS {
            return Err(PollError::Capacity);
        }
        self.next = self.next.checked_add(1).ok_or(PollError::Capacity)?;
        Ok(AdmissionTicket {
            id: PollId(self.next),
            session: admitted.session().session_id(),
            binding: admitted.session().binding().clone(),
            entry,
            expires: admitted.session().expires_at_ns(),
            request_digest: request_digest(admitted.request()),
            deadline: original.deadline(),
            cancellation: original.cancellation(),
            cursor,
            chain,
        })
    }
    pub(super) fn park(
        &mut self,
        ticket: AdmissionTicket,
        request: BufferedRequest,
    ) -> Result<PollId, PollError> {
        let exact = Request::new(request.method().as_str(), request.target(), request.body())
            .map_err(|_| PollError::Invalid)?;
        super::data::prepare_poll(&exact).map_err(|_| PollError::Invalid)?;
        if request.route() != Route::EventsPoll
            || ticket.request_digest != request_digest(&exact)
            || ticket.deadline != request.deadline()
            || !ticket.cancellation.same_channel(&request.cancellation())
        {
            return Err(PollError::Invalid);
        }
        let pending = PendingPoll { ticket, request };
        if pending.request.is_cancelled() {
            return Err(PollError::Cancelled);
        }
        // Abandoned (client-cancelled) polls must not count against the table
        // or against the one-poll-per-consumer rule.
        self.prune_cancelled();
        if pending.ticket.chain.is_some_and(|chain| chain.index > 0) {
            // A later poll of a chain joins the consumer's held chain, bounded.
            let held = self
                .pending
                .values()
                .filter(|p| same_consumer(&p.ticket, &pending.ticket))
                .count();
            if held >= MAX_CHAIN_HELD {
                return Err(PollError::Capacity);
            }
        } else {
            self.supersede(&pending.ticket);
        }
        self.repark(pending)
    }
    /// One stable certified consumer holds only one request (or one chain),
    /// even across sessions. A newer plain poll or chain start supersedes the
    /// older requests, which are answered empty (the client simply polls
    /// again) instead of the newer poll being dropped as a 503. Called on
    /// arrival, also when the newer poll is answered at once.
    pub(super) fn supersede(&mut self, ticket: &AdmissionTicket) {
        if ticket.chain.is_some_and(|chain| chain.index > 0) {
            return;
        }
        let superseded: Vec<PollId> = self
            .pending
            .iter()
            .filter(|(_, p)| same_consumer(&p.ticket, ticket))
            .map(|(id, _)| *id)
            .collect();
        for id in superseded {
            if let Some(old) = self.pending.remove(&id) {
                let _ = old.request.respond(hyper::StatusCode::OK, Vec::new());
            }
        }
    }
    /// Sessions that currently hold a poll.
    pub(super) fn sessions(&self) -> std::collections::BTreeSet<[u8; 16]> {
        self.pending.values().map(|p| p.ticket.session).collect()
    }
    /// Retain a held poll again (arrival after supersession, or a wake that
    /// found nothing new). Never supersedes: a chain keeps its later polls.
    pub(super) fn repark(&mut self, pending: PendingPoll) -> Result<PollId, PollError> {
        if pending.request.is_cancelled() {
            return Err(PollError::Cancelled);
        }
        self.prune_cancelled();
        if self.pending.len() >= MAX_POLLS || self.pending.contains_key(&pending.ticket.id) {
            return Err(PollError::Capacity);
        }
        let id = pending.ticket.id;
        self.pending.insert(id, pending);
        Ok(id)
    }
    pub(super) fn wake_ids(&self) -> Vec<PollId> {
        self.pending.keys().copied().collect()
    }
    /// When the earliest held poll must be answered idle (`margin` ahead of
    /// its HTTPS deadline).
    pub(super) fn next_deadline(&self, margin: std::time::Duration) -> Option<Instant> {
        self.pending
            .values()
            .map(|p| {
                p.ticket
                    .deadline
                    .checked_sub(margin)
                    .unwrap_or(p.ticket.deadline)
            })
            .min()
    }
    pub(super) fn take(&mut self, id: PollId) -> Option<PendingPoll> {
        self.pending.remove(&id)
    }
    pub(super) fn fence(&mut self, session: [u8; 16]) {
        self.pending
            .retain(|_, pending| pending.ticket.session != session);
    }
    pub(super) fn prune_cancelled(&mut self) {
        self.pending
            .retain(|_, pending| !pending.request.is_cancelled());
    }
}
impl AdmissionTicket {
    pub(super) fn chain(&self) -> Option<ChainPoll> {
        self.chain
    }
}
impl PendingPoll {
    pub(super) fn session_id(&self) -> [u8; 16] {
        self.ticket.session
    }
    pub(super) fn original(&self) -> &BufferedRequest {
        &self.request
    }
    /// Parent must first revalidate the same live registry entry against a fresh
    /// CommittedView. This binding check alone never authorizes a page.
    pub(super) fn check_session(
        &self,
        session: &VerifiedSession,
        entry: u64,
        now_ns: u64,
    ) -> Result<(), PollError> {
        if self.request.is_cancelled() {
            return Err(PollError::Cancelled);
        }
        if entry != self.ticket.entry
            || session.session_id() != self.ticket.session
            || session.binding() != &self.ticket.binding
            || now_ns >= self.ticket.expires
            || session.expires_at_ns() != self.ticket.expires
        {
            return Err(PollError::Replaced);
        }
        Ok(())
    }
    pub(super) fn cursor(&self) -> Option<&CursorToken> {
        self.ticket.cursor.as_ref()
    }
    pub(super) fn chain(&self) -> Option<ChainPoll> {
        self.ticket.chain
    }
    pub(super) fn into_request(self) -> BufferedRequest {
        self.request
    }
}
#[cfg(test)]
mod tests;
