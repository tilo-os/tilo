//! The states an invocation passes through on its way to an answer: accepted
//! while its work goes on, then succeeded or failed.
use tilo_actions::remote::{ActionCallError, InvocationReceipt, InvocationState};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

use super::ServiceWorkspaceActions as Actions;

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

/// How long an invocation that asked its person waits for an answer at hand before its caller
/// hears that it waits for them.
const APPROVING: std::time::Duration = std::time::Duration::from_secs(15);

/// What `work` answered within `wait` (until it ends without one) and whether its person was
/// asked on the way, or `Err(asked)` while it is still under way, waiting for its person when
/// `asked`. One its person was asked about (`asking`) is given a while for an answer at hand (a
/// computer holding their root answers at once), and then its caller hears it waits for them; it
/// goes on, and is done once they allow it.
pub(super) async fn answered_within(
    work: &mut tokio::task::JoinHandle<Result<serde_json::Value, ActionCallError>>,
    wait: Option<std::time::Duration>,
    asking: &tokio::sync::Notify,
) -> Result<
    (
        Result<Result<serde_json::Value, ActionCallError>, tokio::task::JoinError>,
        bool,
    ),
    bool,
> {
    let asked = std::sync::atomic::AtomicBool::new(false);
    // The question is looked at first, so work that ends as its person answers is known to
    // have asked them.
    tokio::select! {
        biased;
        () = async {
            asking.notified().await;
            asked.store(true, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(APPROVING).await;
        } => Err(true),
        ran = async {
            match wait {
                Some(wait) => tokio::time::timeout(wait, &mut *work).await.map_err(|_| false),
                None => Ok((&mut *work).await),
            }
        } => ran.map(|ran| (ran, asked.load(std::sync::atomic::Ordering::Relaxed))),
    }
}

/// What `invocation` answers once its work `ran`, recorded as it ended: a success is kept for
/// whoever follows it, and so is a no from its person, once `asked`, so the same invocation
/// asked again answers their no rather than putting the question to them a second time.
pub(super) fn ended(
    actions: &Actions,
    invocation: InvocationId,
    ran: Result<Result<serde_json::Value, ActionCallError>, tokio::task::JoinError>,
    asked: bool,
) -> Result<InvocationReceipt, ActionCallError> {
    match ran {
        Ok(Ok(value)) => {
            let state = succeeded(invocation, value);
            let receipt = state.receipt();
            actions.log().complete(&invocation, state);
            Ok(receipt)
        }
        Ok(Err(error)) if asked => {
            actions
                .log()
                .complete(&invocation, failed(invocation, &error.to_string()));
            Err(error)
        }
        // Refused before anything changed, or answered from the workspace's request records
        // on a retry: nothing to remember.
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

/// Answer that `invocation` runs, record it so, and record how it ends once
/// `work` does: the caller follows it there.
pub(super) fn still_running(
    actions: &Actions,
    invocation: InvocationId,
    work: tokio::task::JoinHandle<Result<serde_json::Value, ActionCallError>>,
    asking: bool,
) -> InvocationReceipt {
    let update = if asking {
        InvocationUpdate::Progress {
            invocation,
            sequence: 1,
            phase: tilo_actions::remote::AWAITING_APPROVAL.to_owned(),
            completed: None,
            total: None,
            message: Some(
                "it waits for its person to approve it on a device that holds their identity"
                    .to_owned(),
            ),
        }
    } else {
        InvocationUpdate::Accepted { invocation }
    };
    let accepted = InvocationState::new(invocation, update);
    let receipt = accepted.receipt();
    actions.log().complete(&invocation, accepted);
    let actions = actions.clone();
    actions.shared.runtime.clone().spawn(async move {
        let state = match work.await {
            Ok(Ok(value)) => succeeded(invocation, value),
            Ok(Err(error)) => failed(invocation, &error.to_string()),
            Err(stopped) => failed(
                invocation,
                &format!("the server's operation stopped: {stopped}"),
            ),
        };
        actions.log().complete(&invocation, state);
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
