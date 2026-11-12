//! Another device's actions, asked through this device's session with the workspace's host: an
//! invocation frame addressed to a device's key is carried there, and its answer comes back on the
//! same session. [`SessionActions`] is that, as an [`ActionHost`], so whatever drives a local action
//! host drives another device's instead. The answering device authenticates the session's device
//! and decides every request from that.
//!
//! A host brings its own session ([`RemoteSession`]): this desktop's peer actor, a page's endpoint,
//! a grant's session with its relay. An invocation the device accepted and still runs is followed
//! here, so whoever waits is woken by the change itself rather than asking again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Completer,
    ConfirmedInvocation, ContentGetAnswer, ContentPutAnswer, InvocationReceipt, InvocationState,
    InvokeRequest, MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    UpdateMark, pending, serve,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use web_time::Instant;

#[cfg(not(target_arch = "wasm32"))]
mod gets;
#[cfg(not(target_arch = "wasm32"))]
mod puts;

/// How long a request waits for the device's answer.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(30);

/// How often requests past their deadline are answered.
const SWEEP: Duration = Duration::from_millis(500);

/// An answer sooner than this with nothing new is a device that does not hold.
const HELD: Duration = Duration::from_secs(5);

/// What a host's session heard for the asks here, since the last call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// A device answered, or another of the person's devices sent a request of its own.
    Action {
        workspace_id: u64,
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// The session could not carry a request to the device, and why.
    Undeliverable {
        workspace_id: u64,
        to: [u8; 32],
        reason: String,
    },
}

/// A host's session with a workspace's host: frames to a device's key, what it heard, and a wake.
pub trait RemoteSession: Send + Sync {
    /// Send `frame` to the device `device` in `workspace_id`.
    ///
    /// # Errors
    ///
    /// The session cannot carry it.
    fn send_remote_action(
        &self,
        workspace_id: u64,
        device: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String>;
    /// Take everything the session heard since the last call.
    fn take_heard(&self) -> Vec<Heard>;
    /// Call `wake` whenever the session heard something or its state changed.
    fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>);
}

/// One device's actions in one workspace, asked through the session: the last clone dropped stops
/// listening.
#[derive(Clone)]
pub struct SessionActions(Arc<Inner>);

struct Inner {
    session: Arc<dyn RemoteSession>,
    workspace_id: u64,
    device: [u8; 32],
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
    puts: HashMap<u64, Asked<Answered<ContentPutAnswer>>>,
    gets: HashMap<u64, Asked<Answered<ContentGetAnswer>>>,
    confirms: HashMap<u64, Asked<Answered<ConfirmedInvocation>>>,
}

/// An invocation the device accepted, as it last answered for it.
struct Followed {
    state: InvocationState,
    /// Whether the device holds an answer until the invocation moves: one that answers at once is
    /// not asked to, so nobody spins on it.
    holds: bool,
    /// The held ask out now: when it was sent.
    asked: Option<Instant>,
    /// Waiting for the invocation to move.
    waiters: Vec<Completer<()>>,
}

/// What to answer once the lock on [`Waiting`] is released: completing one runs its caller's
/// continuation, which may ask again.
type Give = Box<dyn FnOnce() + Send>;

/// `asked` answered with `answer`, once the lock on [`Waiting`] is released.
fn giving<P: Send + 'static>(asked: Asked<P>, answer: P) -> Give {
    Box::new(move || asked.answer.complete(answer))
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
            give.push(giving(asked, Err(error.clone())));
        }
        for asked in self.receipts.drain().flat_map(|(_, all)| all) {
            give.push(giving(asked, Err(error.clone())));
        }
        for asked in self.updates.drain().flat_map(|(_, all)| all) {
            give.push(giving(asked, None));
        }
        for asked in self.cancels.drain().flat_map(|(_, all)| all) {
            give.push(giving(asked, Err(error.clone())));
        }
        for (_, asked) in self.puts.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
        for (_, asked) in self.gets.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
        for (_, asked) in self.confirms.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
    }

    /// The requests past their deadline, answered as unanswered.
    fn expire(&mut self, now: Instant, give: &mut Vec<Give>) {
        let late = || unavailable("the device did not answer in time");
        for asked in expired(&mut self.catalogues, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in split(&mut self.receipts, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in split(&mut self.updates, now) {
            give.push(giving(asked, None));
        }
        for asked in split(&mut self.cancels, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in expired(&mut self.puts, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in expired(&mut self.gets, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in expired(&mut self.confirms, now) {
            give.push(giving(asked, Err(late())));
        }
    }

    fn is_empty(&self) -> bool {
        self.catalogues.is_empty()
            && self.receipts.is_empty()
            && self.updates.is_empty()
            && self.cancels.is_empty()
            && self.puts.is_empty()
            && self.gets.is_empty()
            && self.confirms.is_empty()
    }

    /// The requests `message` answers.
    fn answer(&mut self, message: RemoteActionMessage, give: &mut Vec<Give>) {
        match message {
            RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            } => {
                if let Some(asked) = self.catalogues.remove(&correlation) {
                    give.push(giving(asked, result));
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
                    give.push(giving(asked, answer));
                }
            }
            // An update the device pushes unasked answers whoever waits for
            // one; with nobody waiting, the next observe asks again.
            RemoteActionMessage::Update { invocation, state } => {
                if let (Some(followed), Some(state)) = (self.follows.get_mut(&invocation), &state) {
                    followed.state = state.clone();
                }
                for asked in self.updates.remove(&invocation).unwrap_or_default() {
                    give.push(giving(asked, state.clone()));
                }
            }
            RemoteActionMessage::CancelResponse { invocation, result } => {
                for asked in self.cancels.remove(&invocation).unwrap_or_default() {
                    give.push(giving(asked, result.clone()));
                }
            }
            RemoteActionMessage::ContentPutResponse {
                correlation,
                result,
            } => {
                if let Some(asked) = self.puts.remove(&correlation) {
                    give.push(giving(asked, result));
                }
            }
            RemoteActionMessage::ContentGetResponse {
                correlation,
                result,
            } => {
                if let Some(asked) = self.gets.remove(&correlation) {
                    give.push(giving(asked, result));
                }
            }
            RemoteActionMessage::ConfirmResponse {
                correlation,
                result,
            } => {
                if let Some(asked) = self.confirms.remove(&correlation) {
                    give.push(giving(asked, result.map(|confirmed| *confirmed)));
                }
            }
            _ => {}
        }
    }
}

/// `answered`'s answer, if it came within [`REPLY_DEADLINE`].
#[cfg(not(target_arch = "wasm32"))]
fn in_time<T>(answered: tilo_actions::remote::Pending<T>) -> Option<T> {
    answered.wait_timeout(REPLY_DEADLINE).ok().flatten()
}

/// The requests of `all` past their deadline, taken out of it.
fn expired<K: std::hash::Hash + Eq + Clone, T>(
    all: &mut HashMap<K, Asked<T>>,
    now: Instant,
) -> Vec<Asked<T>> {
    let late: Vec<K> = all
        .iter()
        .filter(|(_, asked)| asked.until <= now)
        .map(|(key, _)| key.clone())
        .collect();
    late.into_iter()
        .filter_map(|key| all.remove(&key))
        .collect()
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

/// What `host` answers `message`, asked as `caller`: the request decoded, the answer as JSON.
///
/// # Errors
/// A text that is not a request, a host that abandoned the answer, or an answer that will not
/// encode.
pub async fn ask<H: ActionHost + Clone + Send + Sync + 'static>(
    host: &H,
    caller: &Caller,
    message: &str,
) -> Result<String, String> {
    let message: RemoteActionMessage =
        serde_json::from_str(message).map_err(|error| error.to_string())?;
    let answer = serve(host, caller, message)
        .ok_or_else(|| "that is not a request".to_owned())?
        .await
        .ok_or_else(|| "the answer was abandoned".to_owned())?;
    serde_json::to_string(&answer).map_err(|error| error.to_string())
}

impl SessionActions {
    /// Ask the device for `step` of an upload, waiting for its answer.
    #[cfg(not(target_arch = "wasm32"))]
    fn put_step(&self, step: tilo_actions::remote::ContentPutStep) -> Answered<ContentPutAnswer> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = pending();
        self.0
            .waiting
            .lock_recover()
            .puts
            .insert(correlation, Asked::new(answer));
        if let Err(error) = self
            .0
            .send(RemoteActionMessage::ContentPutRequest { correlation, step })
        {
            self.0.waiting.lock_recover().puts.remove(&correlation);
            return Err(error);
        }
        in_time(answered).unwrap_or_else(|| Err(unavailable("the device did not answer in time")))
    }

    /// Ask the device for a piece of `content`, waiting for its answer.
    #[cfg(not(target_arch = "wasm32"))]
    fn get_piece(&self, content: &str, offset: u64, length: u64) -> Answered<ContentGetAnswer> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = pending();
        self.0
            .waiting
            .lock_recover()
            .gets
            .insert(correlation, Asked::new(answer));
        let request = RemoteActionMessage::ContentGetRequest {
            correlation,
            content: content.to_owned(),
            offset,
            length,
        };
        if let Err(error) = self.0.send(request) {
            self.0.waiting.lock_recover().gets.remove(&correlation);
            return Err(error);
        }
        in_time(answered).unwrap_or_else(|| Err(unavailable("the device did not answer in time")))
    }

    /// The actions of the device `device` in `workspace_id`, asked through `session`.
    #[must_use]
    pub fn new(
        session: impl Into<Arc<dyn RemoteSession>>,
        workspace_id: u64,
        device: [u8; 32],
    ) -> Self {
        let session: Arc<dyn RemoteSession> = session.into();
        let inner = Arc::new(Inner {
            session: Arc::clone(&session),
            workspace_id,
            device,
            waiting: Mutex::default(),
            next_correlation: AtomicU64::new(1),
        });
        let woken = Arc::downgrade(&inner);
        session.on_change(Arc::new(move || {
            if let Some(inner) = woken.upgrade() {
                inner.pump();
            }
        }));
        sweep(Arc::downgrade(&inner));
        Self(inner)
    }

    /// The device's answer to observing `id`, waited for: callers off a page may block.
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

/// Answer requests past their deadline on the page's own thread.
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
    for give in give {
        give();
    }
    true
}

impl Inner {
    /// Take what the session received: the device's answers, and its unreachability.
    fn pump(&self) {
        let mut give = Vec::new();
        for heard in self.session.take_heard() {
            match heard {
                Heard::Action {
                    workspace_id,
                    from,
                    frame,
                } if workspace_id == self.workspace_id && from == self.device => {
                    let Ok(frame) = RemoteActionFrame::decode(&frame) else {
                        continue;
                    };
                    self.waiting.lock_recover().answer(frame.message, &mut give);
                }
                Heard::Undeliverable {
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
        for give in give {
            give();
        }
    }

    /// What the held ask for `id` was answered: keep it, and wake whoever waits.
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
        self.session
            .send_remote_action(self.workspace_id, self.device, frame)
            .map_err(|error| unavailable(format!("the session is closed: {error}")))
    }
}

impl ActionHost for SessionActions {
    /// Uploads into the content store of the device's workspace, step by step
    /// over the session: a caller off a page waits for each answer.
    #[cfg(not(target_arch = "wasm32"))]
    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        Some(Arc::new(puts::SessionPuts(self.clone())))
    }

    /// Content read out of the device's workspace's store, piece by piece
    /// over the session: a caller off a page waits for each answer.
    #[cfg(not(target_arch = "wasm32"))]
    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        Some(Arc::new(gets::SessionGets(self.clone())))
    }

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

    /// The invocation as the device last answered for it. A followed one is
    /// read from what the device last said only while that is still its
    /// word: once it finished, or while a held ask is out, which the device
    /// answers as soon as it moves. Otherwise the device is asked: an
    /// invocation accepted after its caller stopped following it (a write
    /// still being saved when the caller's wait ran out) moves with nobody
    /// asking, and what was kept would say accepted forever.
    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let kept = self
            .0
            .waiting
            .lock_recover()
            .follows
            .get(id)
            .map(|followed| {
                let current =
                    followed.state.is_terminal() || (followed.holds && followed.asked.is_some());
                (current, followed.state.clone())
            });
        match kept {
            Some((true, state)) => Some(state),
            #[cfg(target_arch = "wasm32")]
            kept => kept.map(|(_, state)| state),
            #[cfg(not(target_arch = "wasm32"))]
            _ => self.ask_state(id),
        }
    }

    /// Ask the device to answer once the followed invocation `id` moves;
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

    /// The person's confirmation for `request`, asked of the host the device is with: where they
    /// are put to them is the host's to say.
    fn confirm(
        &self,
        _caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Answered<ConfirmedInvocation>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = pending();
        self.0
            .waiting
            .lock_recover()
            .confirms
            .insert(correlation, Asked::new(answer));
        if let Err(error) = self.0.send(RemoteActionMessage::Confirm {
            correlation,
            request: Box::new(request.clone()),
        }) {
            let asked = self.0.waiting.lock_recover().confirms.remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        }
        answered
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tilo_actions::remote::TestHost;

    use super::*;

    /// A host shared with the continuation [`serve`] answers through: a test holds one handle to
    /// see what was asked, the door holds another.
    #[derive(Clone)]
    struct Shared(Arc<TestHost>);

    impl ActionHost for Shared {
        fn query(
            &self,
            caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            self.0.query(caller, query)
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            self.0.invoke(caller, request)
        }

        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.0.observe(caller, id)
        }

        fn cancel(
            &self,
            caller: &Caller,
            id: &InvocationId,
        ) -> Pending<Result<(), ActionHostError>> {
            self.0.cancel(caller, id)
        }
    }

    fn caller() -> Caller {
        Caller::local_person("person").in_workspace("workspace")
    }

    /// A page asks a catalogue and an invocation as the JSON a frame carries, and reads the
    /// answer as JSON: the door speaks the remote-action vocabulary, not one of its own.
    #[tokio::test]
    async fn a_request_is_asked_and_answered_as_the_frames_json() {
        let host = Arc::new(TestHost::new());
        let door = Shared(Arc::clone(&host));
        let answered = ask(
            &door,
            &caller(),
            r#"{"CatalogueRequest":{"correlation":7,"query":{"max_results":8}}}"#,
        )
        .await
        .expect("the door answers");
        let catalogue: serde_json::Value = serde_json::from_str(&answered).expect("JSON");
        assert_eq!(catalogue["CatalogueResponse"]["correlation"], 7);
        assert!(
            catalogue["CatalogueResponse"]["result"]["Ok"].is_object(),
            "a host that offers nothing still answers a page: {catalogue}"
        );

        let invocation = InvocationId::new();
        let request = serde_json::json!({"Invoke": {
            "invocation": invocation,
            "target": {"provider": "record:notes:one",
                        "address": {"resource": "notes.note:one", "action": "open"}},
        }});
        let answered = ask(&door, &caller(), &request.to_string())
            .await
            .expect("the door answers");
        let receipt: serde_json::Value = serde_json::from_str(&answered).expect("JSON");
        assert_eq!(
            receipt["Receipt"]["invocation"],
            serde_json::json!(invocation)
        );
        assert!(
            receipt["Receipt"]["status"]["Completed"].is_object(),
            "the host completed it: {receipt}"
        );
        assert_eq!(host.asked().len(), 1, "the host ran the invocation once");
    }

    /// A text that is not a request is refused instead of asked, and so is an answer to a
    /// request nobody made here.
    #[tokio::test]
    async fn only_a_request_is_asked() {
        let door = Shared(Arc::new(TestHost::new()));
        assert!(
            ask(&door, &caller(), "not a message").await.is_err(),
            "a text that is no message is refused"
        );
        assert!(
            ask(
                &door,
                &caller(),
                r#"{"CatalogueResponse":{"correlation":1,"result":"Ok"}}"#
            )
            .await
            .is_err(),
            "an answer is never asked as a request"
        );
    }

    /// A device that answers every ask at once, never holding an observe (as the host's actions
    /// do), from the state it is told the invocation is in; it counts the observes it was asked.
    struct Device {
        state: Mutex<InvocationUpdate>,
        heard: Mutex<Vec<Heard>>,
        wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
        observed: Mutex<usize>,
        holds: bool,
    }

    const DEVICE: [u8; 32] = [7; 32];

    impl Device {
        fn new(invocation: InvocationId, holds: bool) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(InvocationUpdate::Accepted { invocation }),
                heard: Mutex::default(),
                wake: Mutex::default(),
                observed: Mutex::default(),
                holds,
            })
        }

        fn say(&self, message: RemoteActionMessage) {
            let frame = RemoteActionFrame::new(message).encode().expect("encodes");
            self.heard.lock_recover().push(Heard::Action {
                workspace_id: 1,
                from: DEVICE,
                frame,
            });
            let wake = self.wake.lock_recover().clone();
            if let Some(wake) = wake {
                wake();
            }
        }
    }

    impl RemoteSession for Device {
        fn send_remote_action(&self, _: u64, _: [u8; 32], frame: Vec<u8>) -> Result<(), String> {
            let message = RemoteActionFrame::decode(&frame)
                .map_err(|error| error.to_string())?
                .message;
            match message {
                RemoteActionMessage::Invoke(request) => {
                    let state = self.state.lock_recover().clone();
                    self.say(RemoteActionMessage::Receipt(
                        InvocationState::new(request.invocation, state).receipt(),
                    ));
                }
                RemoteActionMessage::Observe {
                    invocation,
                    wait_ms,
                    ..
                } => {
                    let mut observed = self.observed.lock_recover();
                    *observed = observed.saturating_add(1);
                    drop(observed);
                    if self.holds && wait_ms.is_some() {
                        return Ok(());
                    }
                    let state = self.state.lock_recover().clone();
                    self.say(RemoteActionMessage::Update {
                        invocation,
                        state: Some(InvocationState::new(invocation, state)),
                    });
                }
                _ => {}
            }
            Ok(())
        }

        fn take_heard(&self) -> Vec<Heard> {
            std::mem::take(&mut *self.heard.lock_recover())
        }

        fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>) {
            *self.wake.lock_recover() = Some(wake);
        }
    }

    fn write(invocation: InvocationId) -> InvokeRequest {
        serde_json::from_value(serde_json::json!({
            "invocation": invocation,
            "target": {"provider": "record:notes:one",
                        "address": {"resource": "notes.note:one", "action": "append-paragraph"}},
        }))
        .expect("a request")
    }

    fn accepted(actions: &SessionActions, invocation: InvocationId) {
        let receipt = actions
            .invoke(&caller(), write(invocation))
            .wait_timeout(Duration::from_secs(5))
            .ok()
            .flatten()
            .expect("the device answers")
            .expect("the device takes it");
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }

    /// A write the device answered accepted (still being saved) after its caller stopped
    /// waiting, which it then finished with nobody following it: asked how it went, the device
    /// is asked, not the accepted kept from its receipt.
    #[test]
    fn an_invocation_nobody_follows_reads_as_the_device_says_it_ended() {
        let invocation = InvocationId::new();
        let device = Device::new(invocation, false);
        let actions = SessionActions::new(Arc::clone(&device) as Arc<dyn RemoteSession>, 1, DEVICE);
        accepted(&actions, invocation);

        *device.state.lock_recover() = InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        };
        let state = actions.observe(&caller(), &invocation).expect("a state");
        assert!(
            matches!(state.update, InvocationUpdate::Succeeded { .. }),
            "the write the device saved reads done: {:?}",
            state.update
        );
        let observed = *device.observed.lock_recover();
        actions.observe(&caller(), &invocation).expect("a state");
        assert_eq!(
            *device.observed.lock_recover(),
            observed,
            "what ended is read from what the device said"
        );
    }

    /// While a held ask is out the device answers as soon as the invocation moves: a wait that
    /// looks between its changes reads what the device said, without asking it again.
    #[test]
    fn a_held_ask_out_reads_what_the_device_said() {
        let invocation = InvocationId::new();
        let device = Device::new(invocation, true);
        let actions = SessionActions::new(Arc::clone(&device) as Arc<dyn RemoteSession>, 1, DEVICE);
        accepted(&actions, invocation);

        let change = actions.next_change(&caller(), &invocation);
        assert!(change.is_some(), "an accepted invocation is followed");
        let state = actions.observe(&caller(), &invocation).expect("a state");
        assert!(matches!(state.update, InvocationUpdate::Accepted { .. }));
        assert_eq!(
            *device.observed.lock_recover(),
            1,
            "only the held ask went to the device"
        );
    }
}
