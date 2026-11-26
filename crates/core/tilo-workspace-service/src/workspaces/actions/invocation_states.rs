//! The states an invocation passes through on its way to an answer: accepted
//! while its work goes on, asked while it waits for its person's yes, then
//! succeeded or failed.
use tilo_actions::remote::{ActionCallError, InvocationReceipt, InvocationState, asked};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

use tilo_lock::MutexRecover as _;

use super::ServiceWorkspaceActions as Actions;
use crate::workspaces::providers::{Answered, Live};

/// The state of an invocation that answered `value`.
pub(super) fn succeeded(invocation: InvocationId, value: serde_json::Value) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
    )
}

/// What `work` answered within `wait`, or `None` while it is still under way.
pub(super) async fn answered_within(
    work: &mut tokio::task::JoinHandle<Result<Answered, ActionCallError>>,
    wait: Option<std::time::Duration>,
) -> Option<Result<Result<Answered, ActionCallError>, tokio::task::JoinError>> {
    match wait {
        Some(wait) => tokio::time::timeout(wait, &mut *work).await.ok(),
        None => Some((&mut *work).await),
    }
}

/// Whether `state` is a call that waits in its asked record for its person's yes.
pub(super) fn waits_asked(state: &InvocationState) -> bool {
    matches!(
        &state.update,
        InvocationUpdate::Progress { phase, .. } if phase == asked::ASKED_PHASE
    )
}

/// Whether an answer finds `state` a call that waits: recorded as waiting, or still under way
/// because its asked record was written before the call was recorded as waiting ([`ended`]).
/// The call's entry among the host's asked calls is what says it waits; this is what the log
/// says while that entry is being taken.
pub(super) fn waits_an_answer(state: &InvocationState) -> bool {
    waits_asked(state) || !state.is_terminal()
}

/// The call whose work ended, as [`ended`] keeps it.
pub(super) struct Call {
    pub(super) workspaces: crate::workspaces::Workspaces,
    pub(super) workspace_id: Option<collab_types::WorkspaceId>,
    /// The caller's device certificate, as the transport verified it.
    pub(super) certificate: Option<tilo_user_identity::DeviceCert>,
    pub(super) caller: tilo_actions::remote::Caller,
    pub(super) request: tilo_actions::remote::InvokeRequest,
}

/// What `call` answers once its work `ran`, recorded as it ended. A success is kept for whoever
/// follows it. One that waits for its person's yes is kept as its asked record, and waits there
/// under way until they answer; one their approve admitted again is kept however it ends, for
/// the record to show. Anything else refused before anything changed: nothing to remember.
pub(super) async fn ended(
    actions: &Actions,
    call: &Call,
    ran: Result<Result<Answered, ActionCallError>, tokio::task::JoinError>,
) -> Result<InvocationReceipt, ActionCallError> {
    let invocation = call.request.invocation;
    match ran {
        Ok(Ok(Answered::Live(moves))) => Ok(follow(actions, call, moves)),
        Ok(Ok(Answered::Value(value))) => {
            let state = succeeded(invocation, value);
            let receipt = state.receipt();
            actions.log().complete(&invocation, state);
            Ok(receipt)
        }
        Ok(Err(ActionCallError::ConfirmationRequired(facts)))
            if call.request.approval().is_none() =>
        {
            // What a person's approve admits it again with, there before its record is
            // written, which keeps it too.
            actions
                .shared
                .asked_calls
                .lock_recover()
                .insert(invocation, (call.workspace_id, call.certificate.clone()));
            let kept = call
                .workspaces
                .keep_asked(call.workspace_id, &call.caller, &call.request, &facts)
                .await;
            if !matches!(kept, Some(Ok(_))) {
                actions
                    .shared
                    .asked_calls
                    .lock_recover()
                    .remove(&invocation);
            }
            match kept {
                Some(Ok(record)) => {
                    // The record is readable by whoever answers it before the call is recorded
                    // as waiting, so an answer can come first: its entry among the asked calls
                    // says the call waits, and one that took it keeps the ending it wrote.
                    let asking = actions.shared.asked_calls.lock_recover();
                    if asking.contains_key(&invocation) {
                        let state = InvocationState::new(invocation, record.waiting(invocation));
                        let receipt = state.receipt();
                        actions.log().complete(&invocation, state);
                        Ok(receipt)
                    } else {
                        let receipt = actions
                            .log()
                            .get(&call.caller, &invocation)
                            .map(InvocationState::receipt);
                        // Answered already: what it was admitted again as, until it says more.
                        Ok(receipt.unwrap_or(InvocationReceipt {
                            invocation,
                            status: tilo_actions::remote::ReceiptStatus::Accepted,
                        }))
                    }
                }
                Some(Err(why)) => {
                    actions.log().abandon(&invocation);
                    Err(ActionCallError::Refused {
                        reason: format!(
                            "it waits for its person's yes, and could not be kept for them to answer: {why}"
                        ),
                    })
                }
                None => {
                    actions.log().abandon(&invocation);
                    Err(ActionCallError::ConfirmationRequired(facts))
                }
            }
        }
        Ok(Err(error)) if call.request.approval().is_some() => {
            actions
                .log()
                .complete(&invocation, failed(invocation, &error.to_string()));
            Err(error)
        }
        Ok(Err(error)) => {
            actions.log().abandon(&invocation);
            Err(error)
        }
        Err(stopped) => {
            actions.log().abandon(&invocation);
            Err(ActionCallError::Unavailable {
                reason: format!("the host's operation stopped: {stopped}"),
            })
        }
    }
}

/// Answer that `call` runs, record it so, and record how it ends once `work` does: the caller
/// follows it there.
pub(super) fn still_running(
    actions: &Actions,
    call: Call,
    work: tokio::task::JoinHandle<Result<Answered, ActionCallError>>,
) -> InvocationReceipt {
    let invocation = call.request.invocation;
    let accepted = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
    let receipt = accepted.receipt();
    actions.log().complete(&invocation, accepted);
    let actions = actions.clone();
    actions.shared.runtime.clone().spawn(async move {
        if let Err(error) = ended(&actions, &call, work.await).await {
            actions
                .log()
                .complete(&invocation, failed(invocation, &error.to_string()));
        }
    });
    receipt
}

/// The state of an invocation that ended in `message`.
pub(super) fn failed(invocation: InvocationId, message: &str) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "refused".into(),
                message: message.to_owned(),
                details: None,
                retryable: false,
            },
        },
    )
}

/// Keep `moves`, a result that moves, as the items of `call`'s invocation: its newest item now,
/// each newer one as it comes, and its output once it ends. Answers the receipt of its first
/// item; a cancel of the invocation lets go of it ([`Actions::cancel`]).
pub(super) fn follow(
    actions: &Actions,
    call: &Call,
    mut moves: tokio::sync::watch::Receiver<Live>,
) -> InvocationReceipt {
    let invocation = call.request.invocation;
    let first = moves.borrow_and_update().clone();
    let state = live_state(invocation, 1, first);
    let receipt = state.receipt();
    actions.log().complete(&invocation, state);
    let following = moves.clone();
    let kept = actions.clone();
    let task = actions.shared.runtime.spawn(async move {
        let mut sequence = 1_u64;
        while moves.changed().await.is_ok() {
            sequence = sequence.saturating_add(1);
            let now = moves.borrow_and_update().clone();
            let ended = matches!(now, Live::Ended(_));
            kept.log()
                .complete(&invocation, live_state(invocation, sequence, now));
            if ended {
                break;
            }
        }
        kept.shared.live.lock_recover().remove(&invocation);
    });
    actions
        .shared
        .live
        .lock_recover()
        .insert(invocation, (following, task.abort_handle()));
    receipt
}

/// The state of invocation `invocation` whose result stands at `live`, as its `sequence`th item.
fn live_state(invocation: InvocationId, sequence: u64, live: Live) -> InvocationState {
    match live {
        Live::Item(value) => InvocationState::new(
            invocation,
            InvocationUpdate::Item {
                invocation,
                sequence,
                value,
            },
        ),
        Live::Ended(Ok(value)) => succeeded(invocation, value),
        Live::Ended(Err(why)) => failed(invocation, &why),
    }
}
