//! Another device's actions, asked through this device's session with the
//! workspace's host.
//!
//! A device that is a member of a workspace reaches every other device of
//! it through the session it already holds with the workspace's host: a
//! remote-action frame addressed to a device's key is carried there, and
//! that device's answer comes back on the same session. The host's own
//! device answers on its app-serving plane, which on a host that serves the
//! workspace's records is every record's actions, whoever has it open.
//!
//! [`SessionActions`] is that, as an [`ActionHost`]: whatever drives a local
//! action host (an MCP projection, a script) drives the other device's
//! instead. Nothing is decided here. The answering device authenticates the
//! session's device and decides every request from that and its own
//! policy; the [`Caller`] a local host would be handed is not sent.
//!
//! A request whose answer does not come within [`REPLY_DEADLINE`] is
//! answered as unavailable, as is every request waiting when the host says
//! the device cannot be reached, so a caller always hears back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, pending,
};
use tilo_lock::MutexRecover as _;

use crate::session::{PeerEvent, PeerEvents, PeerHandle};

/// How long a request waits for the device's answer.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(30);

/// How often requests past their deadline are answered.
const SWEEP: Duration = Duration::from_millis(500);

/// One device's actions in one workspace, asked through `peer`'s session.
/// Cheap to clone; the last clone dropped stops listening.
#[derive(Clone)]
pub struct SessionActions(Arc<Inner>);

struct Inner {
    peer: PeerHandle,
    workspace_id: u64,
    device: [u8; 32],
    events: PeerEvents,
    waiting: Mutex<Waiting>,
    next_correlation: AtomicU64,
}

/// A request waiting for its answer, and when it gives up.
struct Asked<T> {
    answer: Completer<T>,
    until: Instant,
}

impl<T> Asked<T> {
    fn new(answer: Completer<T>) -> Self {
        Self {
            answer,
            until: Instant::now()
                .checked_add(REPLY_DEADLINE)
                .unwrap_or_else(Instant::now),
        }
    }
}

type Answered<T> = Result<T, ActionHostError>;

#[derive(Default)]
struct Waiting {
    catalogues: HashMap<u64, Asked<Answered<CataloguePage>>>,
    receipts: HashMap<InvocationId, Vec<Asked<Answered<InvocationReceipt>>>>,
    updates: HashMap<InvocationId, Vec<Asked<Option<InvocationState>>>>,
    cancels: HashMap<InvocationId, Vec<Asked<Answered<()>>>>,
}

/// An answer to give once the lock on [`Waiting`] is released: completing
/// one runs its caller's continuation, which may ask again.
enum Give {
    Catalogue(Completer<Answered<CataloguePage>>, Answered<CataloguePage>),
    Receipt(
        Completer<Answered<InvocationReceipt>>,
        Answered<InvocationReceipt>,
    ),
    Update(Completer<Option<InvocationState>>, Option<InvocationState>),
    Cancel(Completer<Answered<()>>, Answered<()>),
}

impl Give {
    fn give(self) {
        match self {
            Self::Catalogue(answer, value) => answer.complete(value),
            Self::Receipt(answer, value) => answer.complete(value),
            Self::Update(answer, value) => answer.complete(value),
            Self::Cancel(answer, value) => answer.complete(value),
        }
    }
}

fn unavailable(reason: impl Into<String>) -> ActionHostError {
    ActionHostError::Unavailable {
        reason: reason.into(),
    }
}

impl Waiting {
    /// Every waiting request, answered with `error`.
    fn fail_all(&mut self, error: &ActionHostError, give: &mut Vec<Give>) {
        for (_, asked) in self.catalogues.drain() {
            give.push(Give::Catalogue(asked.answer, Err(error.clone())));
        }
        for asked in self.receipts.drain().flat_map(|(_, all)| all) {
            give.push(Give::Receipt(asked.answer, Err(error.clone())));
        }
        for asked in self.updates.drain().flat_map(|(_, all)| all) {
            give.push(Give::Update(asked.answer, None));
        }
        for asked in self.cancels.drain().flat_map(|(_, all)| all) {
            give.push(Give::Cancel(asked.answer, Err(error.clone())));
        }
    }

    /// The requests past their deadline, answered as unanswered.
    fn expire(&mut self, now: Instant, give: &mut Vec<Give>) {
        let late = || unavailable("the device did not answer in time");
        let expired: Vec<u64> = self
            .catalogues
            .iter()
            .filter(|(_, asked)| asked.until <= now)
            .map(|(correlation, _)| *correlation)
            .collect();
        for correlation in expired {
            if let Some(asked) = self.catalogues.remove(&correlation) {
                give.push(Give::Catalogue(asked.answer, Err(late())));
            }
        }
        for asked in split(&mut self.receipts, now) {
            give.push(Give::Receipt(asked.answer, Err(late())));
        }
        for asked in split(&mut self.updates, now) {
            give.push(Give::Update(asked.answer, None));
        }
        for asked in split(&mut self.cancels, now) {
            give.push(Give::Cancel(asked.answer, Err(late())));
        }
    }

    fn is_empty(&self) -> bool {
        self.catalogues.is_empty()
            && self.receipts.is_empty()
            && self.updates.is_empty()
            && self.cancels.is_empty()
    }

    /// The requests `message` answers.
    fn answer(&mut self, message: RemoteActionMessage, give: &mut Vec<Give>) {
        match message {
            RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            } => {
                if let Some(asked) = self.catalogues.remove(&correlation) {
                    give.push(Give::Catalogue(asked.answer, result));
                }
            }
            RemoteActionMessage::Receipt(receipt) => {
                for asked in self
                    .receipts
                    .remove(&receipt.invocation)
                    .unwrap_or_default()
                {
                    let answer = match &receipt.status {
                        ReceiptStatus::Rejected { error } => Err(error.clone()),
                        _ => Ok(receipt.clone()),
                    };
                    give.push(Give::Receipt(asked.answer, answer));
                }
            }
            // An update the device pushes unasked answers whoever waits for
            // one; with nobody waiting, the next observe asks again.
            RemoteActionMessage::Update { invocation, state } => {
                for asked in self.updates.remove(&invocation).unwrap_or_default() {
                    give.push(Give::Update(asked.answer, state.clone()));
                }
            }
            RemoteActionMessage::CancelResponse { invocation, result } => {
                for asked in self.cancels.remove(&invocation).unwrap_or_default() {
                    give.push(Give::Cancel(asked.answer, result.clone()));
                }
            }
            _ => {}
        }
    }
}

/// The requests of `all` past their deadline, taken out of it.
fn split<T>(all: &mut HashMap<InvocationId, Vec<Asked<T>>>, now: Instant) -> Vec<Asked<T>> {
    let mut late = Vec::new();
    for waiting in all.values_mut() {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(waiting)
            .into_iter()
            .partition(|asked| asked.until <= now);
        *waiting = kept;
        late.extend(gone);
    }
    all.retain(|_, waiting| !waiting.is_empty());
    late
}

impl SessionActions {
    /// The actions of the device `device` in `workspace_id`, asked through
    /// `peer`, a session already in that workspace.
    #[must_use]
    pub fn new(peer: &PeerHandle, workspace_id: u64, device: [u8; 32]) -> Self {
        let events = peer.events();
        let inner = Arc::new(Inner {
            peer: peer.clone(),
            workspace_id,
            device,
            events,
            waiting: Mutex::default(),
            next_correlation: AtomicU64::new(1),
        });
        let woken = Arc::downgrade(&inner);
        peer.on_change(Arc::new(move || {
            if let Some(inner) = woken.upgrade() {
                inner.pump();
            }
        }));
        sweep(Arc::downgrade(&inner));
        Self(inner)
    }

    /// The device whose actions these are.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.device
    }
}

/// Answer requests past their deadline until the actions are dropped.
fn sweep(inner: Weak<Inner>) {
    let spawned = std::thread::Builder::new()
        .name("session-actions-deadline".into())
        .spawn(move || {
            loop {
                std::thread::sleep(SWEEP);
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                let mut give = Vec::new();
                {
                    let mut waiting = inner.waiting.lock_recover();
                    if waiting.is_empty() {
                        continue;
                    }
                    waiting.expire(Instant::now(), &mut give);
                }
                give.into_iter().for_each(Give::give);
            }
        });
    // Without the thread, every request is still answered when the device
    // answers or the session ends; only the deadline is lost.
    drop(spawned);
}

impl Inner {
    /// Take what the session received: the device's answers, and its
    /// unreachability.
    fn pump(&self) {
        let mut give = Vec::new();
        for event in self.events.drain() {
            match event {
                PeerEvent::RemoteAction {
                    workspace_id,
                    from,
                    frame,
                } if workspace_id == self.workspace_id && from == self.device => {
                    let Ok(frame) = RemoteActionFrame::decode(&frame) else {
                        continue;
                    };
                    self.waiting.lock_recover().answer(frame.message, &mut give);
                }
                PeerEvent::RemoteActionUndeliverable {
                    workspace_id,
                    to,
                    reason,
                } if workspace_id == self.workspace_id && to == self.device => {
                    self.waiting
                        .lock_recover()
                        .fail_all(&unavailable(reason), &mut give);
                }
                _ => {}
            }
        }
        give.into_iter().for_each(Give::give);
    }

    /// Send `message` to the device; `Err` when the session cannot carry it.
    fn send(&self, message: RemoteActionMessage) -> Result<(), ActionHostError> {
        let frame = RemoteActionFrame::new(message).encode().map_err(|error| {
            ActionHostError::InvalidRequest {
                reason: error.to_string(),
            }
        })?;
        self.peer
            .send_remote_action(self.workspace_id, self.device, frame)
            .map_err(|error| unavailable(format!("the session is closed: {error}")))
    }
}

impl ActionHost for SessionActions {
    fn query(&self, _caller: &Caller, query: CatalogueQuery) -> Pending<Answered<CataloguePage>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = pending();
        self.0
            .waiting
            .lock_recover()
            .catalogues
            .insert(correlation, Asked::new(answer));
        if let Err(error) = self
            .0
            .send(RemoteActionMessage::CatalogueRequest { correlation, query })
        {
            let asked = self
                .0
                .waiting
                .lock_recover()
                .catalogues
                .remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        }
        answered
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Answered<InvocationReceipt>> {
        let invocation = request.invocation;
        let (answer, answered) = pending();
        self.0
            .waiting
            .lock_recover()
            .receipts
            .entry(invocation)
            .or_default()
            .push(Asked::new(answer));
        if let Err(error) = self.0.send(RemoteActionMessage::Invoke(request)) {
            let asked = self.0.waiting.lock_recover().receipts.remove(&invocation);
            for asked in asked.unwrap_or_default() {
                asked.answer.complete(Err(error.clone()));
            }
        }
        answered
    }

    /// The device's answer to asking, waited for as the trait's synchronous
    /// shape requires: callers observe from threads that may block.
    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let (answer, answered) = pending();
        self.0
            .waiting
            .lock_recover()
            .updates
            .entry(*id)
            .or_default()
            .push(Asked::new(answer));
        self.0
            .send(RemoteActionMessage::Observe {
                invocation: *id,
                wait_ms: None,
                seen: None,
            })
            .ok()?;
        answered
            .wait_timeout(REPLY_DEADLINE)
            .ok()
            .flatten()
            .flatten()
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Answered<()>> {
        let (answer, answered) = pending();
        self.0
            .waiting
            .lock_recover()
            .cancels
            .entry(*id)
            .or_default()
            .push(Asked::new(answer));
        if let Err(error) = self.0.send(RemoteActionMessage::Cancel { invocation: *id }) {
            let asked = self.0.waiting.lock_recover().cancels.remove(id);
            for asked in asked.unwrap_or_default() {
                asked.answer.complete(Err(error.clone()));
            }
        }
        answered
    }
}
