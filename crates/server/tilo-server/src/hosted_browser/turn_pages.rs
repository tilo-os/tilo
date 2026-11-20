//! A page action of a workspace's Browser window, for a turn this server carries out itself.
//!
//! An agent a person keeps on this server works with every device of theirs off, so its turn
//! arrives on no connection, and [`HostedBrowsers::route`] serves callers on a connection. What
//! such a turn does at its workspace's window is handed over here instead
//! ([`HostedBrowsers::page_for_turn`]): the window is handed the Browser's own page action by
//! this server's own device, for the person the turn works for, and answers this server, the
//! device that asked it. That answer is taken here ([`HostedBrowsers::page_answered`]): nobody
//! on a connection waits for it.
//!
//! Which page actions a turn has carried, where it may send a window, and what its person is
//! asked first is decided by whoever carries out the turn, before anything reaches this.
//! Nothing here asks a person, and nothing here answers for one.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, Caller, InvokeRequest, ReceiptStatus, RemoteActionMessage, pending,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use super::{HostedBrowsers, page_action, update_of_error};
use crate::WorkspaceId;

/// How long a window has to say how a page action it was handed for a turn ended.
const PAGE_WAIT: Duration = Duration::from_secs(90);
/// How long a window is asked to hold an answer back for the action to end, in milliseconds.
const ANSWER_HOLD_MS: u32 = 20_000;
/// How long this server waits before it asks a window again how an action ended.
const ASK_AGAIN: Duration = Duration::from_millis(250);
/// The most page actions of turns that wait on windows at a time.
const MAX_WAITING: usize = 256;

/// What a turn is told where its workspace has no Browser window up.
const NO_WINDOW: &str = "No Browser window is open in this workspace. Open Browser first.";
/// What a turn is told while this server cannot hand a window anything yet.
const STARTING: &str = "Browser is still starting. Try again in a minute.";
/// What a turn is told when too many page actions wait on windows already.
const TOO_MANY: &str = "Too many page actions wait on Browser windows. Try again in a minute.";
/// What a turn is told when the window never said how the action ended.
const NOT_SAID: &str = "The Browser window did not say how that ended: take it as not done.";
/// What stands for a window's answer that it no longer knows the action.
const FORGOTTEN: &str = "the Browser window no longer knows the action";

/// A page action handed to a window for a turn, while the window has not said how it ended.
struct Waiting {
    workspace_id: WorkspaceId,
    /// The device the action was handed to: only its answer is taken.
    window: [u8; 32],
    /// What the window says of it, for whoever waits for its end.
    said: Arc<Said>,
}

/// What a window said of one page action, in the order it said it.
#[derive(Default)]
struct Said {
    statuses: Mutex<VecDeque<ReceiptStatus>>,
    told: tokio::sync::Notify,
}

impl Said {
    /// The window said `status`.
    fn say(&self, status: ReceiptStatus) {
        self.statuses.lock_recover().push_back(status);
        self.told.notify_one();
    }

    /// The next thing the window said, once it has said one.
    async fn next(&self) -> ReceiptStatus {
        loop {
            let next = self.statuses.lock_recover().pop_front();
            if let Some(status) = next {
                return status;
            }
            self.told.notified().await;
        }
    }
}

/// The page actions this server handed to windows for its turns, by the action's id.
#[derive(Default)]
pub(super) struct TurnPages(Mutex<HashMap<InvocationId, Waiting>>);

#[allow(
    clippy::significant_drop_tightening,
    reason = "each lock is held for one look at what waits"
)]
impl HostedBrowsers {
    /// Hand `request`, a page action of `workspace_id`'s Browser window, to that window for a
    /// turn this server carries out for `person`, and answer how it ended, as the window said
    /// it. The window hears this server's own device asking for that person. An action the
    /// window carried out is counted like any page action.
    ///
    /// Whoever calls this decided that the turn may have this action carried, and under what:
    /// nothing is asked here.
    ///
    /// Whatever the window itself says, a refusal of its own included, is the end this
    /// answers: other people's content, which whoever asked words for the turn.
    ///
    /// # Errors
    ///
    /// This server's own refusals, in its own words: no window is up in the workspace,
    /// `request` is no page action of that window, this server cannot hand it anything yet,
    /// the workspace's account may not use Browser, too many actions wait, or the window
    /// never said how it ended.
    pub async fn page_for_turn(
        &self,
        workspace_id: WorkspaceId,
        person: [u8; 32],
        request: InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let Some(window) = self.window(workspace_id) else {
            return Err(ActionCallError::Refused {
                reason: NO_WINDOW.to_owned(),
            });
        };
        // A turn reaches the window of the workspace it works in, and its page: anything else
        // names nothing here.
        if !page_action(&request.target, &window.provider) {
            return Err(ActionCallError::UnknownAction);
        }
        let (Some(hand), Some(own)) = (self.hand(), self.this_device()) else {
            return Err(ActionCallError::Unavailable {
                reason: STARTING.to_owned(),
            });
        };
        self.meter
            .admit(&self.account)
            .map_err(|refusal| ActionCallError::Refused {
                reason: refusal.to_reason(),
            })?;
        let invocation = request.invocation;
        let said = Arc::new(Said::default());
        {
            let mut waiting = self.turn_pages.0.lock_recover();
            if waiting.len() >= MAX_WAITING {
                return Err(ActionCallError::Unavailable {
                    reason: TOO_MANY.to_owned(),
                });
            }
            waiting.insert(
                invocation,
                Waiting {
                    workspace_id,
                    window: window.device,
                    said: Arc::clone(&said),
                },
            );
        }
        // This server's own device asks, for the person the turn works for.
        let from = (own, person);
        hand.hand(
            workspace_id,
            from,
            window.device,
            RemoteActionMessage::Invoke(request),
        );
        let ask_again = || {
            hand.hand(
                workspace_id,
                from,
                window.device,
                RemoteActionMessage::Observe {
                    correlation: 0,
                    invocation,
                    wait_ms: Some(ANSWER_HOLD_MS),
                    seen: None,
                },
            );
        };
        let ended = tokio::time::timeout(PAGE_WAIT, ended(&said, ask_again)).await;
        self.turn_pages.0.lock_recover().remove(&invocation);
        match ended {
            Ok(ReceiptStatus::Completed { output }) => {
                let workspace = self
                    .open
                    .lock_recover()
                    .get(&workspace_id)
                    .map(|up| up.workspace.clone());
                if let Some(workspace) = workspace {
                    self.counted(&workspace);
                }
                Ok(InvocationUpdate::Succeeded { invocation, output })
            }
            Ok(ReceiptStatus::Failed { error }) => {
                Ok(InvocationUpdate::Failed { invocation, error })
            }
            Ok(ReceiptStatus::Cancelled { message }) => Ok(InvocationUpdate::Cancelled {
                invocation,
                message,
            }),
            // What a window refuses with is the window's word, as everything it answers is:
            // an end it said, for whoever asked to word, and never this server's own refusal.
            Ok(ReceiptStatus::Rejected { error }) => Ok(update_of_error(invocation, &error)),
            Ok(ReceiptStatus::Accepted) | Err(_) => Err(ActionCallError::Unavailable {
                reason: NOT_SAID.to_owned(),
            }),
        }
    }

    /// This server's own device, as the transport names it: whom a window answers when this
    /// server asked it.
    fn this_device(&self) -> Option<[u8; 32]> {
        let authority = self.authority.lock_recover();
        Some(*authority.as_ref()?.server.id.as_bytes())
    }

    /// What `workspace_id`'s window says of a page action this server handed it for a turn:
    /// taken here, since nobody on a connection asked. `None` for any other message, which is
    /// served as it always was.
    pub(super) fn page_answered(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        let (invocation, status) = match message {
            RemoteActionMessage::Receipt(receipt) => (receipt.invocation, receipt.status.clone()),
            RemoteActionMessage::Update {
                invocation, state, ..
            } => (
                *invocation,
                state.as_ref().map_or_else(
                    || ReceiptStatus::Rejected {
                        error: ActionCallError::Unavailable {
                            reason: FORGOTTEN.to_owned(),
                        },
                    },
                    |state| state.receipt().status,
                ),
            ),
            _ => return None,
        };
        let device = *caller.device_public_key()?;
        {
            let waiting = self.turn_pages.0.lock_recover();
            let waits = waiting.get(&invocation)?;
            // Only the window the action was handed to says how it ended.
            if waits.workspace_id != workspace_id || waits.window != device {
                return None;
            }
            // Whoever waited for it may have stopped waiting: then nobody hears.
            waits.said.say(status);
        }
        // An answer needs no answer: one that is given up writes nothing to the window.
        let (unanswered, answer) = pending();
        drop(unanswered);
        Some(crate::app_serving::Routing::Answer(answer))
    }
}

/// How the page action ended whose window says what it says into `said`: the first thing it
/// says that is an end. While the window says the action goes on, it is asked again
/// (`ask_again`).
async fn ended(said: &Said, ask_again: impl Fn()) -> ReceiptStatus {
    loop {
        match said.next().await {
            ReceiptStatus::Accepted => {
                tokio::time::sleep(ASK_AGAIN).await;
                ask_again();
            }
            status => return status,
        }
    }
}
