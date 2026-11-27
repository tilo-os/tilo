//! A field of a page filled from a vault item, for a turn this server carries out itself.
//!
//! An agent a person keeps on this server works with every device of theirs off, so its turn
//! arrives on no connection, and nothing hands its page actions to the workspace's Browser
//! window ([`HostedBrowsers::route`] serves callers on a connection). This is the one page
//! action such a turn has carried to the window: the fill of a field from an item of its
//! person's vault, by reference.
//!
//! - **Hand** ([`HostedBrowsers::fill_for_turn`]): the window is handed the Browser's own
//!   [`FILL_PAGE_ITEM`], as this server's own device, and this server keeps what decides the
//!   fill under the page action's id. The window answers this server, the device that asked it,
//!   and that answer is taken here ([`HostedBrowsers::fill_answered`]): nobody on a connection
//!   waits for it.
//! - **Release** ([`HostedBrowsers::item_asked`]): the window names the site its engine says
//!   the page is at and asks for the item's text
//!   ([`tilo_action_services::environment_device::USE_ITEM`]). Only the device this server
//!   placed for that workspace's Browser is answered, only for a fill this server handed it,
//!   naming that same item, and once. What decides is whoever carries out the turn
//!   ([`Releasing`]): the vault's own rule and the person's leave. A released text is sealed to
//!   the window's device key for that page action and that site
//!   ([`key_group::seal_released_text`]), so nothing between here and the window reads it, and
//!   an answer sent again opens for no other fill.
//!
//! Nothing here keeps a text: it is held for the one sealing and wiped.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, json};
use tilo_action_services::environment_device::{ItemUse, ItemUsed};
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, pending,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_app_host::agents::windows::{PageWindow, Release, Releasing, TurnFill};
use tilo_lock::MutexRecover as _;

use super::HostedBrowsers;
use tilo_server_protocol::WorkspaceId;

/// The Browser's page action that fills a field from a vault item by reference.
pub const FILL_PAGE_ITEM: &str = "fill-page-item";
/// How long a window has to say how a fill it was handed ended.
const FILL_WAIT: Duration = Duration::from_secs(60);
/// How long a window is asked to hold an answer back for the fill to end, in milliseconds.
const ANSWER_HOLD_MS: u32 = 20_000;
/// How long this server waits before it asks a window again how a fill ended.
const ASK_AGAIN: Duration = Duration::from_millis(250);
/// The most fills that wait on windows at a time.
const MAX_FILLS: usize = 256;

/// A fill handed to a window that has not said how it ended.
struct Waiting {
    workspace_id: WorkspaceId,
    /// The device the fill was handed to: only it is answered, and only its answer is taken.
    window: [u8; 32],
    /// The item the fill names, by its reference.
    item: String,
    /// What decides it, until the window asked: a fill is decided once.
    releasing: Option<Releasing>,
    /// What the window says of it, for whoever waits for its end.
    said: Arc<Said>,
}

/// What a window said of one fill, in the order it said it, for whoever waits for its end.
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

/// The fills this server handed to windows for its turns, by the page action's id.
#[derive(Default)]
pub(super) struct TurnFills(Mutex<HashMap<InvocationId, Waiting>>);

#[allow(
    clippy::significant_drop_tightening,
    reason = "each lock is held for one look at what waits"
)]
impl HostedBrowsers {
    /// Have `workspace_id`'s Browser window fill what `fill` names, for a turn this server
    /// carries out: the window is handed [`FILL_PAGE_ITEM`], asks for the item's text for the
    /// site its page is at, and `releasing` decides that once. Answers how the fill ended, as
    /// the window said it. A fill the window carried out is counted like any page action.
    ///
    /// # Errors
    ///
    /// No window is up in the workspace, this server cannot hand it anything yet, the
    /// workspace's account may not use Browser, too many fills wait, the window refused the
    /// action, or it never said how the fill ended.
    pub async fn fill_for_turn(
        &self,
        workspace_id: WorkspaceId,
        fill: TurnFill,
        releasing: Releasing,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let Some((window, page)) = self.window_and_page(workspace_id) else {
            return Err(ActionCallError::Refused {
                reason: "No Browser window is open in this workspace, so there is no page to fill"
                    .to_owned(),
            });
        };
        let (Some(hand), Some(own)) = (self.hand(), self.own_device()) else {
            return Err(ActionCallError::Unavailable {
                reason: "Browser is still starting. Try again in a minute.".to_owned(),
            });
        };
        self.meter
            .admit(&self.account)
            .map_err(|refusal| ActionCallError::Refused {
                reason: refusal.to_reason(),
            })?;
        let invocation = fill.invocation;
        let said = Arc::new(Said::default());
        {
            let mut fills = self.turn_fills.0.lock_recover();
            if fills.len() >= MAX_FILLS {
                return Err(ActionCallError::Unavailable {
                    reason: "too many page fills are waiting for their window; ask again shortly"
                        .into(),
                });
            }
            fills.insert(
                invocation,
                Waiting {
                    workspace_id,
                    window: window.device,
                    item: fill.item.clone(),
                    releasing: Some(releasing),
                    said: Arc::clone(&said),
                },
            );
        }
        hand.hand(
            workspace_id,
            own,
            window.device,
            RemoteActionMessage::Invoke(fill_request(&page, &fill)),
        );
        let ask_again = || {
            hand.hand(
                workspace_id,
                own,
                window.device,
                RemoteActionMessage::Observe {
                    correlation: 0,
                    invocation,
                    wait_ms: Some(ANSWER_HOLD_MS),
                    seen: None,
                },
            );
        };
        let ended = tokio::time::timeout(FILL_WAIT, ended(&said, ask_again)).await;
        self.turn_fills.0.lock_recover().remove(&invocation);
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
            Ok(ReceiptStatus::Rejected { error }) => Err(error),
            Ok(ReceiptStatus::Accepted) | Err(_) => Err(ActionCallError::Unavailable {
                reason: "The Browser window did not say how the fill ended: take the field \
                             as not filled."
                    .to_owned(),
            }),
        }
    }

    /// This server's own device and its person, as the transport names them: whom a window
    /// answers when this server asked it.
    fn own_device(&self) -> Option<([u8; 32], [u8; 32])> {
        let authority = self.authority.lock_recover();
        let authority = authority.as_ref()?;
        Some((*authority.server.id.as_bytes(), authority.user_public_key))
    }

    /// What `workspace_id`'s window says of a fill this server handed it for a turn: taken
    /// here, since nobody on a connection asked. `None` for any other message, which is served
    /// as it always was.
    pub(super) fn fill_answered(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<tilo_workspace_service::app_serving::Routing> {
        let (invocation, status) = match message {
            RemoteActionMessage::Receipt(receipt) => (receipt.invocation, receipt.status.clone()),
            RemoteActionMessage::Update {
                invocation, state, ..
            } => (
                *invocation,
                state.as_ref().map_or_else(
                    || ReceiptStatus::Rejected {
                        error: ActionCallError::Unavailable {
                            reason: "the Browser window no longer knows the fill".to_owned(),
                        },
                    },
                    |state| state.receipt().status,
                ),
            ),
            _ => return None,
        };
        let device = *caller.device_public_key()?;
        {
            let fills = self.turn_fills.0.lock_recover();
            let waiting = fills.get(&invocation)?;
            if waiting.workspace_id != workspace_id || waiting.window != device {
                return None;
            }
            // Whoever waited for it may have stopped waiting: then nobody hears.
            waiting.said.say(status);
        }
        // An answer needs no answer: one that is given up writes nothing to the window.
        let (unanswered, answer) = pending();
        drop(unanswered);
        Some(tilo_workspace_service::app_serving::Routing::Answer(answer))
    }

    /// `caller` asks for the text of the item a fill names
    /// ([`tilo_action_services::environment_device::USE_ITEM`]): answered for the device this
    /// server placed for `workspace_id`'s Browser and nobody else, for a fill this server
    /// handed that device, naming that item, and once.
    pub(super) fn item_asked(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<tilo_workspace_service::app_serving::Routing> {
        let message = RemoteActionMessage::Invoke(request.clone());
        let refused = |error: ActionCallError| super::answered_with(&message, error);
        let Some(device) = caller.device_public_key().copied() else {
            return refused(ActionCallError::NotPermitted);
        };
        let placed = self
            .open
            .lock_recover()
            .get(&workspace_id)
            .filter(|up| up.window.device == device && up.device.device_public_key == device)
            .map(|up| up.workspace.clone());
        let Some(workspace) = placed else {
            return refused(ActionCallError::NotPermitted);
        };
        let asked = match &request.argument {
            InvokeArgument::Json(arguments) => {
                serde_json::from_value::<ItemUse>(serde_json::Value::Object(arguments.clone())).ok()
            }
            _ => None,
        };
        let Some(asked) = asked else {
            return refused(ActionCallError::InvalidRequest {
                reason: "an item to use names the page action that fills, the item and the \
                         site the page is at"
                    .to_owned(),
            });
        };
        let releasing = self
            .turn_fills
            .0
            .lock_recover()
            .get_mut(&asked.invocation)
            .filter(|waiting| {
                waiting.workspace_id == workspace_id
                    && waiting.window == device
                    && waiting.item == asked.item
            })
            .and_then(|waiting| waiting.releasing.take());
        let Some(releasing) = releasing else {
            return refused(ActionCallError::Refused {
                reason: "No fill this server handed this window waits for that item".to_owned(),
            });
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return refused(ActionCallError::Unavailable {
                reason: "this server answers no window yet".to_owned(),
            });
        };
        let invocation = request.invocation;
        let (answer, answered) = pending();
        runtime.spawn(async move {
            let status = match releasing(asked.origin.clone()).await {
                Release::Text { text, into } => sealed(&workspace, device, &asked, &text, into),
                Release::Asked(words) => completed(&ItemUsed::Asked { words }),
                Release::Refused(reason) => ReceiptStatus::Rejected {
                    error: ActionCallError::Refused { reason },
                },
            };
            answer.complete(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status,
            }));
        });
        Some(tilo_workspace_service::app_serving::Routing::Answer(
            answered,
        ))
    }
}

/// The page action `page`'s window is handed for `fill`, answered at once where the fill ends
/// quickly.
fn fill_request(page: &PageWindow, fill: &TurnFill) -> InvokeRequest {
    let mut arguments = Map::new();
    arguments.insert("element".into(), json!(fill.element));
    arguments.insert("item".into(), json!(fill.item));
    if !fill.reason.is_empty() {
        arguments.insert("reason".into(), json!(fill.reason));
    }
    InvokeRequest {
        invocation: fill.invocation,
        target: page.target(FILL_PAGE_ITEM),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: Some(ANSWER_HOLD_MS),
        appearance: None,
        signed: None,
    }
}

/// How the fill ended whose window says what it says into `said`: the first thing it says
/// that is an end. While the window says the fill goes on, it is asked again (`ask_again`).
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

/// `text` sealed to `device` for the fill `asked` names in `workspace`, as the ask answers it.
fn sealed(
    workspace: &str,
    device: [u8; 32],
    asked: &ItemUse,
    text: &str,
    into: &str,
) -> ReceiptStatus {
    let bound = key_group::ReleasedFor {
        workspace,
        invocation: *asked.invocation.0.as_bytes(),
        origin: &asked.origin,
    };
    match key_group::seal_released_text(device, &bound, text) {
        Ok(sealed) => completed(&ItemUsed::Released {
            sealed: hex::encode(sealed),
            into: into.to_owned(),
        }),
        Err(error) => ReceiptStatus::Rejected {
            error: ActionCallError::Unavailable {
                reason: format!("the text could not be sealed to the window: {error}"),
            },
        },
    }
}

/// `answer` as what the ask completed with.
fn completed(answer: &ItemUsed) -> ReceiptStatus {
    match serde_json::to_value(answer) {
        Ok(value) => ReceiptStatus::Completed {
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
        Err(error) => ReceiptStatus::Rejected {
            error: ActionCallError::Unavailable {
                reason: error.to_string(),
            },
        },
    }
}
