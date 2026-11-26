//! A call that waits for its person's yes, as its caller and its person meet it on this host:
//! its receipt says it is under way, its asked record shows the person what it would do, and
//! the person's answer to that record through `tilo.invocations#resolve` ends it once.

use serde_json::Value;
use tilo_actions::remote::asked::{self, Offer, Resolve};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionTarget, workspace_invocations,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use tilo_workspace_service::workspaces::Workspaces;

use crate::fixtures::settled;

/// What `receipt`, the answer to `request`, says at once: what the call produced where it
/// completed, and where it waits in its asked record for its person's yes, what they are shown
/// there ([`ActionCallError::ConfirmationRequired`]).
pub(crate) fn at_once(
    workspaces: &Workspaces,
    request: &InvokeRequest,
    receipt: InvocationReceipt,
) -> Result<Value, ActionCallError> {
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        status => Err(kept_asking(workspaces, request, &status)),
    }
}

/// What the host kept of `request` for its person, as the answer that awaits them: a receipt
/// that neither completed nor waits is a host breaking its one rule.
fn kept_asking(
    workspaces: &Workspaces,
    request: &InvokeRequest,
    status: &ReceiptStatus,
) -> ActionCallError {
    let store = workspaces
        .asked_store()
        .expect("the host keeps one store of the calls that wait");
    let kept = store
        .record(&request.invocation.0.to_string())
        .and_then(|at| store.kept_ask(&at))
        .unwrap_or_else(|| {
            panic!("the host answers at once or keeps the call for its person, not {status:?}")
        });
    ActionCallError::ConfirmationRequired(Box::new(kept.facts))
}

/// `request`, which `caller` made over `page` and which waits in its asked record, answered
/// `resolution` by its person `person` over the same page: what the call answers once it ends.
/// Declined, it ends with nothing done and its caller hears why.
pub(crate) async fn answered_by(
    page: &(impl ActionProvider + Sync),
    person: &Caller,
    (caller, request): (&Caller, &InvokeRequest),
    resolution: Offer,
) -> Result<Value, ActionCallError> {
    let resolving = Resolve {
        resolution,
        ..Resolve::default()
    };
    let Value::Object(arguments) = serde_json::to_value(&resolving).expect("an answer encodes")
    else {
        panic!("an answer is an object");
    };
    let answer = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(request.invocation.0.to_string(), asked::RESOLVE),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    settled(page.invoke(person, answer)).await?;
    let ended = tilo_test_env::within("the answered call's end", async {
        loop {
            match page
                .observe(caller, &request.invocation)
                .map(|state| state.update)
            {
                Some(
                    update @ (InvocationUpdate::Succeeded { .. }
                    | InvocationUpdate::Failed { .. }
                    | InvocationUpdate::Cancelled { .. }),
                ) => return update,
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await;
    match ended {
        InvocationUpdate::Succeeded { output, .. } => Ok(output.value.unwrap_or(Value::Null)),
        InvocationUpdate::Failed { error, .. } => Err(ActionCallError::Refused {
            reason: error
                .message
                .strip_prefix("refused: ")
                .unwrap_or(&error.message)
                .to_owned(),
        }),
        InvocationUpdate::Cancelled { message, .. } => Err(ActionCallError::Refused {
            reason: message.unwrap_or_default(),
        }),
        other => unreachable!("the loop returns only an end: {other:?}"),
    }
}
