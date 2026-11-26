//! Answering one request against a provider: what each message a peer sends asks of an
//! [`ActionProvider`], and the refusal for what a provider keeps none of.

use tilo_actions::remote::{
    ActionCallError, ActionProvider, CHANGE_FEED_UNAVAILABLE, CHANGED_UNAVAILABLE,
    CONTENT_GETS_UNAVAILABLE, CONTENT_LINKS_UNAVAILABLE, CONTENT_PUTS_UNAVAILABLE, Caller,
    InvocationReceipt, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage,
    STORE_COMMANDS_UNAVAILABLE, WatchScope, confirmation_unanswered, pending, read_answered,
    refuse, updated, wait_deadline,
};

use crate::wait;
use crate::wait::settle_with;

/// The refusal of a request [`serve`] keeps nothing for: a retired feedback
/// ask; uploads over a session, kept by a
/// provider that keeps them open between steps ([`HostedContentPuts`](tilo_actions::remote::HostedContentPuts)); and
/// content, read from a store a device keeps. `None` for any other message.
fn not_kept(message: &RemoteActionMessage) -> Option<RemoteActionMessage> {
    let reason = match message {
        RemoteActionMessage::RetiredFeedbackRequest { .. } => "feedback receipts are retired",
        RemoteActionMessage::ContentPutRequest { .. } => CONTENT_PUTS_UNAVAILABLE,
        RemoteActionMessage::ContentGetRequest { .. } => CONTENT_GETS_UNAVAILABLE,
        RemoteActionMessage::ContentLinkRequest { .. } => CONTENT_LINKS_UNAVAILABLE,
        _ => return None,
    };
    refuse(
        message,
        ActionCallError::Unavailable {
            reason: reason.into(),
        },
    )
}

/// `command` to the records of `caller`'s app, answered by `provider`'s store
/// ([`ActionProvider::store_commands`]) for `correlation`.
fn store_answered<H: ActionProvider>(
    provider: &H,
    caller: &Caller,
    correlation: u64,
    (command, kind): (serde_json::Value, Option<String>),
) -> Pending<RemoteActionMessage> {
    let result = match provider.store_commands() {
        Some(store) => store.command(caller, command, kind),
        None => Pending::ready(Err(ActionCallError::Unavailable {
            reason: STORE_COMMANDS_UNAVAILABLE.into(),
        })),
    };
    result.map(move |result| RemoteActionMessage::StoreResponse {
        correlation,
        result,
    })
}

/// The changes to `caller`'s workspace's records after `after`, from
/// `provider`'s feed, answered for `correlation` ([`ActionProvider::change_feed`]).
fn changed<H: ActionProvider>(
    provider: &H,
    caller: &Caller,
    correlation: u64,
    after: Option<&str>,
) -> Pending<RemoteActionMessage> {
    let result = match provider.change_feed() {
        Some(feed) => feed.since(caller, after),
        None => Pending::ready(Err(ActionCallError::Unavailable {
            reason: CHANGE_FEED_UNAVAILABLE.into(),
        })),
    };
    result.map(move |result| RemoteActionMessage::ChangesResponse {
        correlation,
        result,
    })
}

/// A call media lane is its own host's, never a provider's: one asked here says it carries none.
fn no_call_media_lane() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this connection carries no call media lane".into(),
    }
}

/// The answer to a shell asked of a provider's connection, which carries none: a shell is the
/// device's own, on an app's connection.
fn no_shell(correlation: u64) -> RemoteActionMessage {
    RemoteActionMessage::ShellOpenResponse {
        correlation,
        result: Err(ActionCallError::Unavailable {
            reason: "this connection carries no shell".into(),
        }),
    }
}

/// A presence lane is its own host's, never a provider's: one asked here says it carries none.
fn no_presence_lane() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this connection carries no presence lane".into(),
    }
}

/// A sound output lane is its own host's, never a provider's: one asked here says it carries none.
fn no_audio_output_lane() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this connection carries no sound output lane".into(),
    }
}

/// A call lane is its own host's, never a provider's: one asked here says it carries none.
fn no_call_lane() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this connection carries no call lane".into(),
    }
}

/// Answer one request from `caller` against `provider`.
///
/// Returns `None` for a message that is not a request (a response arriving
/// at a provider is dropped, never answered). A refused invocation becomes a
/// [`ReceiptStatus::Rejected`] receipt so the caller always learns the fate
/// of the id it chose; an answer the provider abandons becomes a refusal too.
///
/// A request that asks to wait (`wait_ms` on [`RemoteActionMessage::Observe`]
/// or [`InvokeRequest`]) is answered once the invocation moves, through
/// [`ActionProvider::next_change`] and without holding a thread; see
/// [`settle_with`]. A provider that cannot signal changes answers at once.
pub fn serve<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    message: RemoteActionMessage,
) -> Option<Pending<RemoteActionMessage>> {
    let abandoned = refuse(
        &message,
        ActionCallError::Unavailable {
            reason: "the provider dropped the request".into(),
        },
    )?;
    if let Some(answer) = unprovided(&message) {
        return Some(Pending::ready(answer));
    }
    let answer = match message {
        RemoteActionMessage::CatalogueRequest { correlation, query } => provider
            .query(caller, query)
            .map(move |result| RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            }),
        RemoteActionMessage::Invoke(request) => {
            invoked(provider, caller, request).map(RemoteActionMessage::Receipt)
        }
        RemoteActionMessage::Observe {
            correlation,
            invocation,
            wait_ms: Some(wait_ms),
            seen,
        } => settle_with(provider, caller, invocation, seen, wait_deadline(wait_ms))
            .map(move |state| updated(correlation, invocation, state)),
        RemoteActionMessage::Observe {
            correlation,
            invocation,
            ..
        } => Pending::ready(updated(
            correlation,
            invocation,
            provider.observe(caller, &invocation),
        )),
        RemoteActionMessage::Cancel { invocation } => provider
            .cancel(caller, &invocation)
            .map(move |result| RemoteActionMessage::CancelResponse { invocation, result }),
        // Read by a provider's own reader, which answers through its trait object: what a reading is
        // made of loads with the code that reads records, not with every provider's API.
        RemoteActionMessage::ResourceRead {
            correlation, read, ..
        } => read_answered(provider, caller, correlation, read),
        RemoteActionMessage::ChangesRequest { correlation, after } => {
            changed(provider, caller, correlation, after.as_deref())
        }
        RemoteActionMessage::ChangedRequest { correlation, scope } => {
            noticed(provider, caller, correlation, scope)
        }
        RemoteActionMessage::SurfaceRequest { correlation, app } => {
            surfaced(provider, correlation, &app)
        }
        RemoteActionMessage::StoreRequest {
            correlation,
            command,
            kind,
        } => store_answered(provider, caller, correlation, (command, kind)),
        // An answer is never a request: `refuse` classified every message before this match and
        // answered `None` for an answer, and `unprovided` answered what no provider takes, so
        // neither reaches here.
        _ => return None,
    };
    // An abandoned answer still answers: the caller hears a refusal.
    let (completer, answered) = pending();
    answer.then(move |answer| completer.complete(answer.unwrap_or(abandoned)));
    Some(answered)
}

/// The answer to a request that no provider takes, or `None` for any other message. What a
/// stateless provider keeps none of, it refuses; a question is the person's to answer, on the
/// device they are at; and a lane (calls, sound output, call media, presence, shells) is its own
/// host's, so one asked here says it carries none.
fn unprovided(message: &RemoteActionMessage) -> Option<RemoteActionMessage> {
    Some(match *message {
        RemoteActionMessage::RetiredFeedbackRequest { .. }
        | RemoteActionMessage::ContentPutRequest { .. }
        | RemoteActionMessage::ContentGetRequest { .. }
        | RemoteActionMessage::ContentLinkRequest { .. } => not_kept(message)?,
        RemoteActionMessage::ConfirmationRequest { correlation, .. } => {
            confirmation_unanswered(correlation, ActionCallError::confirmation_required())
        }
        RemoteActionMessage::CallCarryRequest { correlation, .. } => {
            RemoteActionMessage::CallCarryResponse {
                correlation,
                result: Err(no_call_lane()),
            }
        }
        RemoteActionMessage::CallReport { correlation, .. } => {
            RemoteActionMessage::CallReportResponse {
                correlation,
                result: Err(no_call_lane()),
            }
        }
        RemoteActionMessage::AudioOutputRequest { correlation, .. } => {
            RemoteActionMessage::AudioOutputResponse {
                correlation,
                result: Err(no_audio_output_lane()),
            }
        }
        RemoteActionMessage::CallMediaRequest { correlation, .. } => {
            RemoteActionMessage::CallMediaResult {
                correlation,
                result: Err(no_call_media_lane()),
            }
        }
        RemoteActionMessage::PresenceSaid { correlation, .. } => {
            RemoteActionMessage::PresenceSaidResult {
                correlation,
                result: Err(no_presence_lane()),
            }
        }
        RemoteActionMessage::ShellOpenRequest { correlation, .. } => no_shell(correlation),
        _ => return None,
    })
}

/// Run `request` at `provider`, answering its receipt once it is ready, or once it moves when it
/// asks to wait. How the answer travels is not part of what runs.
fn invoked<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    mut request: InvokeRequest,
) -> Pending<InvocationReceipt> {
    let until = request.wait_ms.take().map(wait_deadline);
    let invocation = request.invocation;
    let receipt = provider.invoke(caller, request).map(move |result| {
        result.unwrap_or_else(|error| InvocationReceipt {
            invocation,
            status: ReceiptStatus::Rejected { error },
        })
    });
    match until {
        Some(until) => wait::settle_receipt(provider, caller, receipt, until),
        None => receipt,
    }
}

/// The next time what `scope` follows may have changed, as `provider` tells it
/// ([`ActionProvider::changed`]), answered for `correlation`: a provider that cannot tell says so,
/// and the device that asked tells its waiter why rather than leaving it to a signal that never
/// comes.
fn noticed<H: ActionProvider>(
    provider: &H,
    caller: &Caller,
    correlation: u64,
    scope: WatchScope,
) -> Pending<RemoteActionMessage> {
    let respond = move |result| RemoteActionMessage::ChangedResponse {
        correlation,
        result,
    };
    match provider.changed(caller, scope) {
        Some(changed) => changed.map(respond),
        None => Pending::ready(respond(Err(ActionCallError::Unavailable {
            reason: CHANGED_UNAVAILABLE.into(),
        }))),
    }
}

/// The surface the workspace's package `app` publishes, from `provider`,
/// answered for `correlation` ([`ActionProvider::installed_surface`]).
fn surfaced<H: ActionProvider>(
    provider: &H,
    correlation: u64,
    app: &str,
) -> Pending<RemoteActionMessage> {
    provider
        .installed_surface(app)
        .map(move |surface| RemoteActionMessage::SurfaceResponse {
            correlation,
            result: Ok(surface.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())),
        })
}
