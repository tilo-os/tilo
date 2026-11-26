//! What one tool step of a turn cost, said in the server's log.
//!
//! A turn's model asks for tools, and each call of one is a step. A tool asks the workspace's action service for what it
//! needs: listings, the admission of an action and how that action ended, a search. Each of
//! those is counted and timed where the service is asked ([`asking`], [`looked`]), for the call
//! it is asked within ([`Step::carried_out`]), and when the step ends one line says all of it
//! ([`Step::say`]), as the model gateway says what a call of the model cost. So a server's own
//! log answers where a turn's time went between two calls of its model.
//!
//! The line holds names, counts, times and sizes. The tool's name is one a server's tools can
//! have, or `other`; nothing a tool was called with, nothing it answered and no word of a
//! person's is in it.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_actions::remote::{InvocationState, Pending, ReceiptStatus, RemoteActionMessage};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_agent_turn::logged::offered_name;
use tilo_lock::MutexRecover as _;
use tokio::task::futures::TaskLocalFuture;

tokio::task_local! {
    /// What the tool call being carried out has asked of the workspace's action service.
    static ASKED: Arc<Mutex<Asked>>;
}

/// What one tool call asked of the workspace's action service, and how long it waited for it.
#[derive(Default)]
pub(super) struct Asked {
    lists: u32,
    listing: Duration,
    admits: u32,
    admitting: Duration,
    searches: u32,
    searching: Duration,
    looks: u32,
    /// The invocations admitted and not yet seen ended, each with when its receipt came.
    followed: Vec<(InvocationId, Instant)>,
    following: Duration,
    /// Whether an action of it needs its person's answer first.
    needs_person: bool,
}

impl Asked {
    /// Invocation `id` is seen ended, or its tool stopped waiting: it is followed no longer.
    fn ended(&mut self, id: &InvocationId) {
        if let Some(at) = self
            .followed
            .iter()
            .position(|(followed, _)| followed == id)
        {
            let (_, since) = self.followed.swap_remove(at);
            self.following = self.following.saturating_add(since.elapsed());
        }
    }
}

/// Whether `failure` says its action waits for its person's answer.
fn needs_person(failure: &ActionFailure) -> bool {
    failure.code == tilo_actions::api::ApiError::ConfirmationRequired.code()
}

/// `future`, counted for the tool call the calling task carries out, if any: a task it is
/// spawned into is not that call's task, and what it asks of the workspace's action service
/// would otherwise count for no step.
pub(super) fn carried<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let asked = ASKED.try_with(Arc::clone).ok();
    async move {
        match asked {
            Some(asked) => ASKED.scope(asked, future).await,
            None => future.await,
        }
    }
}

/// What a request to the workspace's action service is counted as.
enum Kind {
    List,
    Admit,
    Search,
}

/// One request to the workspace's action service made within a tool call, until it is answered.
pub(super) struct Asking {
    asked: Arc<Mutex<Asked>>,
    kind: Kind,
    began: Instant,
}

/// `message`, about to go to the workspace's action service: what counts it for its tool call
/// when the answer comes. `None` outside a tool call, and for a message that asks for none of
/// a listing, an admission or a search.
pub(super) fn asking(message: &RemoteActionMessage) -> Option<Asking> {
    let kind = match message {
        RemoteActionMessage::CatalogueRequest { .. } => Kind::List,
        RemoteActionMessage::Invoke(request)
            if tilo_actions::remote::search::is_find(&request.target) =>
        {
            Kind::Search
        }
        RemoteActionMessage::Invoke(_) => Kind::Admit,
        _ => return None,
    };
    let asked = ASKED.try_with(Arc::clone).ok()?;
    Some(Asking {
        asked,
        kind,
        began: tilo_clock::now(),
    })
}

impl Asking {
    /// `answer`, counted for its tool call when it comes.
    pub(super) fn answered(
        self,
        answer: Pending<RemoteActionMessage>,
    ) -> Pending<RemoteActionMessage> {
        answer.map(move |answer| {
            self.count(&answer);
            answer
        })
    }

    fn count(self, answer: &RemoteActionMessage) {
        let took = self.began.elapsed();
        let mut asked = self.asked.lock_recover();
        match self.kind {
            Kind::List => {
                asked.lists = asked.lists.saturating_add(1);
                asked.listing = asked.listing.saturating_add(took);
            }
            Kind::Search => {
                asked.searches = asked.searches.saturating_add(1);
                asked.searching = asked.searching.saturating_add(took);
            }
            Kind::Admit => {
                asked.admits = asked.admits.saturating_add(1);
                asked.admitting = asked.admitting.saturating_add(took);
                if let RemoteActionMessage::Receipt(receipt) = answer {
                    match &receipt.status {
                        ReceiptStatus::Accepted => {
                            asked.followed.push((receipt.invocation, tilo_clock::now()));
                        }
                        ReceiptStatus::Failed { error } if needs_person(error) => {
                            asked.needs_person = true;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// [`looked`], as the turn's server is told of each look: what a step counts
/// for the tool call it waits within.
pub(super) fn observed() -> tilo_mcp::Observation {
    std::sync::Arc::new(looked)
}

/// A look at how invocation `id` stands, made within a tool call: counted, and where it shows
/// the invocation ended, how long its end was waited for is known.
pub(super) fn looked(id: &InvocationId, state: Option<&InvocationState>) {
    let Ok(asked) = ASKED.try_with(Arc::clone) else {
        return;
    };
    let mut asked = asked.lock_recover();
    asked.looks = asked.looks.saturating_add(1);
    let Some(state) = state.filter(|state| state.update.is_terminal()) else {
        return;
    };
    asked.ended(id);
    if let InvocationUpdate::Failed { error, .. } = &state.update
        && needs_person(error)
    {
        asked.needs_person = true;
    }
}

fn millis(time: Duration) -> u64 {
    u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
}

/// One tool step of a turn, timed from when it began.
pub(super) struct Step {
    tool: String,
    /// Which call of its answer it is, counted from one, and how many that answer holds.
    place: (usize, usize),
    began: Instant,
    asked: Arc<Mutex<Asked>>,
}

impl Step {
    /// The step that calls `tool`, the call at `nth` of the `of` its answer holds, beginning
    /// now.
    pub(super) fn begin(tool: &str, (nth, of): (usize, usize)) -> Self {
        Self {
            tool: offered_name(tool).to_owned(),
            place: (nth.saturating_add(1), of),
            began: tilo_clock::now(),
            asked: Arc::default(),
        }
    }

    /// `call`, the tool itself: what it asks of the workspace's action service is counted for
    /// this step.
    pub(super) fn carried_out<F: Future>(&self, call: F) -> TaskLocalFuture<Arc<Mutex<Asked>>, F> {
        ASKED.scope(Arc::clone(&self.asked), call)
    }

    /// Say what the step cost: its tool answered `bytes`, and `failed` or not.
    pub(super) fn say(self, bytes: usize, failed: bool) {
        let tool = self.began.elapsed();
        let mut asked = self.asked.lock_recover();
        // An action still followed when its tool answered has not ended: the tool stopped
        // waiting for it, and what it waited is the time until now.
        let unended: Vec<InvocationId> = asked.followed.iter().map(|(id, _)| *id).collect();
        for id in &unended {
            asked.ended(id);
        }
        let service = asked
            .listing
            .saturating_add(asked.admitting)
            .saturating_add(asked.following)
            .saturating_add(asked.searching);
        let outcome = if failed { "failed" } else { "ok" };
        tracing::info!(
            target: "tilo_app_host::agents",
            tool = %self.tool,
            call = self.place.0,
            calls = self.place.1,
            %outcome,
            tool_ms = millis(tool),
            lists = asked.lists,
            list_ms = millis(asked.listing),
            admits = asked.admits,
            admit_ms = millis(asked.admitting),
            looks = asked.looks,
            follow_ms = millis(asked.following),
            searches = asked.searches,
            search_ms = millis(asked.searching),
            own_ms = millis(tool.saturating_sub(service)),
            result_bytes = bytes,
            needs_person = asked.needs_person,
            pending = !unended.is_empty(),
            "timing tool"
        );
    }
}
