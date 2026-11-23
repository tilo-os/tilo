//! A workspace's mail accounts as a live read: a window that follows them is
//! sent the whole listing again when they change, woken by the change rather
//! than asking for it again.
//!
//! A page keeps no vault and no mail connection, so its window reads the
//! accounts the workspace keeps, and another member's add or removal must
//! reach it when it happens.

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::person_accounts::{self, MailAccounts, MailArrivals};
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
        .keep_workspace_account((workspace, &workspaces.acting_or_own(None)), &account)
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
    let hers = host.in_workspace_as(workspace_id, &ada);
    let ada = caller(&ada);
    let bos_page = host.in_workspace_as(workspace_id, &bo);
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

/// What a receipt answered, as the arrivals it carries.
fn arrived(receipt: tilo_actions::remote::InvocationReceipt) -> MailArrivals {
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the host answers at once, not {:?}", receipt.status);
    };
    serde_json::from_value(output.value.expect("an answer")).expect("the arrivals read")
}

/// What arrived in a workspace's inboxes is its members' to ask for, and
/// asking is listening: the host watches its inboxes from then on, with no
/// window open. A member of another workspace reads their own log, which
/// holds none of this one's mail, and neither an app nor an agent with no
/// device behind it reaches it at all.
#[tokio::test]
async fn a_member_asks_what_arrived_and_the_host_watches_from_then_on() {
    use crate::workspaces::mail_watch::testing::{Watched, keep as keep_signed_in, told};
    use tilo_mail_session::InboxSeen;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let servers = std::sync::Arc::new(Watched::default());
    workspaces.carry_mail_through(servers.clone());
    let (workspace, workspace_id, host) = governed(&workspaces).await;
    let ada = admit(&workspaces, &workspace, &owner, 1).await;
    keep_signed_in(
        &workspaces,
        &workspace_id.to_string(),
        "ada@example.test",
        "adas-password",
    )
    .await;
    let page = host.in_workspace_as(workspace_id, &ada);
    let ada = caller(&ada);
    assert!(servers.running().is_empty(), "nobody asked yet");

    let first = arrived(
        settled(page.invoke(&ada, asking(person_accounts::ARRIVED, json!({}))))
            .await
            .unwrap(),
    );
    assert!(first.arrivals.is_empty());
    assert_eq!(first.newest, 0);
    workspaces.mail_watches_settled().await;
    assert_eq!(servers.running().len(), 1, "asking is listening");

    servers.heard("ada@example.test").arrived(
        vec![told(
            4,
            "jonas@northzone.example",
            "Cap table",
            "Could you send it?",
        )],
        InboxSeen {
            uid_validity: 7,
            uid: 4,
        },
    );
    let then = arrived(
        settled(page.invoke(
            &ada,
            asking(person_accounts::ARRIVED, json!({"after": first.newest})),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(then.arrivals.len(), 1);
    assert_eq!(then.arrivals[0].subject, "Cap table");
    assert_eq!(
        then.arrivals[0].facts()["from"],
        json!("jonas@northzone.example")
    );
    assert_eq!(then.newest, 1);
    workspaces.mail_watches_settled().await;
    assert_eq!(
        servers.running().len(),
        1,
        "asking again starts no second watch"
    );

    // A member of another workspace reads their own log, never this one.
    let (elsewhere, elsewhere_id, _) = governed(&workspaces).await;
    let bo = admit(&workspaces, &elsewhere, &owner, 2).await;
    let elsewhere_page = host.in_workspace_as(elsewhere_id, &bo);
    let theirs = arrived(
        settled(elsewhere_page.invoke(&caller(&bo), asking(person_accounts::ARRIVED, json!({}))))
            .await
            .unwrap(),
    );
    assert!(theirs.arrivals.is_empty());

    // An app, and an agent with no device behind it, are nobody here.
    let asking_arrivals = asking(person_accounts::ARRIVED, json!({}));
    let app =
        settled(page.invoke(&Caller::local_app("so.tilo.chat"), asking_arrivals.clone())).await;
    assert!(
        matches!(
            app,
            Err(tilo_actions::remote::ActionCallError::UnknownAction)
        ),
        "{app:?}"
    );
    let unowned = settled(page.invoke(&Caller::local_agent("writer"), asking_arrivals)).await;
    assert!(
        matches!(
            unowned,
            Err(tilo_actions::remote::ActionCallError::UnknownAction)
        ),
        "{unowned:?}"
    );
}
