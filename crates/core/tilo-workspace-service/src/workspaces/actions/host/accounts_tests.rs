//! A workspace's mail accounts as a live read: a window that follows them is
//! sent the whole listing again when they change, woken by the change rather
//! than asking for it again.
//!
//! A page keeps no vault and no mail connection, so its window reads the
//! accounts the workspace keeps, and another member's add or removal must
//! reach it when it happens.

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::person_accounts::{self, MailAccounts};
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
};

use super::super::tests::{admit, caller, governed, settled};
use crate::workspaces::tests::{governor, test_host};

/// A request of one of the accounts actions, with `argument` when it has one.
pub(super) fn asking(action: &str, argument: Value) -> InvokeRequest {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: person_accounts::target(action),
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `address` kept in the workspace's accounts, as a window's add keeps one.
async fn keep(workspaces: &crate::workspaces::Workspaces, workspace: &str, address: &str) {
    let mut account = accounts::Account::new(address, accounts::email::TYPE_ID);
    account.config = json!({
        "email": address,
        "imap_host": "imap.example.test",
        "imap_port": 993,
        "smtp_host": "smtp.example.test",
        "smtp_port": 465,
    });
    workspaces
        .keep_workspace_account((workspace, &workspaces.acting_or_own(None), None), &account)
        .await
        .expect("the account is kept");
}

/// The listing the follow `id` reads now, as the window is sent it.
pub(super) fn listed(
    host: &impl tilo_actions::remote::ActionProvider,
    caller: &Caller,
    id: InvocationId,
) -> MailAccounts {
    let state = host
        .observe(caller, &id)
        .expect("the follow is the caller's");
    let tilo_actions::InvocationUpdate::Item { value, .. } = state.update else {
        panic!("a follow answers the whole listing, not {state:?}");
    };
    serde_json::from_value(value).expect("the listing reads")
}

/// A window following the workspace's accounts is sent the whole listing
/// again when another member removes one: no timer asks the host, and the
/// wake is the change.
#[tokio::test]
async fn a_window_following_the_accounts_is_sent_them_again_when_they_change() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let (workspace, workspace_id, host) = governed(&workspaces).await;
    let ada = admit(&workspaces, &workspace, &owner, 1).await;
    let bo = admit(&workspaces, &workspace, &owner, 2).await;
    let keeping = workspace_id.to_string();
    keep(&workspaces, &keeping, "ada@example.test").await;
    keep(&workspaces, &keeping, "bob@example.test").await;
    let hers = host.device_page(workspace_id, &ada);
    let ada = caller(&ada);
    let bos_page = host.device_page(workspace_id, &bo);
    let bo = caller(&bo);

    // Her window follows them: the host reads them as they are.
    let receipt = settled(hers.invoke(&ada, asking(person_accounts::WATCH, json!({}))))
        .await
        .unwrap();
    let id = receipt.invocation;
    assert!(
        matches!(receipt.status, ReceiptStatus::Accepted),
        "a follow stays open: {:?}",
        receipt.status
    );
    assert_eq!(listed(&hers, &ada, id).accounts.len(), 2);

    // Another member's window removes one through the same host, and hers is
    // woken by that change rather than asking again.
    let waking = hers.next_change(&ada, &id).expect("a follower is woken");
    settled(bos_page.invoke(
        &bo,
        asking(
            person_accounts::REMOVE,
            json!({"account": "bob@example.test"}),
        ),
    ))
    .await
    .unwrap();
    settled(waking).await;

    let now = listed(&hers, &ada, id);
    assert_eq!(
        now.accounts
            .iter()
            .map(|account| account.address.as_str())
            .collect::<Vec<_>>(),
        ["ada@example.test"]
    );
}
