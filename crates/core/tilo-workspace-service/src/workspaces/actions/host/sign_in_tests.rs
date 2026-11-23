//! Signing in on a provider's own page, from a window: a Nextcloud server's
//! login flow, begun, answered by a stand-in server, and kept as the
//! workspace's account.
//!
//! The server is a stand-in on loopback: no real account, password or
//! mailbox is touched.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{self, MailAccounts, SignInStep};
use tilo_actions::remote::{ActionProvider as _, InvocationReceipt, ReceiptStatus};

use super::super::tests::{admit, caller, governed, settled};
use super::accounts_tests::asking;
use tilo_server_protocol::WorkspaceId;

use crate::workspaces::Workspaces;
use crate::workspaces::tests::{governor, test_host};

/// What an action answered.
fn answered<T: serde::de::DeserializeOwned>(receipt: InvocationReceipt) -> T {
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the server answers at once, not {:?}", receipt.status);
    };
    serde_json::from_value(output.value.expect("an answer")).expect("the answer reads")
}

/// The account of `workspace` of the kind `type_id`, once the workspace's accounts keep one.
async fn kept_of_type(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    type_id: &str,
) -> Option<accounts::Account> {
    workspaces
        .workspace_accounts(&workspace.to_string())
        .await
        .ok()?
        .of_type(type_id)
        .into_iter()
        .next()
}

/// The account of `workspace` of the kind `type_id`, once the background poll kept it.
async fn kept_soon(workspaces: &Workspaces, workspace: WorkspaceId, type_id: &str) {
    let kept = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while kept_of_type(workspaces, workspace, type_id).await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(kept.is_ok(), "the {type_id} account is kept");
}

/// The sign-in the account `id` of a signed-in workspace names: what the entry of the
/// workspace's vault it names holds.
async fn sealed(workspaces: &Workspaces, workspace: WorkspaceId, id: &str) -> Option<String> {
    let workspace = workspace.to_string();
    let named = workspaces
        .workspace_accounts(&workspace)
        .await
        .ok()?
        .account(id)
        .and_then(|kept| {
            accounts::email::password_item(&kept.config)
                .and_then(tilo_actions::remote::person_vault::ItemRef::parse)
        })?;
    let vault = workspaces.open_vault(&workspace).await.ok()?;
    vault.entry(named.id()).map(|entry| entry.password.clone())
}

/// A stand-in Nextcloud server that starts a login flow, and answers its
/// poll with a minted app password while `completes` says so.
fn nextcloud_server(
    completes: &Arc<std::sync::atomic::AtomicBool>,
) -> (test_http::Responder, Arc<Mutex<String>>) {
    use test_http::{Reply, Responder};

    let base = Arc::new(Mutex::new(String::new()));
    let for_handler = Arc::clone(&base);
    let answering = Arc::clone(completes);
    let responder = Responder::handle(move |request| {
        let base = for_handler.lock().unwrap().clone();
        match (request.method.as_str(), request.target.as_str()) {
            ("POST", "/index.php/login/v2") => Some(Reply::status(200).body(
                serde_json::json!({
                    "poll": {"token": "poll-token", "endpoint": format!("{base}/login/v2/poll")},
                    "login": format!("{base}/login/v2/flow/abc"),
                })
                .to_string(),
            )),
            ("POST", "/login/v2/poll") => Some(if answering.load(std::sync::atomic::Ordering::SeqCst)
            {
                Reply::status(200).body(
                    serde_json::json!({
                        "server": base,
                        "loginName": "ada",
                        "appPassword": "minted-app-password",
                    })
                    .to_string(),
                )
            } else {
                Reply::status(404)
            }),
            _ => Some(Reply::status(404)),
        }
    });
    *base.lock().unwrap() = responder.url();
    (responder, base)
}

/// A Nextcloud server's login flow keeps the account, its app password and
/// the calendar server of the same sign-in; a flow begun is its workspace's
/// alone, and cancelling stops waiting.
#[tokio::test]
async fn a_nextcloud_login_flow_keeps_the_account_and_its_calendar() {
    let completes = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (responder, _base) = nextcloud_server(&completes);

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.in_workspace_as(workspace_id, &device);
    let ada = caller(&device);

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "nextcloud", "server": responder.url()}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(step.open, format!("{}/login/v2/flow/abc", responder.url()));
    kept_soon(workspaces, workspace_id, accounts::cloud_types::NEXTCLOUD).await;
    let app_password = sealed(workspaces, workspace_id, "nextcloud:ada@127.0.0.1")
        .await
        .unwrap_or_default();
    assert_eq!(app_password, "minted-app-password");
    kept_soon(workspaces, workspace_id, "caldav").await;
    let calendar = kept_of_type(workspaces, workspace_id, "caldav")
        .await
        .expect("the calendar server is kept");
    assert_eq!(
        calendar
            .config
            .get("calendar_provider")
            .and_then(Value::as_str),
        Some("nextcloud")
    );

    // A flow nobody approves waits, and cancelling stops it.
    completes.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "nextcloud", "server": responder.url()}),
            ),
        ))
        .await
        .unwrap(),
    );
    let listed: MailAccounts = answered(
        settled(page.invoke(&ada, asking(person_accounts::LIST, json!({}))))
            .await
            .unwrap(),
    );
    assert_eq!(listed.signing_in.len(), 1);

    // Another workspace's member shows none of it, and can stop none of it.
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let bob_device = admit(workspaces, &elsewhere, &owner, 2).await;
    let bob_page = actions.in_workspace_as(elsewhere_id, &bob_device);
    let theirs: MailAccounts = answered(
        settled(bob_page.invoke(
            &caller(&bob_device),
            asking(person_accounts::LIST, json!({})),
        ))
        .await
        .unwrap(),
    );
    assert!(theirs.signing_in.is_empty());
    assert!(
        settled(bob_page.invoke(
            &caller(&bob_device),
            asking(
                person_accounts::CANCEL_SIGN_IN,
                json!({"signIn": waiting.id})
            ),
        ))
        .await
        .is_err()
    );
    let stopped: MailAccounts = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::CANCEL_SIGN_IN,
                json!({"signIn": waiting.id}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(stopped.signing_in.is_empty());
}

/// Under the default public-only reach, beginning a Nextcloud sign-in to an
/// address of this machine or its network is refused before anything is sent.
#[tokio::test]
async fn a_nextcloud_sign_in_to_an_inside_address_is_refused_and_touches_nobody() {
    let silent = test_http::Responder::silent();

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.in_workspace_as(workspace_id, &device);
    let ada = caller(&device);

    for inside in [
        silent.url(),
        format!("http://localhost:{}", silent.port()),
        "https://[::1]".into(),
        "https://10.0.0.7".into(),
        "https://2130706433".into(),
    ] {
        let refused = settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "nextcloud", "server": inside}),
            ),
        ))
        .await
        .unwrap_err();
        assert_eq!(
            refused,
            tilo_actions::remote::ActionCallError::Refused {
                reason: "That server's address is not one this server may call".into(),
            },
            "{inside}"
        );
    }
    silent.assert_untouched();
}
