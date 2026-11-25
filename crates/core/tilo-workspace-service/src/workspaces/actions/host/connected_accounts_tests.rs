//! Connecting a person's account at an issuer an app describes, from that app, against a
//! stand-in issuer on loopback: the account it confirms is the person's alone, read back only by
//! the app that described the issuer, and an issuer that refuses the address the browser came
//! back to says which address to register.

use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{self, ConnectedAccount, SignInStep};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, ReceiptStatus,
};
use tilo_oauth::fake::{FakeAuthorizationServer, browse};

use super::super::tests::{admit, caller, governed, settled};
use super::accounts_tests::asking;
use crate::workspaces::Workspaces;
use crate::workspaces::tests::{governor, test_host};

const BRIDGE: &str = "so.tilo.bridge";

fn answered<T: serde::de::DeserializeOwned>(receipt: InvocationReceipt) -> T {
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the server answers at once, not {:?}", receipt.status);
    };
    serde_json::from_value(output.value.expect("an answer")).expect("the answer reads")
}

/// A port free on this computer now, for the issuer's one registered redirect.
fn free_port() -> u16 {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .expect("a free loopback port")
        .port()
}

/// The stand-in issuer as the app describes it, with its client.
fn connect(server: &FakeAuthorizationServer, port: u16) -> Value {
    let issuer = server.described_issuer(port);
    json!({
        "issuer": {
            "id": issuer.id,
            "label": issuer.label,
            "authorizationEndpoint": issuer.authorization_endpoint,
            "tokenEndpoint": issuer.token_endpoint,
            "identityEndpoint": issuer.identity_endpoint,
            "revocationEndpoint": issuer.revocation_endpoint,
            "idField": issuer.id_field,
            "nameField": issuer.name_field,
            "scopes": issuer.scopes,
            "redirectPort": issuer.redirect_port,
        },
        "clientId": "bridge-client",
    })
}

/// `who` asks `page` to connect at `issuer`, the browser goes there and back, and the page waits
/// for what came of it.
async fn connected(
    page: &(impl ActionProvider + Sync),
    who: &Caller,
    issuer: Value,
) -> Result<ConnectedAccount, ActionCallError> {
    let step: SignInStep = answered(
        settled(page.invoke(who, asking(person_accounts::CONNECT_ACCOUNT, issuer)))
            .await
            .unwrap(),
    );
    let open = step.open.clone();
    let browser = tokio::task::spawn_blocking(move || browse(&open));
    let came = settled(page.invoke(
        who,
        asking(
            person_accounts::FINISH_CONNECT_ACCOUNT,
            json!({"signIn": step.id}),
        ),
    ))
    .await;
    browser.await.unwrap().expect("the browser came back");
    came.map(answered)
}

/// How many grants for `name` the workspace's vault keeps.
fn grants(vault: &crate::workspaces::vault::OpenVault, name: &str) -> usize {
    vault
        .entries()
        .iter()
        .filter(|(item, _)| item.username == name)
        .count()
}

fn refused_with(error: &ActionCallError, words: &str) -> bool {
    matches!(error, ActionCallError::Refused { reason } if reason.contains(words))
}

/// The account's grant is in the person's vault, and only there: the one `ada` grant, and no
/// token kept beside the workspace for the bridge's fake issuer.
async fn only_in_the_vault(workspaces: &Workspaces, here: &str, person: &str) {
    assert_eq!(
        grants(&workspaces.open_vault(here).await.unwrap(), "ada"),
        1
    );
    let kept = std::fs::read_to_string(
        workspaces
            .workspace_root(here)
            .join("connected-accounts")
            .join(person)
            .join(BRIDGE)
            .join("fake.json"),
    )
    .unwrap();
    assert!(
        !kept.contains("access-"),
        "no token is kept beside the workspace: {kept}"
    );
}

#[tokio::test]
async fn a_person_connects_their_account_at_an_issuer_an_app_describes() {
    let server = FakeAuthorizationServer::start();
    server.set_account("80351110224678912", "ada");
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let host = test_host(dir.path(), &key, &owner).await;
    let workspaces = host.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let person = hex::encode(device.user_public_key);
    let here = workspace_id.to_string();
    let in_bridge = caller(&device).through_app(BRIDGE);

    // Only a person, through the app that describes the issuer, connects an account there.
    let outside = settled(page.invoke(
        &caller(&device),
        asking(person_accounts::CONNECT_ACCOUNT, connect(&server, port)),
    ))
    .await
    .unwrap_err();
    assert!(refused_with(&outside, "from the app"), "{outside:?}");

    let account = connected(&page, &in_bridge, connect(&server, port))
        .await
        .expect("the account is connected");
    assert_eq!(
        account,
        ConnectedAccount {
            app: BRIDGE.to_owned(),
            issuer: "fake".to_owned(),
            label: "Fake Provider".to_owned(),
            id: "80351110224678912".to_owned(),
            name: "ada".to_owned(),
        }
    );
    let authorize = server
        .requests()
        .into_iter()
        .find(|request| request.target.starts_with("/authorize?"))
        .expect("the browser was sent to the issuer");
    assert!(
        authorize.target.contains(&format!(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A{port}%2Foauth"
        )),
        "the browser comes back to the one registered address: {}",
        authorize.target
    );

    // The app that described the issuer reads the account; another app, or another issuer,
    // reads nothing.
    let read = workspaces
        .connected_account(&here, (&person, BRIDGE, "fake"))
        .expect("the bridge reads the account its person connected");
    assert_eq!(
        (read.id.as_str(), read.name.as_str()),
        ("80351110224678912", "ada")
    );
    assert!(
        workspaces
            .connected_account(&here, (&person, "so.tilo.other", "fake"))
            .is_none()
    );
    assert!(
        workspaces
            .connected_account(&here, (&person, BRIDGE, "other"))
            .is_none()
    );
    assert_eq!(workspaces.connected_accounts(&here, &person), vec![account]);

    // The grant is in the person's vault, and only there.
    only_in_the_vault(workspaces, &here, &person).await;

    // Connecting the same account again keeps one grant, the new one.
    connected(&page, &in_bridge, connect(&server, port))
        .await
        .expect("connected again");
    let vault = workspaces.open_vault(&here).await.unwrap();
    assert_eq!(grants(&vault, "ada"), 1, "the earlier grant went");

    // An issuer that refuses the address the browser came back to says which to register.
    server.refuse_redirect();
    let error = connected(&page, &in_bridge, connect(&server, port))
        .await
        .unwrap_err();
    let register = format!("Add http://127.0.0.1:{port}/oauth to your app's redirects");
    assert!(refused_with(&error, &register), "{error:?}");

    // Another app cannot wait for a connect it did not begin.
    let stray: SignInStep = answered(
        settled(page.invoke(
            &in_bridge,
            asking(person_accounts::CONNECT_ACCOUNT, connect(&server, port)),
        ))
        .await
        .unwrap(),
    );
    let other = settled(page.invoke(
        &caller(&device).through_app("so.tilo.other"),
        asking(
            person_accounts::FINISH_CONNECT_ACCOUNT,
            json!({"signIn": stray.id}),
        ),
    ))
    .await
    .unwrap_err();
    assert!(refused_with(&other, "not begun here"), "{other:?}");
    // Its browser still comes back, so the connect is over.
    tokio::task::spawn_blocking(move || browse(&stray.open))
        .await
        .unwrap()
        .expect("the browser came back");
}

#[tokio::test]
async fn a_person_forgets_an_account_they_connected() {
    let server = FakeAuthorizationServer::start();
    server.set_account("80351110224678912", "ada");
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let host = test_host(dir.path(), &key, &owner).await;
    let workspaces = host.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let person = hex::encode(device.user_public_key);
    let here = workspace_id.to_string();
    connected(
        &page,
        &caller(&device).through_app(BRIDGE),
        connect(&server, port),
    )
    .await
    .expect("the account is connected");

    // The grant leaves the vault and goes back to the issuer.
    let forget = || {
        asking(
            person_accounts::FORGET_CONNECTED_ACCOUNT,
            json!({"app": BRIDGE, "issuer": "fake"}),
        )
    };
    let left: Vec<ConnectedAccount> = answered(
        settled(page.invoke(&caller(&device), forget()))
            .await
            .unwrap(),
    );
    assert!(left.is_empty());
    assert!(
        workspaces
            .connected_account(&here, (&person, BRIDGE, "fake"))
            .is_none()
    );
    assert_eq!(
        grants(&workspaces.open_vault(&here).await.unwrap(), "ada"),
        0
    );
    assert_eq!(server.revoked().len(), 1, "the issuer took the grant back");
    let again = settled(page.invoke(&caller(&device), forget()))
        .await
        .unwrap_err();
    assert!(refused_with(&again, "not connected"), "{again:?}");
}
