//! A call that waits for its person's yes, through this host's own dispatch: it is kept as its
//! asked record, listed to the person it asks and to nobody else, and their approve of the
//! record carries it out once; their decline ends it with nothing done.

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::remote::asked::{self, AskedList, AskedStore, Offer, Resolve};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvocationReceipt, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionTarget, person_vault,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, governed, settled};
use crate::workspaces::tests::{governor, test_host};

/// The agent that asks here, by its id in the workspace.
const AGENT: &str = "scout";

/// The call of `target` with `argument`, under a fresh invocation id.
fn calling(target: RemoteActionTarget, argument: Value) -> InvokeRequest {
    let Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A login for `site` its agent would keep in the workspace's vault.
fn keeping(site: &str) -> InvokeRequest {
    calling(
        person_vault::target(person_vault::ADD),
        json!({"kind": "login", "belongsTo": [site], "username": "ada", "secret": "s3cret"}),
    )
}

/// `request` by `caller`, over the connection `device` certifies to the workspace.
async fn invoked(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    request: InvokeRequest,
) -> Result<InvocationReceipt, ActionCallError> {
    settled(
        service
            .in_workspace_as(workspace_id, device)
            .invoke(caller, request),
    )
    .await
}

/// The questions that wait for `caller`, as their own list reads them.
async fn waiting(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> Result<AskedList, ActionCallError> {
    let listing = calling(asked::target(asked::WAITING, asked::LIST), json!({}));
    let receipt = invoked(service, workspace_id, who, listing).await?;
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the list answers at once: {receipt:?}");
    };
    Ok(serde_json::from_value(output.value.unwrap_or(Value::Null)).unwrap())
}

/// `answer` given to the asked record `record` by `who`.
async fn resolve(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    record: &asked::RecordRef,
    answer: Offer,
) -> Result<InvocationReceipt, ActionCallError> {
    let resolving = Resolve {
        resolution: answer,
        choice: None,
    };
    let request = calling(
        RemoteActionTarget {
            provider: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(&record.record, asked::RESOLVE),
        },
        serde_json::to_value(resolving).unwrap(),
    );
    invoked(service, workspace_id, who, request).await
}

/// How `invocation` of `caller` ended, once it did.
async fn ended(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    invocation: InvocationId,
) -> InvocationUpdate {
    let here = service.in_workspace_as(workspace_id, device);
    tilo_test_env::within("the approved call's end", async {
        loop {
            match here.observe(caller, &invocation).map(|state| state.update) {
                Some(
                    update @ (InvocationUpdate::Succeeded { .. }
                    | InvocationUpdate::Failed { .. }
                    | InvocationUpdate::Cancelled { .. }),
                ) => return update,
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .unwrap()
}

/// What `caller`'s vault lists.
async fn logins(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> usize {
    let listing = calling(person_vault::target(person_vault::LIST), json!({}));
    let receipt = invoked(service, workspace_id, who, listing).await.unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the vault lists at once: {receipt:?}");
    };
    output.value.unwrap()["items"]
        .as_array()
        .map_or(0, Vec::len)
}

/// An agent's change waits in its record for its person, who alone finds it among what waits
/// for them and answers it: their approve keeps the login once, a second approve finds nothing,
/// and their decline of another keeps nothing. Another member, the agent itself and the person's
/// key on another member's device read none of it and answer none of it.
#[tokio::test]
async fn a_call_that_waits_is_its_persons_alone_and_their_approve_carries_it_out_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.keep_asked_in(&Arc::new(AskedStore::default()));
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));
    let agent = as_ada.clone().delegated_to(AGENT);
    let before = logins(&service, workspace_id, (&ada, &as_ada)).await;

    let adding = keeping("https://example.test");
    let receipt = invoked(&service, workspace_id, (&ada, &agent), adding.clone())
        .await
        .unwrap();
    assert!(!receipt.status.is_terminal(), "it waits: {receipt:?}");
    let declined = keeping("https://elsewhere.test");
    invoked(&service, workspace_id, (&ada, &agent), declined.clone())
        .await
        .unwrap();

    // Hers alone, oldest first.
    let hers = waiting(&service, workspace_id, (&ada, &as_ada))
        .await
        .unwrap();
    assert_eq!(hers.questions.len(), 2, "{hers:?}");
    let (first, second) = (&hers.questions[0], &hers.questions[1]);
    assert_eq!(first.workspace, workspace_id.to_string());
    assert!(first.offers.contains(&Offer::Approve), "{first:?}");
    let his = waiting(&service, workspace_id, (&bo, &as_bo))
        .await
        .unwrap();
    assert!(his.questions.is_empty(), "{his:?}");
    for (who, device, asking) in [
        ("her own agent", &ada, agent.clone()),
        ("her key on another member's device", &bo, as_ada.clone()),
    ] {
        let refused = waiting(&service, workspace_id, (device, &asking)).await;
        assert!(refused.is_err(), "{who}: {refused:?}");
    }

    // Nobody but her answers it.
    for (who, device, asking) in [
        ("another member", &bo, as_bo.clone()),
        ("her own agent", &ada, agent.clone()),
    ] {
        let refused = resolve(
            &service,
            workspace_id,
            (device, &asking),
            &first.record,
            Offer::Approve,
        )
        .await;
        assert!(refused.is_err(), "{who}: {refused:?}");
    }
    assert_eq!(
        logins(&service, workspace_id, (&ada, &as_ada)).await,
        before
    );

    // Her approve carries it out, once.
    resolve(
        &service,
        workspace_id,
        (&ada, &as_ada),
        &first.record,
        Offer::Approve,
    )
    .await
    .unwrap();
    let kept = ended(&service, workspace_id, (&ada, &agent), adding.invocation).await;
    assert!(
        matches!(kept, InvocationUpdate::Succeeded { .. }),
        "{kept:?}"
    );
    let again = resolve(
        &service,
        workspace_id,
        (&ada, &as_ada),
        &first.record,
        Offer::Approve,
    )
    .await;
    assert!(again.is_err(), "approved once: {again:?}");

    // Her decline ends the other with nothing done.
    resolve(
        &service,
        workspace_id,
        (&ada, &as_ada),
        &second.record,
        Offer::Decline,
    )
    .await
    .unwrap();
    let ended_declined = ended(&service, workspace_id, (&ada, &agent), declined.invocation).await;
    assert!(
        matches!(ended_declined, InvocationUpdate::Cancelled { .. }),
        "{ended_declined:?}"
    );
    assert_eq!(
        logins(&service, workspace_id, (&ada, &as_ada)).await,
        before + 1
    );
    let left = waiting(&service, workspace_id, (&ada, &as_ada))
        .await
        .unwrap();
    assert!(left.questions.is_empty(), "{left:?}");
    server.shutdown().await;
}
