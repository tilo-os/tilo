//! The action API of whichever session a device holds now, for a caller that outlives each one.
//!
//! A page keeps one id while the session under it is replaced (it certified its device again, and
//! started a new session with the new certificate). A window mounted before that asks the same
//! API it always did, and what it follows (a live reading of the person's accounts, an
//! invocation still running) was admitted on a session that is gone: the new session's server
//! never heard of it, and answers an observe of it with nothing, which a window reads as an
//! invocation that does not exist, and keeps the last answer it had. What a window asked while
//! the page held no session at all was refused at once, and the window kept that refusal.
//!
//! [`CurrentSession`] stands between such callers and the session: it looks the session up for
//! each request, notes which session (its generation) admitted each invocation, and asks every
//! invocation that is still going of the session that replaced the one it was asked of, under the
//! same id. Asking an id again is the providers' own rule for an answer that went missing
//! ([`ActionProvider`]: the same request under the same id reports the original), so nothing
//! that began already starts twice. Until the new session answers, the caller is told what it was
//! told last, and a live result's sequence goes on from where the earlier session's ended, so each
//! new result reads as new. What is asked while no session carries it waits for the next one, for
//! [`HOLD`]: a session is replaced in seconds, and a page that holds none for longer says so
//! itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus, pending,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_clock::time::Instant;
use tilo_lock::MutexRecover as _;

/// A provider and the generation of the session it asks through.
pub type Reached = (u64, Arc<dyn ActionProvider + Send + Sync>);

/// How long a caller waiting on an invocation whose session is gone, or not answering for it
/// yet, is left before it looks again: a session is replaced within a few seconds.
const LOOK_AGAIN: Duration = Duration::from_millis(500);

/// How long what was asked while no session carried it waits for one before it is refused.
pub const HOLD: Duration = Duration::from_secs(20);

/// How many invocations are kept to be asked again: more than any page's windows follow at once.
const KEPT: usize = 256;

/// An invocation asked through a session, kept until it ends.
struct Asked {
    caller: Caller,
    request: InvokeRequest,
    /// The generation of the session it was last asked of; `None` while no session took it.
    generation: Option<u64>,
    /// Since when it waits for a session to carry it, while it does.
    waiting: Option<Instant>,
    /// What the caller was told last.
    last: Option<InvocationState>,
    /// Added to each sequence the current session gives its results: the highest the sessions
    /// before it gave, so a result after a replacement never reads as one already seen.
    offset: u64,
    /// The highest sequence the caller was told.
    top: u64,
}

type Kept = Arc<Mutex<HashMap<InvocationId, Asked>>>;
type Refusal = Arc<dyn Fn() -> ActionCallError + Send + Sync>;

/// The actions of the session `reach` answers now, for callers that keep asking across its
/// replacement; `unreached` says why nothing was asked while there is none.
pub struct CurrentSession {
    reach: Box<dyn Fn() -> Option<Reached> + Send + Sync>,
    unreached: Refusal,
    asked: Kept,
}

/// Whether `error` says the session did not carry the request, rather than that its server
/// refused it: nothing ran, and the next session carries it.
fn not_carried(error: &ActionCallError, unreached: &ActionCallError) -> bool {
    error == unreached
        || matches!(error, ActionCallError::Unavailable { reason } if reason == crate::NOT_CONNECTED)
}

impl CurrentSession {
    /// Ask through whatever session `reach` answers, with its generation: a number that changes
    /// each time a new session takes the place of the one before. `unreached` is the refusal of
    /// a request no session carried within [`HOLD`].
    pub fn new(
        reach: impl Fn() -> Option<Reached> + Send + Sync + 'static,
        unreached: impl Fn() -> ActionCallError + Send + Sync + 'static,
    ) -> Self {
        Self {
            reach: Box::new(reach),
            unreached: Arc::new(unreached),
            asked: Arc::default(),
        }
    }

    /// Keep `request` to be asked again, asked now of the session of `generation` if any.
    fn keep(&self, caller: &Caller, request: &InvokeRequest, generation: Option<u64>) {
        let mut asked = self.asked.lock_recover();
        if asked.len() >= KEPT {
            // The oldest are no longer followed by anyone who would notice.
            let forgotten: Vec<InvocationId> = asked
                .keys()
                .take(asked.len().saturating_add(1).saturating_sub(KEPT))
                .copied()
                .collect();
            for id in forgotten {
                asked.remove(&id);
            }
        }
        asked.insert(
            request.invocation,
            Asked {
                caller: caller.clone(),
                request: request.clone(),
                generation,
                waiting: generation.is_none().then(tilo_clock::now),
                last: None,
                offset: 0,
                top: 0,
            },
        );
    }

    /// Ask `request` of `api`: what its receipt says is kept for the caller's next look, and a
    /// session that did not carry it leaves it waiting for the next one.
    fn ask(
        &self,
        api: &Arc<dyn ActionProvider + Send + Sync>,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (kept, unreached) = (Arc::clone(&self.asked), Arc::clone(&self.unreached));
        let id = request.invocation;
        api.invoke(caller, request).map(move |receipt| {
            let mut asked = kept.lock_recover();
            match receipt {
                Ok(receipt) => {
                    match ended(id, receipt.status.clone()) {
                        // Running: followed on this session from now on.
                        None => {}
                        Some(update) => {
                            if let Some(asked) = asked.get_mut(&id) {
                                asked.last = Some(InvocationState::new(id, update));
                            }
                        }
                    }
                    drop(asked);
                    Ok(receipt)
                }
                Err(error) if not_carried(&error, &unreached()) && asked.contains_key(&id) => {
                    if let Some(asked) = asked.get_mut(&id) {
                        asked.generation = None;
                        asked.waiting.get_or_insert_with(tilo_clock::now);
                    }
                    drop(asked);
                    Ok(accepted(id))
                }
                Err(error) => {
                    if let Some(asked) = asked.get_mut(&id) {
                        asked.last = Some(InvocationState::new(id, refused(id, &error)));
                    }
                    drop(asked);
                    Err(error)
                }
            }
        })
    }

    /// The session to ask about `id` now: one that replaced the session `id` was asked of, or the
    /// first to come after none, is asked for it first, under the same id.
    fn reach_for(&self, id: &InvocationId) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        let (generation, api) = (self.reach)()?;
        let again = {
            let mut asked = self.asked.lock_recover();
            asked.get_mut(id).and_then(|asked| {
                (asked.generation != Some(generation)).then(|| {
                    asked.generation = Some(generation);
                    asked.waiting = None;
                    asked.offset = asked.top;
                    (asked.caller.clone(), asked.request.clone())
                })
            })
        };
        if let Some((caller, request)) = again {
            // Its answer is what the caller observes next.
            self.ask(&api, &caller, request).then(drop);
        }
        Some(api)
    }

    /// Whether `id` is an invocation asked here that has not ended.
    fn going(&self, id: &InvocationId) -> bool {
        self.asked.lock_recover().get(id).is_some_and(|asked| {
            !asked
                .last
                .as_ref()
                .is_some_and(InvocationState::is_terminal)
        })
    }

    /// `state`, the current session's word on `id`, as its caller is told it: numbered on from
    /// the sessions before, and kept as the last word, or forgotten once it ends.
    fn told(&self, id: &InvocationId, state: InvocationState) -> InvocationState {
        let mut asked = self.asked.lock_recover();
        let Some(kept) = asked.get_mut(id) else {
            return state;
        };
        let mut state = state;
        let offset = kept.offset;
        match &mut state.update {
            InvocationUpdate::Progress { sequence, .. }
            | InvocationUpdate::Item { sequence, .. } => {
                *sequence = sequence.saturating_add(offset);
                kept.top = kept.top.max(*sequence);
            }
            _ => {}
        }
        if state.is_terminal() {
            asked.remove(id);
        } else {
            kept.last = Some(state.clone());
        }
        drop(asked);
        state
    }

    /// What `id`'s caller was told last, or that it was accepted, where the session asked now
    /// has no word on it yet; refused once it waited for a session past [`HOLD`]. `None` for an
    /// invocation not asked here.
    fn last(&self, id: &InvocationId) -> Option<InvocationState> {
        let mut asked = self.asked.lock_recover();
        let kept = asked.get(id)?;
        let waited_out = kept
            .waiting
            .is_some_and(|since| tilo_clock::now().saturating_duration_since(since) >= HOLD);
        let last = if waited_out {
            InvocationState::new(*id, refused(*id, &(self.unreached)()))
        } else {
            kept.last
                .clone()
                .unwrap_or_else(|| InvocationState::new(*id, accepted_update(*id)))
        };
        // One that ended while no caller looked is told once, then forgotten.
        if last.is_terminal() {
            asked.remove(id);
        }
        drop(asked);
        Some(last)
    }
}

fn accepted_update(id: InvocationId) -> InvocationUpdate {
    InvocationUpdate::Accepted { invocation: id }
}

fn accepted(id: InvocationId) -> InvocationReceipt {
    InvocationReceipt {
        invocation: id,
        status: ReceiptStatus::Accepted,
    }
}

/// The update an invocation that ended at its receipt ended with; `None` while it goes on.
fn ended(id: InvocationId, status: ReceiptStatus) -> Option<InvocationUpdate> {
    match status {
        ReceiptStatus::Accepted => None,
        ReceiptStatus::Completed { output } => Some(InvocationUpdate::Succeeded {
            invocation: id,
            output,
        }),
        ReceiptStatus::Failed { error } => Some(InvocationUpdate::Failed {
            invocation: id,
            error,
        }),
        ReceiptStatus::Rejected { error } => Some(refused(id, &error)),
        ReceiptStatus::Cancelled { message } => Some(InvocationUpdate::Cancelled {
            invocation: id,
            message,
        }),
    }
}

/// An invocation a session would not take, as its caller is told.
fn refused(id: InvocationId, error: &ActionCallError) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation: id,
        error: ActionFailure {
            code: error.code().to_owned(),
            message: error.to_string(),
            details: None,
            retryable: true,
        },
    }
}

/// Resolves after `pause`, on the page's own thread or a thread of its own.
fn after(pause: Duration) -> Pending<()> {
    let (done, waited) = pending();
    #[cfg(target_arch = "wasm32")]
    n0_future::task::spawn(async move {
        n0_future::time::sleep(pause).await;
        done.complete(());
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        let spawned = std::thread::Builder::new()
            .name("current-session-look-again".into())
            .spawn(move || {
                std::thread::sleep(pause);
                done.complete(());
            });
        // Without the thread, the waiter looks again at its own deadline.
        drop(spawned);
    }
    waited
}

impl ActionProvider for CurrentSession {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        match (self.reach)() {
            Some((_, api)) => api.query(caller, query),
            None => Pending::ready(Err((self.unreached)())),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let reached = (self.reach)();
        self.keep(
            caller,
            &request,
            reached.as_ref().map(|(generation, _)| *generation),
        );
        let Some((_, api)) = reached else {
            // No session now: the next one carries it, and the caller follows it as running.
            return Pending::ready(Ok(accepted(request.invocation)));
        };
        let id = request.invocation;
        let kept = Arc::clone(&self.asked);
        self.ask(&api, caller, request).map(move |receipt| {
            // One that ended at its receipt, or was refused, is nothing to ask again.
            let running = matches!(
                &receipt,
                Ok(InvocationReceipt {
                    status: ReceiptStatus::Accepted,
                    ..
                })
            );
            if !running {
                kept.lock_recover().remove(&id);
            }
            receipt
        })
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let Some(api) = self.reach_for(id) else {
            return self.last(id);
        };
        match api.observe(caller, id) {
            Some(state) => Some(self.told(id, state)),
            None => self.last(id),
        }
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let change = self
            .reach_for(id)
            .and_then(|api| api.next_change(caller, id));
        change.or_else(|| self.going(id).then(|| after(LOOK_AGAIN)))
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let api = self.reach_for(id);
        let waited = self
            .asked
            .lock_recover()
            .remove(id)
            .is_some_and(|asked| asked.generation.is_none());
        match api {
            Some(api) => api.cancel(caller, id),
            // Nothing carried it anywhere: it is stopped by being forgotten.
            None if waited => Pending::ready(Ok(())),
            None => Pending::ready(Err((self.unreached)())),
        }
    }
}

#[cfg(test)]
#[path = "replaced_tests.rs"]
mod tests;
