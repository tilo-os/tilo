//! Another device's actions, asked through this device's session with the workspace's host: an
//! invocation frame addressed to a device's key is carried there, and its answer comes back on the
//! same session. [`SessionActions`] is that, as an [`ActionProvider`], so whatever drives a local action
//! host drives another device's instead. The answering device authenticates the session's device
//! and decides every request from that.
//!
//! A host brings its own session ([`RemoteSession`]): this desktop's peer actor, a page's endpoint,
//! a grant's session with its server. An invocation the device accepted and still runs is followed
//! here, so whoever waits is woken by the change itself rather than asking again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tilo_server_protocol::refusal::Refusal;

use collab_types::WorkspaceId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ChangePage, Completer,
    ContentGetAnswer, ContentLink, ContentLinkAsk, ContentPutAnswer, InvocationReceipt,
    InvocationState, InvokeRequest, MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, UpdateMark, WatchScope, pending, serve,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_clock::time::Instant;
use tilo_lock::MutexRecover as _;

#[cfg(not(target_arch = "wasm32"))]
mod gets;
#[cfg(not(target_arch = "wasm32"))]
mod puts;
pub mod replaced;
pub mod upload;
mod workspace;

pub use replaced::CurrentSession;

/// How long a request waits for the device's answer.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(30);

/// How often requests past their deadline are answered.
const SWEEP: Duration = Duration::from_millis(500);

/// An answer sooner than this with nothing new is a device that does not hold.
const HELD: Duration = Duration::from_secs(5);

/// How long a request the session could not carry waits before it is asked
/// again: long enough for the connection that answers it to be up.
const ASK_AGAIN_AFTER: Duration = Duration::from_millis(250);

/// The most times one set of waiting requests is asked again. A session that
/// loses its connection more often than this is not one to keep a caller's
/// request for: the request is answered, and the caller asks again.
const ASKED_AGAIN_AT_MOST: u8 = 3;

/// Why a session says it could not carry a request: it holds no connection to
/// its host right now. A connection that comes back carries what waited for it.
pub const NOT_CONNECTED: &str = "not connected to the host";

/// Whether a refused request is asked again instead of answered: the session
/// could not carry it, or the host was not ready for it, and nothing ran, so
/// the same request works once a connection that is the device's carries it.
const fn worth_asking_again(refusal: &Refusal) -> bool {
    refusal.passes_once_ready()
}

/// What a host's session heard for the asks here, since the last call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// A device answered, or another of the person's devices sent a request of its own.
    Action {
        workspace_id: WorkspaceId,
        /// The person the sending device was certified for, as the session carrying
        /// the frame authenticated it.
        user: [u8; 32],
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// The session could not carry a request to the device, and why.
    Undeliverable {
        workspace_id: WorkspaceId,
        to: [u8; 32],
        refusal: Refusal,
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
        workspace_id: WorkspaceId,
        device: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String>;
    /// Take everything the session heard since the last call.
    fn take_heard(&self) -> Vec<Heard>;
    /// Call `wake` whenever the session heard something or its state changed.
    fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>);

    /// Whether the session carries its requests now: it holds a connection to
    /// its host. A request it could not carry is asked again once it does.
    fn carrying(&self) -> bool {
        true
    }

    /// How many connections the session has had ([`Heard`]'s refusals name the
    /// one they came from): a request refused on one connection is asked again
    /// on a later one, never on the same.
    fn connections(&self) -> u64 {
        0
    }

    /// Who is in `workspace_id` now, as the session heard their presence, with the window each
    /// works in; `None` for a session that hears no presence.
    fn here(&self, workspace_id: WorkspaceId) -> Option<Vec<tilo_actions::remote::Here>> {
        let _ = workspace_id;
        None
    }

    /// Where `workspace_id` and this device's writes stand on the session; `None` for a session
    /// that does not say.
    fn session_state(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<tilo_actions::remote::SessionState> {
        let _ = workspace_id;
        None
    }

    /// Resolves the next time the session's states may have changed; `None` for a session that
    /// does not say.
    fn state_changed(&self) -> Option<Pending<()>> {
        None
    }
}

/// One device's actions in one workspace, asked through the session: the last clone dropped stops
/// listening.
#[derive(Clone)]
pub struct SessionActions(Arc<Inner>);

struct Inner {
    session: Arc<dyn RemoteSession>,
    workspace_id: WorkspaceId,
    device: [u8; 32],
    waiting: Mutex<Waiting>,
    next_correlation: AtomicU64,
    /// What the session could not carry, and when to ask it again.
    again: Mutex<Again>,
}

/// The waiting requests of a connection the session could not carry, as they
/// are asked again on the connection that does, or once the workspace moved on it.
#[derive(Default)]
struct Again {
    /// Where they were refused, and when to look for a change of it.
    refused_on: Option<RefusedOn>,
    /// How many times they were asked again.
    times: u8,
}

/// Where waiting requests were refused: the connection, and where the workspace stood on it.
#[derive(Clone, Copy)]
struct RefusedOn {
    connection: u64,
    workspace: Option<tilo_actions::remote::WorkspaceState>,
    not_before: Instant,
}

/// A request waiting for its answer, and when it gives up.
struct Asked<T> {
    answer: Completer<T>,
    until: Instant,
    /// The message it asks, for one the session could not carry and asks again.
    again: RemoteActionMessage,
}

impl<T> Asked<T> {
    /// Whether it is done waiting: past its deadline, or its caller stopped waiting for the
    /// answer (a call that gave up on its own wait). One its caller left is let go at once, so
    /// it is neither kept nor asked again on a later connection.
    fn done_by(&self, now: Instant) -> bool {
        self.until <= now || self.answer.is_abandoned()
    }

    fn new(answer: Completer<T>, again: RemoteActionMessage) -> Self {
        Self {
            answer,
            again,
            until: tilo_clock::now()
                .checked_add(REPLY_DEADLINE)
                .unwrap_or_else(tilo_clock::now),
        }
    }
}

type Answered<T> = Result<T, ActionCallError>;

#[derive(Default)]
struct Waiting {
    catalogues: HashMap<u64, Asked<Answered<CataloguePage>>>,
    receipts: HashMap<InvocationId, Vec<Asked<Answered<InvocationReceipt>>>>,
    /// What the device said of an invocation (`None`: nothing this caller may see), or why it
    /// said nothing.
    updates: HashMap<InvocationId, Vec<Asked<Answered<Option<InvocationState>>>>>,
    cancels: HashMap<InvocationId, Vec<Asked<Answered<()>>>>,
    follows: HashMap<InvocationId, Followed>,
    /// The followed invocations, oldest first: the ones that ended give way once there are more
    /// than [`FOLLOWED`].
    followed_order: std::collections::VecDeque<InvocationId>,
    puts: HashMap<u64, Asked<Answered<ContentPutAnswer>>>,
    gets: HashMap<u64, Asked<Answered<ContentGetAnswer>>>,
    links: HashMap<u64, Asked<Answered<ContentLink>>>,
    changes: HashMap<u64, Asked<Answered<ChangePage>>>,
    /// The waits for what a watch follows to change, by correlation.
    watched: HashMap<u64, Asked<Answered<()>>>,
    surfaces: HashMap<u64, Asked<Answered<Option<String>>>>,
    /// Record reads, by correlation: each answered with the readings as JSON text.
    reads: HashMap<u64, Asked<Answered<String>>>,
}

/// How many invocations a session's asker keeps following. An agent that invokes all day keeps
/// the ones that still run and the latest that ended; an older one that ended is asked of the
/// device again, as one nobody followed is.
const FOLLOWED: usize = 64;

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

/// What a caller is answered for a request the host would not carry: typed
/// where the caller can act on it (ask again once the workspace is ready, or
/// ask for access), the host's words otherwise.
fn refused(refusal: &Refusal) -> ActionCallError {
    match refusal {
        Refusal::NotReady => ActionCallError::NotReady,
        // A key its person revoked: the workspace's own words say what to do about it, which a
        // caller acts on, rather than that it may not.
        Refusal::NotAMember(reason) if reason.contains(tilo_actions::remote::KEY_REVOKED) => {
            ActionCallError::Refused {
                reason: reason.clone(),
            }
        }
        Refusal::NotAMember(_) | Refusal::NotAdmitted | Refusal::NotGranted(_) => {
            ActionCallError::NotPermitted
        }
        other => ActionCallError::unavailable(other.to_string()),
    }
}

impl Waiting {
    /// Every waiting request, answered with `error`.
    fn fail_all(&mut self, error: &ActionCallError, give: &mut Vec<Give>) {
        for (_, asked) in self.catalogues.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
        for asked in self.receipts.drain().flat_map(|(_, all)| all) {
            give.push(giving(asked, Err(error.clone())));
        }
        for asked in self.updates.drain().flat_map(|(_, all)| all) {
            give.push(giving(asked, Err(error.clone())));
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
        for (_, asked) in self.links.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
        for (_, asked) in self.changes.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
        for (_, asked) in self.watched.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
        for (_, asked) in self.surfaces.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
        for (_, asked) in self.reads.drain() {
            give.push(giving(asked, Err(error.clone())));
        }
    }

    /// The requests past their deadline, answered as unanswered, and those whose callers left.
    fn expire(&mut self, now: Instant, give: &mut Vec<Give>) {
        let late = || ActionCallError::Unanswered;
        for asked in expired(&mut self.catalogues, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in split(&mut self.receipts, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in split(&mut self.updates, now) {
            give.push(giving(asked, Err(late())));
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
        for asked in expired(&mut self.links, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in expired(&mut self.changes, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in expired(&mut self.watched, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in expired(&mut self.surfaces, now) {
            give.push(giving(asked, Err(late())));
        }
        for asked in expired(&mut self.reads, now) {
            give.push(giving(asked, Err(late())));
        }
    }

    /// Follow `invocation`, which the device accepted, letting the oldest that ended go past
    /// [`FOLLOWED`].
    fn follow(&mut self, invocation: InvocationId) {
        if self.follows.contains_key(&invocation) {
            return;
        }
        self.follows.insert(
            invocation,
            Followed {
                state: InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }),
                holds: true,
                asked: None,
                waiters: Vec::new(),
            },
        );
        self.followed_order.push_back(invocation);
        if self.follows.len() <= FOLLOWED {
            return;
        }
        let follows = &mut self.follows;
        self.followed_order.retain(|id| {
            let ended = follows
                .get(id)
                .is_none_or(|followed| followed.state.is_terminal() && followed.waiters.is_empty());
            if ended && follows.len() > FOLLOWED {
                follows.remove(id);
                return false;
            }
            follows.contains_key(id)
        });
    }

    fn is_empty(&self) -> bool {
        self.catalogues.is_empty()
            && self.receipts.is_empty()
            && self.updates.is_empty()
            && self.cancels.is_empty()
            && self.puts.is_empty()
            && self.gets.is_empty()
            && self.links.is_empty()
            && self.changes.is_empty()
            && self.watched.is_empty()
            && self.surfaces.is_empty()
            && self.reads.is_empty()
    }

    /// What every request still waiting asks: what is asked again on the
    /// connection that carries, after the one it was asked on did not.
    fn asked_again(&self) -> Vec<RemoteActionMessage> {
        let mut again = Vec::new();
        again.extend(self.catalogues.values().map(|asked| asked.again.clone()));
        again.extend(self.puts.values().map(|asked| asked.again.clone()));
        again.extend(self.gets.values().map(|asked| asked.again.clone()));
        again.extend(self.links.values().map(|asked| asked.again.clone()));
        again.extend(self.changes.values().map(|asked| asked.again.clone()));
        again.extend(self.watched.values().map(|asked| asked.again.clone()));
        again.extend(self.surfaces.values().map(|asked| asked.again.clone()));
        again.extend(self.reads.values().map(|asked| asked.again.clone()));
        again.extend(
            self.receipts
                .values()
                .flatten()
                .map(|asked| asked.again.clone()),
        );
        again.extend(
            self.cancels
                .values()
                .flatten()
                .map(|asked| asked.again.clone()),
        );
        again.extend(
            self.updates
                .values()
                .flatten()
                .map(|asked| asked.again.clone()),
        );
        again
    }

    /// The requests `message` answers.
    fn answer(&mut self, message: RemoteActionMessage, give: &mut Vec<Give>) {
        match message {
            RemoteActionMessage::Receipt(receipt) => self.receipt(&receipt, give),
            RemoteActionMessage::Update {
                invocation,
                state,
                correlation,
            } => {
                self.update(invocation, correlation, state.as_ref(), give);
            }
            RemoteActionMessage::CancelResponse { invocation, result } => {
                for asked in self.cancels.remove(&invocation).unwrap_or_default() {
                    give.push(giving(asked, result.clone()));
                }
            }
            other => self.answer_correlated(other, give),
        }
    }

    /// The request a response carrying its correlation answers.
    fn answer_correlated(&mut self, message: RemoteActionMessage, give: &mut Vec<Give>) {
        match message {
            RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            } => {
                if let Some(asked) = self.catalogues.remove(&correlation) {
                    give.push(giving(asked, result));
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
            RemoteActionMessage::ContentLinkResponse {
                correlation,
                result,
            } => {
                if let Some(asked) = self.links.remove(&correlation) {
                    give.push(giving(asked, result));
                }
            }
            RemoteActionMessage::ChangesResponse {
                correlation,
                result,
            } => {
                let asked = self.changes.remove(&correlation);
                give.extend(asked.map(|asked| giving(asked, result)));
            }
            RemoteActionMessage::ChangedResponse {
                correlation,
                result,
            } => {
                let asked = self.watched.remove(&correlation);
                give.extend(asked.map(|asked| giving(asked, result)));
            }
            RemoteActionMessage::SurfaceResponse {
                correlation,
                result,
            } => {
                let asked = self.surfaces.remove(&correlation);
                give.extend(asked.map(|asked| giving(asked, result)));
            }
            RemoteActionMessage::ResourceReading {
                correlation,
                result,
            } => {
                let asked = self.reads.remove(&correlation);
                give.extend(asked.map(|asked| giving(asked, result)));
            }
            _ => {}
        }
    }

    /// Whoever waits for `receipt`'s invocation.
    fn receipt(&mut self, receipt: &tilo_actions::remote::InvocationReceipt, give: &mut Vec<Give>) {
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

    /// An update the device sent for `invocation` answers the ask it names
    /// (`correlation`); one that names none — a push, or an answer from a
    /// provider that predates named asks — answers those it brings news to.
    /// The state is kept either way. An answer naming an ask this caller gave
    /// up on (its wait outlived the reply deadline) answers nothing: it may
    /// carry the state the caller already saw, which taken for the ask out
    /// now would read as a device that does not hold.
    fn update(
        &mut self,
        invocation: tilo_actions::InvocationId,
        correlation: Option<u64>,
        state: Option<&tilo_actions::remote::InvocationState>,
        give: &mut Vec<Give>,
    ) {
        if let (Some(followed), Some(state)) = (self.follows.get_mut(&invocation), state) {
            followed.state = state.clone();
        }
        let Some(all) = self.updates.remove(&invocation) else {
            return;
        };
        let (answered, kept): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|asked| answers(asked, correlation, state));
        if !kept.is_empty() {
            self.updates.insert(invocation, kept);
        }
        for asked in answered {
            give.push(giving(asked, Ok(state.cloned())));
        }
    }
}

/// Whether `asked`, a wait for an invocation's state, is answered by the update
/// that named `correlation` with `state`.
fn answers(
    asked: &Asked<Answered<Option<InvocationState>>>,
    correlation: Option<u64>,
    state: Option<&InvocationState>,
) -> bool {
    let RemoteActionMessage::Observe {
        correlation: named,
        seen,
        ..
    } = &asked.again
    else {
        return true;
    };
    if let Some(correlation) = correlation {
        return *named != 0 && *named == correlation;
    }
    state.is_none_or(|state| {
        state.is_terminal() || seen.is_none() || Some(UpdateMark::of(&state.update)) != *seen
    })
}

/// `answered`'s answer, if it came within [`REPLY_DEADLINE`].
#[cfg(not(target_arch = "wasm32"))]
fn in_time<T>(answered: tilo_actions::remote::Pending<T>) -> Option<T> {
    answered.wait_timeout(REPLY_DEADLINE).ok().flatten()
}

/// The requests of `all` past their deadline or left by their callers, taken out of it.
fn expired<K: std::hash::Hash + Eq + Clone, T>(
    all: &mut HashMap<K, Asked<T>>,
    now: Instant,
) -> Vec<Asked<T>> {
    let late: Vec<K> = all
        .iter()
        .filter(|(_, asked)| asked.done_by(now))
        .map(|(key, _)| key.clone())
        .collect();
    late.into_iter()
        .filter_map(|key| all.remove(&key))
        .collect()
}

/// The requests of `all` past their deadline or left by their callers, taken out of it.
fn split<T>(all: &mut HashMap<InvocationId, Vec<Asked<T>>>, now: Instant) -> Vec<Asked<T>> {
    let mut late = Vec::new();
    for waiting in all.values_mut() {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(waiting)
            .into_iter()
            .partition(|asked| asked.done_by(now));
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
pub async fn ask<H: ActionProvider + Clone + Send + Sync + 'static>(
    provider: &H,
    caller: &Caller,
    message: &str,
) -> Result<String, String> {
    let message: RemoteActionMessage =
        serde_json::from_str(message).map_err(|error| error.to_string())?;
    let answer = serve(provider, caller, message)
        .ok_or_else(|| "that is not a request".to_owned())?
        .await
        .ok_or_else(|| "the answer was abandoned".to_owned())?;
    serde_json::to_string(&answer).map_err(|error| error.to_string())
}

impl SessionActions {
    /// Admit and start one invocation on the session's connection, as
    /// [`ActionProvider::invoke`] does, for a caller that names what it asks without importing
    /// the provider trait.
    pub fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Answered<InvocationReceipt>> {
        ActionProvider::invoke(self, caller, request)
    }

    /// Wait on the device for the next change of what a watch of its workspace follows
    /// ([`ActionProvider::changed`]), asked as a held request: the device answers when a write
    /// lands in the workspace's records, or when who is in it moves.
    ///
    /// A device that cannot tell answers a refusal, and a session that cannot carry the
    /// request refuses it too: either way the wait is answered at once with why, never left
    /// for its caller's deadline to end.
    pub fn wait_for_change(&self, scope: WatchScope) -> Pending<Answered<()>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let message = RemoteActionMessage::ChangedRequest { correlation, scope };
        let (answer, answered) = pending();
        {
            let mut waiting = self.0.waiting.lock_recover();
            // A wait another source of its watch woke from is one nobody reads any more: forgotten
            // here rather than kept until a change answers it.
            waiting
                .watched
                .retain(|_, asked| !asked.answer.is_abandoned());
            waiting
                .watched
                .insert(correlation, Asked::new(answer, message.clone()));
        }
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().watched.remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        });
        answered
    }

    /// Read the record `read` names through the device's record API: the readings as JSON text
    /// (`{"revision", "records"}`), once they come. With `read.after`, the device answers once the
    /// record moves past that revision or `read.wait_ms` went by, so a reader follows the record
    /// with one held read at a time.
    pub fn read_record(
        &self,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Pending<Answered<String>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let waited = Duration::from_millis(u64::from(read.wait_ms.unwrap_or(0)));
        let message = RemoteActionMessage::ResourceRead {
            correlation,
            on_behalf_of: None,
            read,
        };
        let (answer, answered) = pending();
        let mut asked = Asked::new(answer, message.clone());
        asked.until = asked.until.checked_add(waited).unwrap_or(asked.until);
        self.0
            .waiting
            .lock_recover()
            .reads
            .insert(correlation, asked);
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().reads.remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        });
        answered
    }

    /// Ask the device for `step` of an upload: its answer, once it comes, or why none came within
    /// [`REPLY_DEADLINE`]. A page awaits it; a caller off one may block on it.
    pub fn put_step(
        &self,
        step: tilo_actions::remote::ContentPutStep,
    ) -> Pending<Answered<ContentPutAnswer>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = pending();
        let message = RemoteActionMessage::ContentPutRequest { correlation, step };
        self.0
            .waiting
            .lock_recover()
            .puts
            .insert(correlation, Asked::new(answer, message.clone()));
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().puts.remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        });
        answered
    }

    /// Ask the device for a link its web origin serves `link`'s files at
    /// ([`ContentLink`]): its answer, once it comes, or why none came within
    /// [`REPLY_DEADLINE`].
    pub fn content_link(&self, link: ContentLinkAsk) -> Pending<Answered<ContentLink>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = pending();
        let message = RemoteActionMessage::ContentLinkRequest { correlation, link };
        self.0
            .waiting
            .lock_recover()
            .links
            .insert(correlation, Asked::new(answer, message.clone()));
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().links.remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        });
        answered
    }

    /// [`Self::put_step`], waited for.
    #[cfg(not(target_arch = "wasm32"))]
    fn put_step_waited(
        &self,
        step: tilo_actions::remote::ContentPutStep,
    ) -> Answered<ContentPutAnswer> {
        in_time(self.put_step(step)).unwrap_or(Err(ActionCallError::Unanswered))
    }

    /// Ask the device for a piece of `content`, waiting for its answer.
    #[cfg(not(target_arch = "wasm32"))]
    fn get_piece(&self, content: &str, offset: u64, length: u64) -> Answered<ContentGetAnswer> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = pending();
        let message = RemoteActionMessage::ContentGetRequest {
            correlation,
            content: content.to_owned(),
            offset,
            length,
        };
        self.0
            .waiting
            .lock_recover()
            .gets
            .insert(correlation, Asked::new(answer, message.clone()));
        let mut failed = None;
        self.0.ask(message, |error| failed = Some(error));
        if let Some(error) = failed {
            return Err(error);
        }
        in_time(answered).unwrap_or(Err(ActionCallError::Unanswered))
    }

    /// The actions of the device `device` in `workspace_id`, asked through `session`.
    #[must_use]
    pub fn new(
        session: impl Into<Arc<dyn RemoteSession>>,
        workspace_id: WorkspaceId,
        device: [u8; 32],
    ) -> Self {
        let session: Arc<dyn RemoteSession> = session.into();
        let inner = Arc::new(Inner {
            session: Arc::clone(&session),
            workspace_id,
            device,
            waiting: Mutex::default(),
            next_correlation: AtomicU64::new(1),
            again: Mutex::default(),
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

    /// Ask the device how the followed invocation `id` stands, unless an ask for it is out
    /// already: its answer becomes what is kept of it ([`Waiting::update`]).
    #[cfg(any(target_arch = "wasm32", test))]
    fn ask_again(&self, id: &InvocationId) {
        let (answer, _answered) = pending();
        let message = RemoteActionMessage::Observe {
            correlation: self.0.next_correlation.fetch_add(1, Ordering::Relaxed),
            invocation: *id,
            wait_ms: None,
            seen: None,
        };
        {
            let mut waiting = self.0.waiting.lock_recover();
            if waiting.updates.contains_key(id) {
                return;
            }
            waiting
                .updates
                .entry(*id)
                .or_default()
                .push(Asked::new(answer, message.clone()));
        }
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().updates.remove(id);
            for asked in asked.unwrap_or_default() {
                asked.answer.complete(Err(error.clone()));
            }
        });
    }

    /// The device's answer to observing `id`, waited for: callers off a page may block.
    #[cfg(not(target_arch = "wasm32"))]
    fn ask_state(&self, id: &InvocationId) -> Option<InvocationState> {
        let (answer, answered) = pending();
        let message = RemoteActionMessage::Observe {
            correlation: self.0.next_correlation.fetch_add(1, Ordering::Relaxed),
            invocation: *id,
            wait_ms: None,
            seen: None,
        };
        self.0
            .waiting
            .lock_recover()
            .updates
            .entry(*id)
            .or_default()
            .push(Asked::new(answer, message.clone()));
        let mut failed = None;
        self.0.ask(message, |error| failed = Some(error));
        if failed.is_some() {
            return None;
        }
        answered
            .wait_timeout(REPLY_DEADLINE)
            .ok()
            .flatten()
            .and_then(Result::ok)
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

/// Answer what waited past its deadline, and ask again what the session could
/// not carry; whether the actions still exist.
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
        waiting.expire(tilo_clock::now(), &mut give);
    }
    for give in give {
        give();
    }
    inner.ask_again_if_due();
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
                    ..
                } if workspace_id == self.workspace_id && from == self.device => {
                    let Ok(frame) = RemoteActionFrame::decode(&frame) else {
                        continue;
                    };
                    self.waiting.lock_recover().answer(frame.message, &mut give);
                }
                Heard::Undeliverable {
                    workspace_id,
                    to,
                    refusal,
                } if workspace_id == self.workspace_id && to == self.device => {
                    // A host that was not ready ran nothing: what waits is
                    // asked again on a later connection. Any other refusal is
                    // the answer.
                    let kept = worth_asking_again(&refusal) && self.refused();
                    if !kept {
                        self.waiting
                            .lock_recover()
                            .fail_all(&refused(&refusal), &mut give);
                    }
                }
                _ => {}
            }
        }
        for give in give {
            give();
        }
        self.ask_again_if_due();
    }

    /// The session could not carry what waits: ask it again once a connection
    /// later than the one that refused it carries, or once the workspace moved
    /// on this one (a server that was not serving the session yet refused it, and
    /// says when it does). `false` when it was asked again as often as it may
    /// be, so the refusal is the answer.
    fn refused(&self) -> bool {
        let mut again = self.again.lock_recover();
        if again.refused_on.is_some() {
            return true;
        }
        if again.times >= ASKED_AGAIN_AT_MOST {
            return false;
        }
        let due = tilo_clock::now()
            .checked_add(ASK_AGAIN_AFTER)
            .unwrap_or_else(tilo_clock::now);
        again.refused_on = Some(RefusedOn {
            connection: self.session.connections(),
            workspace: self.workspace_state(),
            not_before: due,
        });
        true
    }

    /// Where the workspace stands on the session now, as it says.
    fn workspace_state(&self) -> Option<tilo_actions::remote::WorkspaceState> {
        self.session
            .session_state(self.workspace_id)
            .map(|state| state.workspace)
    }

    /// Ask what waits again, once a connection later than the one that refused
    /// it carries the session, or the workspace moved on it since: what the
    /// session says of its workspace is the signal, never a timer.
    fn ask_again_if_due(&self) {
        {
            let mut again = self.again.lock_recover();
            let Some(refused) = again.refused_on else {
                return;
            };
            let moved = self.session.connections() > refused.connection
                || self.workspace_state() != refused.workspace;
            if tilo_clock::now() < refused.not_before
                || again.times >= ASKED_AGAIN_AT_MOST
                || !self.session.carrying()
                || !moved
            {
                return;
            }
            again.refused_on = None;
            again.times = again.times.saturating_add(1);
        }
        // What its callers left, or what ran out of time, is answered and let go first: only
        // what someone still waits for is asked again.
        let mut give = Vec::new();
        let asked = {
            let mut waiting = self.waiting.lock_recover();
            waiting.expire(tilo_clock::now(), &mut give);
            waiting.asked_again()
        };
        for give in give {
            give();
        }
        for message in asked {
            let Err(error) = self.send(message) else {
                continue;
            };
            // The connection that carries now would not take them either: the
            // refusal stands, and this is the answer.
            let mut give = Vec::new();
            self.waiting.lock_recover().fail_all(&error, &mut give);
            for give in give {
                give();
            }
        }
    }

    /// What the held ask for `id` was answered: keep it, and wake whoever waits. An ask the
    /// device left unanswered (its wait outlived the reply deadline, or the session dropped)
    /// says nothing of whether it holds: the next wait asks it again. A follow that stopped
    /// holding there is read from what the device last said on a page, so nothing would move
    /// it again.
    fn followed(
        &self,
        id: InvocationId,
        seen: UpdateMark,
        answer: Answered<Option<InvocationState>>,
    ) {
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
        match answer {
            Ok(Some(state)) => {
                if quick && UpdateMark::of(&state.update) == seen {
                    followed.holds = false;
                }
                followed.state = state;
            }
            // No longer the caller's: nothing to hold.
            Ok(None) => followed.holds = false,
            Err(_) => {}
        }
        let waiters = std::mem::take(&mut followed.waiters);
        drop(waiting);
        for waiter in waiters {
            waiter.complete(());
        }
    }

    /// Send `message` to the device; `Err` when the session cannot carry it.
    fn send(&self, message: RemoteActionMessage) -> Result<(), ActionCallError> {
        let frame = RemoteActionFrame::new(message).encode().map_err(|error| {
            ActionCallError::InvalidRequest {
                reason: error.to_string(),
            }
        })?;
        self.session
            .send_remote_action(self.workspace_id, self.device, frame)
            .map_err(|error| {
                ActionCallError::unavailable(format!("the session is closed: {error}"))
            })
    }

    /// Ask the device `message`, `asked` waiting for its answer. A session that
    /// cannot carry it now asks it again once a connection that carries does
    /// (`refused`); any other failure is `fail`ed at once.
    fn ask(&self, message: RemoteActionMessage, fail: impl FnOnce(ActionCallError)) {
        match self.send(message) {
            Ok(()) => {}
            Err(error @ ActionCallError::Unavailable { .. }) => {
                // Asked again as often as it may be: the refusal is the answer.
                if !self.refused() {
                    fail(error);
                }
            }
            Err(error) => fail(error),
        }
    }
}

/// The device's record API, asked through the session: each read goes to the device whole, which
/// decides it as it decides every reader's.
#[cfg(not(target_arch = "wasm32"))]
impl tilo_actions::remote::ResourceReads for SessionActions {
    fn answer_read(
        self: Arc<Self>,
        _caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer {
        Box::pin(async move {
            RemoteActionMessage::ResourceReading {
                correlation,
                result: self
                    .read_record(read)
                    .await
                    .unwrap_or(Err(ActionCallError::Unanswered)),
            }
        })
    }
}

impl ActionProvider for SessionActions {
    /// Where the workspace these actions are asked in stands on the session.
    fn session_state(&self, _caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        self.0.session.session_state(self.0.workspace_id)
    }

    fn session_state_changed(&self, _caller: &Caller) -> Option<Pending<()>> {
        self.0.session.state_changed()
    }

    /// Who is in the workspace, and the window each works in, as the session heard them.
    fn here(&self, _caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.0.session.here(self.0.workspace_id)
    }

    /// The next change of what a watch of the device's workspace follows, asked of the device
    /// that holds it: its records (`WatchScope::Records`) or who is in it (`WatchScope::Here`).
    /// `None` for the roster: the session reads who is here when asked rather than telling
    /// when it moved, so a watch of presence waits until its deadline instead.
    fn changed(&self, _caller: &Caller, scope: WatchScope) -> Option<Pending<Answered<()>>> {
        match scope {
            WatchScope::Records => Some(self.wait_for_change(scope)),
            WatchScope::Here => None,
        }
    }

    /// The device's feed of its workspace's changes, asked over the session.
    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        Some(Arc::new(workspace::SessionChanges(self.clone())))
    }

    /// The device's record API, asked over the session: a read, a listing and a held wait each
    /// go to the device whole, as the record API's read message ([`Self::read_record`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(Arc::new(self.clone()))
    }

    /// The surface the workspace's package `app` publishes, asked of the
    /// device that holds the workspace: its types are readable without an
    /// app opening.
    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        let app = app_id.to_owned();
        self.ask(
            |waiting| &mut waiting.surfaces,
            move |correlation| RemoteActionMessage::SurfaceRequest { correlation, app },
        )
        .map(|answer| answer.ok().flatten().map(String::into_bytes))
    }

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
        let message = RemoteActionMessage::CatalogueRequest { correlation, query };
        self.0
            .waiting
            .lock_recover()
            .catalogues
            .insert(correlation, Asked::new(answer, message.clone()));
        self.0.ask(message, |error| {
            let asked = self
                .0
                .waiting
                .lock_recover()
                .catalogues
                .remove(&correlation);
            if let Some(asked) = asked {
                asked.answer.complete(Err(error));
            }
        });
        answered
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Answered<InvocationReceipt>> {
        let invocation = request.invocation;
        let (answer, answered) = pending();
        let message = RemoteActionMessage::Invoke(request);
        self.0
            .waiting
            .lock_recover()
            .receipts
            .entry(invocation)
            .or_default()
            .push(Asked::new(answer, message.clone()));
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().receipts.remove(&invocation);
            for asked in asked.unwrap_or_default() {
                asked.answer.complete(Err(error.clone()));
            }
        });
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
                inner.waiting.lock_recover().follow(invocation);
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
            // A page cannot wait for the device's answer: it reads what the device last said, and
            // asks it again so the next look reads what it answers.
            #[cfg(target_arch = "wasm32")]
            kept => {
                if kept.is_some() {
                    self.ask_again(id);
                }
                kept.map(|(_, state)| state)
            }
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
        followed.asked = Some(tilo_clock::now());
        let seen = UpdateMark::of(&followed.state.update);
        let (answer, answered) = pending();
        let wait_ms = u32::try_from(MAX_REMOTE_WAIT.as_millis()).unwrap_or(u32::MAX);
        let message = RemoteActionMessage::Observe {
            correlation: self.0.next_correlation.fetch_add(1, Ordering::Relaxed),
            invocation: *id,
            wait_ms: Some(wait_ms),
            seen: Some(seen),
        };
        waiting
            .updates
            .entry(*id)
            .or_default()
            .push(Asked::new(answer, message.clone()));
        drop(waiting);
        let inner = Arc::downgrade(&self.0);
        let invocation = *id;
        answered.then(move |state| {
            if let Some(inner) = inner.upgrade() {
                inner.followed(
                    invocation,
                    seen,
                    state.unwrap_or(Err(ActionCallError::Unanswered)),
                );
            }
        });
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().updates.remove(&invocation);
            for asked in asked.unwrap_or_default() {
                asked.answer.complete(Err(error.clone()));
            }
        });
        Some(change)
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Answered<()>> {
        let (answer, answered) = pending();
        let message = RemoteActionMessage::Cancel { invocation: *id };
        self.0
            .waiting
            .lock_recover()
            .cancels
            .entry(*id)
            .or_default()
            .push(Asked::new(answer, message.clone()));
        self.0.ask(message, |error| {
            let asked = self.0.waiting.lock_recover().cancels.remove(id);
            for asked in asked.unwrap_or_default() {
                asked.answer.complete(Err(error.clone()));
            }
        });
        answered
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tilo_action_services::TestProvider;
    use tilo_actions::remote::ContentPutStep;

    use super::*;

    /// A host shared with the continuation [`serve`] answers through: a test holds one handle to
    /// see what was asked, the entry holds another.
    #[derive(Clone)]
    struct Shared(Arc<TestProvider>);

    impl ActionProvider for Shared {
        fn query(
            &self,
            caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            self.0.query(caller, query)
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.0.invoke(caller, request)
        }

        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.0.observe(caller, id)
        }

        fn cancel(
            &self,
            caller: &Caller,
            id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            self.0.cancel(caller, id)
        }
    }

    fn caller() -> Caller {
        Caller::local_person("person").in_workspace("workspace")
    }

    /// A page asks a catalogue and an invocation as the JSON a frame carries, and reads the
    /// answer as JSON: the entry speaks the remote-action vocabulary, not one of its own.
    #[tokio::test]
    async fn a_request_is_asked_and_answered_as_the_frames_json() {
        let provider = Arc::new(TestProvider::new());
        let entry = Shared(Arc::clone(&provider));
        let answered = ask(
            &entry,
            &caller(),
            r#"{"CatalogueRequest":{"correlation":7,"query":{"max_results":8}}}"#,
        )
        .await
        .expect("the entry answers");
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
        let answered = ask(&entry, &caller(), &request.to_string())
            .await
            .expect("the entry answers");
        let receipt: serde_json::Value = serde_json::from_str(&answered).expect("JSON");
        assert_eq!(
            receipt["Receipt"]["invocation"],
            serde_json::json!(invocation)
        );
        assert!(
            receipt["Receipt"]["status"]["Completed"].is_object(),
            "the host completed it: {receipt}"
        );
        assert_eq!(
            provider.asked().len(),
            1,
            "the provider ran the invocation once"
        );
    }

    /// A text that is not a request is refused instead of asked, and so is an answer to a
    /// request nobody made here.
    #[tokio::test]
    async fn only_a_request_is_asked() {
        let entry = Shared(Arc::new(TestProvider::new()));
        assert!(
            ask(&entry, &caller(), "not a message").await.is_err(),
            "a text that is no message is refused"
        );
        assert!(
            ask(
                &entry,
                &caller(),
                r#"{"CatalogueResponse":{"correlation":1,"result":"Ok"}}"#
            )
            .await
            .is_err(),
            "an answer is never asked as a request"
        );
    }

    /// A device that answers every ask at once, never holding an observe or a wait for a change
    /// (as the host's actions do), from the state it is told the invocation is in; it counts the
    /// observes it was asked.
    struct Device {
        state: Mutex<InvocationUpdate>,
        heard: Mutex<Vec<Heard>>,
        wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
        observed: Mutex<usize>,
        /// How many files each many-files step carried.
        many: Mutex<Vec<usize>>,
        /// The uploads this device keeps open: the bytes each began with, and the bytes it holds.
        uploads: Mutex<HashMap<u64, (u64, u64)>>,
        /// The next upload this device gives an id.
        next_upload: Mutex<u64>,
        /// How many pieces of an upload are answered together, once all of them are here: a
        /// link whose round trip carries the pieces behind the one it answers. `None` answers
        /// each piece as it comes.
        answers_together: Mutex<Option<usize>>,
        /// The answers held for the pieces behind the first one, by correlation.
        held: Mutex<Vec<(u64, ContentPutAnswer)>>,
        holds: bool,
        /// Why this device refuses to tell when its workspace changes, when it does.
        refuses_changes: Mutex<Option<String>>,
    }

    const DEVICE: [u8; 32] = [7; 32];

    impl Device {
        fn new(invocation: InvocationId, holds: bool) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(InvocationUpdate::Accepted { invocation }),
                heard: Mutex::default(),
                wake: Mutex::default(),
                observed: Mutex::default(),
                many: Mutex::default(),
                uploads: Mutex::default(),
                next_upload: Mutex::default(),
                answers_together: Mutex::default(),
                held: Mutex::default(),
                holds,
                refuses_changes: Mutex::default(),
            })
        }

        /// Answer one step of an upload as this device's content store does, or hold it for the
        /// pieces behind it.
        fn put_step(&self, correlation: u64, step: ContentPutStep) -> Result<(), ActionCallError> {
            let refused = |reason: &str| ActionCallError::InvalidRequest {
                reason: reason.to_owned(),
            };
            let append = matches!(&step, ContentPutStep::Append { .. });
            let answer = match step {
                ContentPutStep::Many { files, .. } => {
                    self.many.lock_recover().push(files.len());
                    ContentPutAnswer::Many {
                        contents: (0..files.len())
                            .map(|at| format!("tilo.content:{at}"))
                            .collect(),
                    }
                }
                ContentPutStep::Begin { size, .. } => {
                    let upload = {
                        let mut next = self.next_upload.lock_recover();
                        *next = next.saturating_add(1);
                        *next
                    };
                    self.uploads.lock_recover().insert(upload, (size, 0));
                    ContentPutAnswer::Begun { upload }
                }
                ContentPutStep::Append {
                    upload,
                    offset,
                    bytes,
                } => {
                    let chunk =
                        hex::decode(bytes).map_err(|_| refused("a piece's bytes must be hex"))?;
                    let mut uploads = self.uploads.lock_recover();
                    let (size, written) = uploads
                        .get(&upload)
                        .copied()
                        .ok_or_else(|| refused("no upload is open"))?;
                    if offset != written {
                        return Err(refused("a piece does not continue the bytes held"));
                    }
                    let end = written.saturating_add(chunk.len() as u64);
                    if end > size {
                        return Err(refused("a piece lands past the upload's size"));
                    }
                    uploads.insert(upload, (size, end));
                    drop(uploads);
                    ContentPutAnswer::Appended { written: end }
                }
                ContentPutStep::Progress { upload } => {
                    let written = {
                        let uploads = self.uploads.lock_recover();
                        uploads
                            .get(&upload)
                            .map(|(_, written)| *written)
                            .ok_or_else(|| refused("no upload is open"))?
                    };
                    ContentPutAnswer::Appended { written }
                }
                ContentPutStep::Finish { upload } => {
                    let (size, written) = {
                        let mut uploads = self.uploads.lock_recover();
                        uploads
                            .remove(&upload)
                            .ok_or_else(|| refused("no upload is open"))?
                    };
                    if written != size {
                        return Err(refused("a finish takes a whole upload"));
                    }
                    ContentPutAnswer::Finished {
                        content: format!("tilo.content:{upload}"),
                    }
                }
                ContentPutStep::Have { hashes } => ContentPutAnswer::Have {
                    present: hashes.iter().map(|_| false).collect(),
                },
            };
            let together = *self.answers_together.lock_recover();
            let Some(together) = together.filter(|_| append) else {
                self.answered(correlation, answer);
                return Ok(());
            };
            let answers = {
                let mut held = self.held.lock_recover();
                held.push((correlation, answer));
                if held.len() < together {
                    return Ok(());
                }
                std::mem::take(&mut *held)
            };
            for (correlation, answer) in answers {
                self.answered(correlation, answer);
            }
            Ok(())
        }

        fn answered(&self, correlation: u64, answer: ContentPutAnswer) {
            self.say(RemoteActionMessage::ContentPutResponse {
                correlation,
                result: Ok(answer),
            });
        }

        fn say(&self, message: RemoteActionMessage) {
            let frame = RemoteActionFrame::new(message).encode().expect("encodes");
            self.heard.lock_recover().push(Heard::Action {
                workspace_id: WorkspaceId::from_u128(1),
                user: DEVICE,
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
        fn send_remote_action(
            &self,
            _: WorkspaceId,
            _: [u8; 32],
            frame: Vec<u8>,
        ) -> Result<(), String> {
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
                // A wait for what a watch follows: answered at once by a device that does not
                // hold, and kept unanswered by one that does, as a change still to come.
                RemoteActionMessage::ChangedRequest { correlation, .. } => {
                    let refused = self.refuses_changes.lock_recover().clone();
                    if let Some(reason) = refused {
                        self.say(RemoteActionMessage::ChangedResponse {
                            correlation,
                            result: Err(ActionCallError::Unavailable { reason }),
                        });
                        return Ok(());
                    }
                    if self.holds {
                        return Ok(());
                    }
                    self.say(RemoteActionMessage::ChangedResponse {
                        correlation,
                        result: Ok(()),
                    });
                }
                // An upload step: a many-files one stores each file and answers its
                // reference; an upload's begin, pieces and finish are kept as the device's own
                // content store keeps them, answers held for the pieces behind where it is told
                // to answer them together.
                RemoteActionMessage::ContentPutRequest { correlation, step } => {
                    if let Err(error) = self.put_step(correlation, step) {
                        self.say(RemoteActionMessage::ContentPutResponse {
                            correlation,
                            result: Err(error),
                        });
                    }
                }
                RemoteActionMessage::Observe {
                    correlation,
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
                        correlation: Some(correlation),
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

    /// A server that refused an ask because it did not serve the session yet, and then does.
    struct Admitting {
        serving: Mutex<bool>,
        asked: Mutex<usize>,
        heard: Mutex<Vec<Heard>>,
        wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl Admitting {
        fn hear(&self, heard: Heard) {
            self.heard.lock_recover().push(heard);
            self.woken();
        }

        fn woken(&self) {
            let wake = self.wake.lock_recover().clone();
            if let Some(wake) = wake {
                wake();
            }
        }
    }

    impl RemoteSession for Admitting {
        fn send_remote_action(
            &self,
            workspace_id: WorkspaceId,
            to: [u8; 32],
            frame: Vec<u8>,
        ) -> Result<(), String> {
            let message = RemoteActionFrame::decode(&frame)
                .map_err(|error| error.to_string())?
                .message;
            let RemoteActionMessage::CatalogueRequest { correlation, .. } = message else {
                return Ok(());
            };
            let mut asked = self.asked.lock_recover();
            *asked = asked.saturating_add(1);
            drop(asked);
            if !*self.serving.lock_recover() {
                self.hear(Heard::Undeliverable {
                    workspace_id,
                    to,
                    refusal: Refusal::NotReady,
                });
                return Ok(());
            }
            let answer = RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(CataloguePage::default()),
            };
            self.hear(Heard::Action {
                workspace_id,
                user: DEVICE,
                from: DEVICE,
                frame: RemoteActionFrame::new(answer).encode().expect("encodes"),
            });
            Ok(())
        }

        fn take_heard(&self) -> Vec<Heard> {
            std::mem::take(&mut *self.heard.lock_recover())
        }

        fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>) {
            *self.wake.lock_recover() = Some(wake);
        }

        fn session_state(&self, _: WorkspaceId) -> Option<tilo_actions::remote::SessionState> {
            Some(tilo_actions::remote::SessionState {
                workspace: if *self.serving.lock_recover() {
                    tilo_actions::remote::WorkspaceState::Serving
                } else {
                    tilo_actions::remote::WorkspaceState::Open
                },
                sync: tilo_actions::remote::SyncState::Done,
            })
        }
    }

    /// A listing the server refused because it did not serve the session yet is asked again on
    /// the same connection once the session says it does, and answered then: a computer's peer
    /// that just started asks before its workspace serves it, and its first call waited out
    /// its caller's whole wait instead.
    #[test]
    fn an_ask_refused_before_the_workspace_serves_the_session_is_answered_once_it_does() {
        let server = Arc::new(Admitting {
            serving: Mutex::new(false),
            asked: Mutex::default(),
            heard: Mutex::default(),
            wake: Mutex::default(),
        });
        let actions = SessionActions::new(
            Arc::clone(&server) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        let answered = actions.query(&caller(), CatalogueQuery::default());
        assert_eq!(
            *server.asked.lock_recover(),
            1,
            "asked once while not served"
        );
        *server.serving.lock_recover() = true;
        server.woken();
        let page = answered
            .wait_timeout(Duration::from_secs(5))
            .ok()
            .flatten()
            .expect("answered once the workspace serves the session");
        assert!(page.is_ok(), "{page:?}");
        assert_eq!(
            *server.asked.lock_recover(),
            2,
            "asked again once, when it served"
        );
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
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
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

    /// An agent that invokes all day through one session keeps following only so many of the
    /// invocations that ended: what it follows stays bounded however long it runs, and the
    /// latest that ended still reads from what the device said.
    #[test]
    fn invocations_that_ended_give_way_past_the_followed_bound() {
        let first = InvocationId::new();
        let device = Device::new(first, false);
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        let mut last = first;
        for _ in 0..FOLLOWED * 4 {
            last = InvocationId::new();
            *device.state.lock_recover() = InvocationUpdate::Accepted { invocation: last };
            accepted(&actions, last);
            *device.state.lock_recover() = InvocationUpdate::Succeeded {
                invocation: last,
                output: tilo_actions::ActionOutput::default(),
            };
            actions.observe(&caller(), &last).expect("a state");
        }
        let followed = actions.0.waiting.lock_recover().follows.len();
        assert!(
            followed <= FOLLOWED,
            "{followed} invocations followed, past {FOLLOWED}"
        );
        assert_eq!(
            actions.0.waiting.lock_recover().followed_order.len(),
            followed,
            "the order names exactly the followed invocations"
        );
        let observed = *device.observed.lock_recover();
        let state = actions.observe(&caller(), &last).expect("a state");
        assert!(matches!(state.update, InvocationUpdate::Succeeded { .. }));
        assert_eq!(
            *device.observed.lock_recover(),
            observed,
            "the latest that ended is still read from what the device said"
        );
    }

    /// A watch of a device's workspace is woken by the device's own change signal, and never by
    /// looking again: the device holds the wait until a write lands there, and a device that
    /// cannot tell refuses the wait, which answers it at once with why rather than leaving it to
    /// its caller's deadline.
    #[test]
    fn a_watch_is_woken_by_the_devices_own_change_signal() {
        let invocation = InvocationId::new();
        // The device holds the wait, so nothing answers it until the change comes.
        let holding = Device::new(invocation, true);
        let actions = SessionActions::new(
            Arc::clone(&holding) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        let changed = actions
            .changed(&caller(), WatchScope::Records)
            .expect("the device follows its records");
        assert!(
            changed.wait_timeout(Duration::from_millis(50)).is_err(),
            "a wait is answered when the workspace changes, not before"
        );

        // A device that answers the wait says the workspace changed, which wakes its watch.
        let answering = Device::new(invocation, false);
        let actions = SessionActions::new(
            Arc::clone(&answering) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        let changed = actions
            .changed(&caller(), WatchScope::Records)
            .expect("the device follows its records");
        assert_eq!(
            changed.wait_timeout(Duration::from_secs(5)).ok().flatten(),
            Some(Ok(())),
            "the change answers the wait"
        );

        // A device that cannot tell when its workspace changes refuses the wait, even one that
        // holds the waits it can follow, and the refusal answers the wait at once.
        let refusing = Device::new(invocation, true);
        *refusing.refuses_changes.lock_recover() =
            Some("this device carries no records of the workspace".into());
        let actions = SessionActions::new(
            Arc::clone(&refusing) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        let changed = actions
            .changed(&caller(), WatchScope::Records)
            .expect("the session asks the device");
        assert_eq!(
            changed.wait_timeout(Duration::from_secs(5)).ok().flatten(),
            Some(Err(ActionCallError::Unavailable {
                reason: "this device carries no records of the workspace".into()
            })),
            "the refusal answers the wait, with why"
        );
        assert!(
            actions.changed(&caller(), WatchScope::Here).is_none(),
            "a session that keeps no roster tells nothing of who is in the workspace"
        );
    }

    /// While a held ask is out the device answers as soon as the invocation moves: a wait that
    /// looks between its changes reads what the device said, without asking it again.
    #[test]
    fn a_held_ask_out_reads_what_the_device_said() {
        let invocation = InvocationId::new();
        let device = Device::new(invocation, true);
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
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

    /// A held ask the device leaves unanswered past the reply deadline (a live read of something
    /// nobody changed for a while) is asked again, and the follow keeps holding: what moves after
    /// it still reaches the follower, which a page reads only from what the device said.
    #[test]
    fn a_held_ask_left_unanswered_past_the_deadline_is_asked_again() {
        let invocation = InvocationId::new();
        let device = Device::new(invocation, true);
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        accepted(&actions, invocation);
        let change = actions
            .next_change(&caller(), &invocation)
            .expect("an accepted invocation is followed");

        // Nothing moved for longer than the device is waited for.
        let mut give = Vec::new();
        let past = tilo_clock::now()
            .checked_add(REPLY_DEADLINE.saturating_add(Duration::from_secs(1)))
            .expect("a later instant");
        actions.0.waiting.lock_recover().expire(past, &mut give);
        for give in give {
            give();
        }
        assert_eq!(
            change.wait_timeout(Duration::from_secs(5)).ok().flatten(),
            Some(()),
            "its waiter looks again"
        );
        let change = actions
            .next_change(&caller(), &invocation)
            .expect("the follow still holds");
        assert_eq!(
            *device.observed.lock_recover(),
            2,
            "the device is asked again"
        );

        // Then it moves: the follower hears it.
        let moved = InvocationUpdate::Item {
            invocation,
            sequence: 2,
            value: serde_json::json!({"accounts": ["ada@example.test"]}),
        };
        *device.state.lock_recover() = moved.clone();
        device.say(RemoteActionMessage::Update {
            invocation,
            state: Some(InvocationState::new(invocation, moved.clone())),
            correlation: None,
        });
        assert_eq!(
            change.wait_timeout(Duration::from_secs(5)).ok().flatten(),
            Some(()),
            "the move wakes the follower"
        );
        let state = actions.observe(&caller(), &invocation).expect("a state");
        assert_eq!(state.update, moved);
    }

    /// A follow of a device that does not hold is read, on a page, from what the device last
    /// said: each look asks it again, so what it said next is what the following look reads.
    #[test]
    fn a_follow_that_does_not_hold_keeps_what_the_device_says_next() {
        let invocation = InvocationId::new();
        let device = Device::new(invocation, false);
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        accepted(&actions, invocation);
        // Answered at once with nothing new: it does not hold.
        let change = actions
            .next_change(&caller(), &invocation)
            .expect("an accepted invocation is followed");
        assert_eq!(
            change.wait_timeout(Duration::from_secs(5)).ok().flatten(),
            Some(())
        );
        assert!(actions.next_change(&caller(), &invocation).is_none());

        let moved = InvocationUpdate::Item {
            invocation,
            sequence: 1,
            value: serde_json::json!({"accounts": []}),
        };
        *device.state.lock_recover() = moved.clone();
        actions.ask_again(&invocation);
        let kept = actions
            .0
            .waiting
            .lock_recover()
            .follows
            .get(&invocation)
            .map(|followed| followed.state.update.clone());
        assert_eq!(kept, Some(moved), "what the device said is kept");
    }

    /// An answer names the wait it answers: one naming a wait this caller gave
    /// up on (its answer came past the reply deadline) leaves the wait out now
    /// alone, however much the state it carries says. Taken for the wait out
    /// now, an answer the state has not moved past would read as a device that
    /// does not hold, and the follow would stop hearing its changes.
    #[test]
    fn an_update_answers_only_the_wait_it_names() {
        let invocation = InvocationId::new();
        let device = Device::new(invocation, true);
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        accepted(&actions, invocation);
        let mut change = actions
            .next_change(&caller(), &invocation)
            .expect("an accepted invocation is followed");
        let asked = actions
            .0
            .waiting
            .lock_recover()
            .updates
            .get(&invocation)
            .and_then(|all| all.first())
            .and_then(|asked| match &asked.again {
                RemoteActionMessage::Observe { correlation, .. } => Some(*correlation),
                _ => None,
            })
            .expect("the wait out names itself");

        let waiting_state =
            InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
        let mut give = Vec::new();
        actions.0.waiting.lock_recover().answer(
            RemoteActionMessage::Update {
                invocation,
                state: Some(waiting_state),
                correlation: Some(asked.wrapping_add(1_000)),
            },
            &mut give,
        );
        for give in give {
            give();
        }
        assert!(
            matches!(
                change.poll_ready(),
                tilo_actions::remote::PendingPoll::Waiting
            ),
            "another wait's answer does not answer this one"
        );
        assert!(
            actions.next_change(&caller(), &invocation).is_some(),
            "the follow still holds"
        );

        let moved = InvocationUpdate::Item {
            invocation,
            sequence: 2,
            value: serde_json::json!({"accounts": ["ada@example.test"]}),
        };
        *device.state.lock_recover() = moved.clone();
        let mut give = Vec::new();
        actions.0.waiting.lock_recover().answer(
            RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(invocation, moved.clone())),
                correlation: Some(asked),
            },
            &mut give,
        );
        for give in give {
            give();
        }
        assert_eq!(
            change.wait_timeout(Duration::from_secs(5)).ok().flatten(),
            Some(()),
            "its own answer wakes the follow"
        );
        assert_eq!(
            actions
                .observe(&caller(), &invocation)
                .expect("a state")
                .update,
            moved
        );
    }

    /// A caller that gave up on its answer leaves nothing behind: its ask is let go at the next
    /// sweep and is not asked again on a later connection, so the asks of calls that waited out
    /// a session without its server do not pile up and go out again all at once on its return.
    #[test]
    fn an_ask_its_caller_left_is_let_go_and_never_asked_again() {
        let observe = || RemoteActionMessage::Observe {
            correlation: 0,
            invocation: InvocationId::new(),
            wait_ms: None,
            seen: None,
        };
        let mut waiting = Waiting::default();
        let (left, answer) = pending();
        drop(answer);
        let (kept, _answer) = pending();
        waiting.catalogues.insert(1, Asked::new(left, observe()));
        waiting.catalogues.insert(2, Asked::new(kept, observe()));
        assert_eq!(waiting.asked_again().len(), 2, "both were asked");

        let mut give = Vec::new();
        waiting.expire(tilo_clock::now(), &mut give);
        assert_eq!(give.len(), 1, "the left one is let go before its deadline");
        assert_eq!(
            waiting.asked_again().len(),
            1,
            "only the ask someone still waits for goes again"
        );
        assert!(waiting.catalogues.contains_key(&2));
    }

    /// A folder of small files goes up in steps of at most
    /// [`tilo_actions::remote::MAX_PUT_MANY_FILES`] files: one step answers
    /// within the peer's reply deadline however many files there are.
    #[test]
    fn a_folder_of_small_files_goes_up_in_bounded_steps() {
        use tilo_actions::remote::MAX_PUT_MANY_FILES;

        let device = Device::new(InvocationId::new(), false);
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        let puts = actions.content_puts().expect("a device's content puts");
        let files = vec![vec![b'x']; MAX_PUT_MANY_FILES.saturating_add(1)];
        let contents = puts.put_many(&caller(), &files).expect("stored");
        assert_eq!(contents.len(), files.len(), "every file has its reference");
        assert_eq!(
            *device.many.lock_recover(),
            [MAX_PUT_MANY_FILES, 1],
            "the folder went in steps of at most {MAX_PUT_MANY_FILES} files"
        );
    }

    /// A piece of an upload travels to the device before the one ahead of it is answered: a
    /// device that answers the pieces together, once all of them are here, still takes the whole
    /// upload. A sender that waited for each piece's answer would never get past the first.
    #[test]
    fn an_uploads_pieces_travel_before_any_of_them_is_awaited() {
        use tilo_actions::remote::MAX_CONTENT_FRAME_BYTES;

        let device = Device::new(InvocationId::new(), false);
        let actions = SessionActions::new(
            Arc::clone(&device) as Arc<dyn RemoteSession>,
            WorkspaceId::from_u128(1),
            DEVICE,
        );
        // Three pieces, answered together once the third is here.
        *device.answers_together.lock_recover() = Some(3);
        let bytes = vec![b'x'; 2 * MAX_CONTENT_FRAME_BYTES + 1];
        let mut put = actions
            .content_puts()
            .expect("a device's content puts")
            .begin(&caller(), bytes.len() as u64)
            .expect("an upload begins");
        put.append(0, &bytes).expect("every piece is sent");
        let content = put.finish().expect("the upload is kept whole");
        assert!(content.starts_with("tilo.content:"), "{content}");
        assert!(
            device.held.lock_recover().is_empty(),
            "every piece was answered, none left behind"
        );
    }
}
