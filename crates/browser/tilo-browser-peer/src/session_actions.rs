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
//!
//! An invocation the device accepted and still runs, a live read most of
//! all, is followed here: [`ActionHost::next_change`] asks the device to
//! hold its answer until the invocation moves, and the state it answers is
//! kept, so whoever waits is woken by the change itself rather than asking
//! again. A page, whose one thread never blocks, observes only what is kept.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeRequest, MAX_REMOTE_WAIT, Pending, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, UpdateMark, pending,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use web_time::Instant;

use crate::session::{PeerEvent, PeerEvents, PeerHandle};

/// How long a request waits for the device's answer.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(30);

/// How often requests past their deadline are answered.
const SWEEP: Duration = Duration::from_millis(500);

/// An answer to a held ask that comes sooner than this with nothing new is
/// a device that does not hold: half of [`MAX_REMOTE_WAIT`].
const HELD: Duration = Duration::from_secs(5);

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
    follows: HashMap<InvocationId, Followed>,
}

/// An invocation the device accepted, as it last answered for it.
struct Followed {
    state: InvocationState,
    /// Whether the device holds an answer until the invocation moves; one
    /// that answers at once is not asked to, so nobody spins on it.
    holds: bool,
    /// The held ask out now: when it was sent.
    asked: Option<Instant>,
    /// Waiting for the invocation to move.
    waiters: Vec<Completer<()>>,
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
                if let (Some(followed), Some(state)) = (self.follows.get_mut(&invocation), &state) {
                    followed.state = state.clone();
                }
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

    /// The device's answer to observing `id`, waited for: callers off a
    /// page observe from threads that may block.
    #[cfg(not(target_arch = "wasm32"))]
    fn ask_state(&self, id: &InvocationId) -> Option<InvocationState> {
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

    /// The device whose actions these are.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.device
    }
}

/// Answer requests past their deadline until the actions are dropped.
#[cfg(not(target_arch = "wasm32"))]
fn sweep(inner: Weak<Inner>) {
    let spawned = std::thread::Builder::new()
        .name("session-actions-deadline".into())
        .spawn(move || {
            loop {
                std::thread::sleep(SWEEP);
                if !expire(&inner) {
                    return;
                }
            }
        });
    // Without the thread, every request is still answered when the device
    // answers or the session ends; only the deadline is lost.
    drop(spawned);
}

/// Answer requests past their deadline until the actions are dropped: on
/// the page's own thread, between its other work.
#[cfg(target_arch = "wasm32")]
fn sweep(inner: Weak<Inner>) {
    n0_future::task::spawn(async move {
        loop {
            n0_future::time::sleep(SWEEP).await;
            if !expire(&inner) {
                return;
            }
        }
    });
}

/// Answer what waited past its deadline; whether the actions still exist.
fn expire(inner: &Weak<Inner>) -> bool {
    let Some(inner) = inner.upgrade() else {
        return false;
    };
    let mut give = Vec::new();
    {
        let mut waiting = inner.waiting.lock_recover();
        if waiting.is_empty() {
            return true;
        }
        waiting.expire(Instant::now(), &mut give);
    }
    give.into_iter().for_each(Give::give);
    true
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

    /// What the held ask for `id`, sent having seen `seen`, answered: keep
    /// it, and wake whoever waits for the invocation to move.
    fn followed(&self, id: InvocationId, seen: UpdateMark, state: Option<InvocationState>) {
        let mut waiting = self.waiting.lock_recover();
        let Some(followed) = waiting.follows.get_mut(&id) else {
            return;
        };
        // Nothing new, long before the hold was up: a device that does not
        // hold, which is asked as it is observed from then on.
        let quick = followed
            .asked
            .take()
            .is_some_and(|sent| sent.elapsed() < HELD);
        match state {
            Some(state) => {
                if quick && UpdateMark::of(&state.update) == seen {
                    followed.holds = false;
                }
                followed.state = state;
            }
            // Unreachable, or no longer the caller's: nothing to hold.
            None => followed.holds = false,
        }
        let waiters = std::mem::take(&mut followed.waiters);
        drop(waiting);
        for waiter in waiters {
            waiter.complete(());
        }
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
        // What the device accepted and still runs is followed from here.
        let inner = Arc::downgrade(&self.0);
        answered.map(move |receipt| {
            let accepted = matches!(
                &receipt,
                Ok(InvocationReceipt {
                    status: ReceiptStatus::Accepted,
                    ..
                })
            );
            if let Some(inner) = inner.upgrade().filter(|_| accepted) {
                inner
                    .waiting
                    .lock_recover()
                    .follows
                    .entry(invocation)
                    .or_insert_with(|| Followed {
                        state: InvocationState::new(
                            invocation,
                            InvocationUpdate::Accepted { invocation },
                        ),
                        holds: true,
                        asked: None,
                        waiters: Vec::new(),
                    });
            }
            receipt
        })
    }

    /// The invocation as the device last answered for it. A followed one
    /// whose device holds its answers is answered from what is kept; any
    /// other is asked, and waited for, off a page.
    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let kept = self
            .0
            .waiting
            .lock_recover()
            .follows
            .get(id)
            .map(|followed| (followed.holds, followed.state.clone()));
        match kept {
            Some((true, state)) => Some(state),
            #[cfg(target_arch = "wasm32")]
            kept => kept.map(|(_, state)| state),
            #[cfg(not(target_arch = "wasm32"))]
            _ => self.ask_state(id),
        }
    }

    /// Ask the device to answer once the followed invocation `id` moves;
    /// `None` for one not followed, settled, or on a device that does not
    /// hold its answers.
    fn next_change(&self, _caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let (woken, change) = pending();
        let mut waiting = self.0.waiting.lock_recover();
        let followed = waiting
            .follows
            .get_mut(id)
            .filter(|followed| followed.holds && !followed.state.is_terminal())?;
        followed.waiters.push(woken);
        if followed.asked.is_some() {
            drop(waiting);
            return Some(change);
        }
        followed.asked = Some(Instant::now());
        let seen = UpdateMark::of(&followed.state.update);
        let (answer, answered) = pending();
        waiting
            .updates
            .entry(*id)
            .or_default()
            .push(Asked::new(answer));
        drop(waiting);
        let inner = Arc::downgrade(&self.0);
        let invocation = *id;
        answered.then(move |state| {
            if let Some(inner) = inner.upgrade() {
                inner.followed(invocation, seen, state.flatten());
            }
        });
        let wait_ms = u32::try_from(MAX_REMOTE_WAIT.as_millis()).unwrap_or(u32::MAX);
        let sent = self.0.send(RemoteActionMessage::Observe {
            invocation,
            wait_ms: Some(wait_ms),
            seen: Some(seen),
        });
        if sent.is_err() {
            let asked = self.0.waiting.lock_recover().updates.remove(id);
            for asked in asked.unwrap_or_default() {
                asked.answer.complete(None);
            }
        }
        Some(change)
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
