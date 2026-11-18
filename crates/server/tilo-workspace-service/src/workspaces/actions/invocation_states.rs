//! The states an invocation passes through on its way to an answer: accepted
//! while its work goes on, then succeeded or failed.
use tilo_actions::remote::{ActionCallError, InvocationReceipt, InvocationState};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

use super::HostWorkspaceActions as Actions;

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

/// What `work` answered within `wait` (until it ends without one), or `Err(asked)` while it is
/// still under way, waiting for its person when `asked`. One its person was asked about
/// (`asking`) is given a while for an answer at hand (a computer holding their root answers at
/// once), and then its caller hears it waits for them; it goes on, and is done once they allow it.
pub(super) async fn answered_within(
    work: &mut tokio::task::JoinHandle<Result<serde_json::Value, ActionCallError>>,
    wait: Option<std::time::Duration>,
    asking: &tokio::sync::Notify,
) -> Result<Result<Result<serde_json::Value, ActionCallError>, tokio::task::JoinError>, bool> {
    tokio::select! {
        ran = async {
            match wait {
                Some(wait) => tokio::time::timeout(wait, &mut *work).await.map_err(|_| false),
                None => Ok((&mut *work).await),
            }
        } => ran,
        () = async {
            asking.notified().await;
            tokio::time::sleep(APPROVING).await;
        } => Err(true),
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
