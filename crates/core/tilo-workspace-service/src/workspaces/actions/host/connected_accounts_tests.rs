//! Connecting a person's account at an issuer an app describes, from that app, against a
//! stand-in issuer on loopback: the account it confirms is the person's alone and kept in their
//! own workspace, read back only by the app that described the issuer, and an issuer that
//! refuses the address the browser came back to says which address to register.

use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{self, ConnectedAccount, SignInStep};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, ReceiptStatus,
};
use tilo_oauth::fake::{FakeAuthorizationServer, browse};
use tilo_server_protocol::servers::person_workspace_id;

use super::super::tests::{admit, caller, hosted, settled};
use super::accounts_tests::asking;
use crate::workspaces::person_workspace::{self, connected};
use crate::workspaces::tests::{hosting_device, test_host};

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

/// How many grants for `name` the vault of `person`'s own workspace keeps.
async fn grants(
    workspaces: &crate::workspaces::Workspaces,
    person: &[u8; 32],
    name: &str,
) -> usize {
    workspaces
        .open_vault(&person_workspace_id(person))
        .await
        .unwrap()
        .entries()
        .iter()
        .filter(|(item, _)| item.username == name)
        .count()
}

fn refused_with(error: &ActionCallError, words: &str) -> bool {
    matches!(error, ActionCallError::Refused { reason } if reason.contains(words))
}

/// What `person` connected (`account`, at the bridge's issuer) reads back as theirs alone: by
/// the app that described the issuer and by nobody else, the grant in their own workspace's
/// vault, and nothing secret in the record that reaches their devices.
async fn reads_only_theirs(
    workspaces: &crate::workspaces::Workspaces,
    (device, bo): (
        &tilo_user_identity::DeviceCert,
        &tilo_user_identity::DeviceCert,
    ),
    account: ConnectedAccount,
) {
    let person = hex::encode(device.user_public_key);
    // The app that described the issuer reads the account; another app, or another issuer,
    // reads nothing.
    let read = workspaces
        .connected_account((&person, BRIDGE, "fake"))
        .await
        .expect("the bridge reads the account its person connected");
    assert_eq!(
        (read.id.as_str(), read.name.as_str()),
        ("80351110224678912", "ada")
    );
    assert!(
        workspaces
            .connected_account((&person, "so.tilo.other", "fake"))
            .await
            .is_none()
    );
    assert!(
        workspaces
            .connected_account((&person, BRIDGE, "other"))
            .await
            .is_none()
    );
    assert_eq!(workspaces.connected_accounts(&person).await, vec![account]);
    let bo_person = hex::encode(bo.user_public_key);
    assert!(
        workspaces
            .connected_account((&bo_person, BRIDGE, "fake"))
            .await
            .is_none(),
        "another person reads nothing of hers"
    );

    // The grant is in the vault of the person's own workspace, and only there; the record of
    // what they connected, which reaches their devices, holds nothing secret.
    assert_eq!(grants(workspaces, &device.user_public_key, "ada").await, 1);
    let record = person_workspace::read_on::<connected::ConnectedAccounts>(
        workspaces,
        &person_workspace_id(&device.user_public_key),
        connected::CONNECTED,
    )
    .await
    .unwrap();
    let kept = format!("{record:?}");
    assert!(kept.contains("80351110224678912"), "{kept}");
    assert!(
        !kept.contains("access-"),
        "no token is in the record: {kept}"
    );
}

#[tokio::test]
async fn a_person_connects_their_account_at_an_issuer_an_app_describes() {
    let server = FakeAuthorizationServer::start();
    server.set_account("80351110224678912", "ada");
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let host = test_host(dir.path(), &key, &owner).await;
    let workspaces = host.workspaces();
    // The device that made the root founds the person's own workspace, where what they
    // connect is kept.
    person_workspace::found(workspaces)
        .await
        .expect("the person's own workspace");
    let (workspace, workspace_id, actions) = hosted(workspaces).await;
    let device = owner.clone();
    let page = actions.device_page(workspace_id, &device);
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let in_bridge = caller(&device).through_app(BRIDGE);

    // Only a person, through the app that describes the issuer, connects an account there.
    let outside = settled(page.invoke(
        &caller(&device),
        asking(person_accounts::CONNECT_ACCOUNT, connect(&server, port)),
    ))
    .await
    .unwrap_err();
    assert!(refused_with(&outside, "from the app"), "{outside:?}");
    // Nor does anyone on a device that is not theirs: what they connect is kept on their own.
    let elsewhere = settled(actions.device_page(workspace_id, &bo).invoke(
        &caller(&bo).through_app(BRIDGE),
        asking(person_accounts::CONNECT_ACCOUNT, connect(&server, port)),
    ))
    .await
    .unwrap_err();
    assert!(refused_with(&elsewhere, "not one of them"), "{elsewhere:?}");

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

    reads_only_theirs(workspaces, (&device, &bo), account).await;

    // Connecting the same account again keeps one grant, the new one.
    connected(&page, &in_bridge, connect(&server, port))
        .await
        .expect("connected again");
    assert_eq!(
        grants(workspaces, &device.user_public_key, "ada").await,
        1,
        "the earlier grant went"
    );

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
    let (key, owner) = hosting_device();
    let host = test_host(dir.path(), &key, &owner).await;
    let workspaces = host.workspaces();
    // The device that made the root founds the person's own workspace, where what they
    // connect is kept.
    person_workspace::found(workspaces)
        .await
        .expect("the person's own workspace");
    let (_, workspace_id, actions) = hosted(workspaces).await;
    let device = owner.clone();
    let page = actions.device_page(workspace_id, &device);
    let person = hex::encode(device.user_public_key);
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
            .connected_account((&person, BRIDGE, "fake"))
            .await
            .is_none()
    );
    assert_eq!(grants(workspaces, &device.user_public_key, "ada").await, 0);
    assert_eq!(server.revoked().len(), 1, "the issuer took the grant back");
    let again = settled(page.invoke(&caller(&device), forget()))
        .await
        .unwrap_err();
    assert!(refused_with(&again, "not connected"), "{again:?}");
}
