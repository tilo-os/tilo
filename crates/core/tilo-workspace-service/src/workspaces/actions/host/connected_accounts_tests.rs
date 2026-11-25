//! Connecting a person's account at an issuer an app describes, from that app, against a
//! stand-in issuer on loopback: the account it confirms is the person's alone, read back only by
//! the app that described the issuer, and an issuer that refuses the address the browser came
//! back to says which address to register.

use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{self, ConnectedAccount, SignInStep};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, InvocationReceipt, ReceiptStatus,
};
use tilo_oauth::fake::{FakeAuthorizationServer, browse};

use super::super::tests::{admit, caller, governed, settled};
use super::accounts_tests::asking;
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
    assert!(
        matches!(&outside, ActionCallError::Refused { reason } if reason.contains("from the app")),
        "{outside:?}"
    );

    let step: SignInStep = answered(
        settled(page.invoke(
            &in_bridge,
            asking(person_accounts::CONNECT_ACCOUNT, connect(&server, port)),
        ))
        .await
        .unwrap(),
    );
    let open = step.open.clone();
    let browser = tokio::task::spawn_blocking(move || browse(&open));
    let account: ConnectedAccount = answered(
        settled(page.invoke(
            &in_bridge,
            asking(
                person_accounts::FINISH_CONNECT_ACCOUNT,
                json!({"signIn": step.id}),
            ),
        ))
        .await
        .unwrap(),
    );
    browser.await.unwrap().expect("the browser came back");
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
    assert_eq!(
        workspaces.connected_accounts(&here, &person),
        vec![account.clone()]
    );

    // The grant is in the person's vault, and only there.
    let grants = |vault: &crate::workspaces::vault::OpenVault| {
        vault
            .entries()
            .iter()
            .filter(|(item, _)| item.username == "ada")
            .count()
    };
    let vault = workspaces.open_vault(&here).await.unwrap();
    assert_eq!(grants(&vault), 1);
    let kept = std::fs::read_to_string(
        workspaces
            .workspace_root(&here)
            .join("connected-accounts")
            .join(&person)
            .join(BRIDGE)
            .join("fake.json"),
    )
    .unwrap();
    assert!(
        !kept.contains("access-"),
        "no token is kept beside the workspace: {kept}"
    );

    // Connecting the same account again keeps one grant, the new one.
    server.set_account("80351110224678912", "ada");
    let again: SignInStep = answered(
        settled(page.invoke(
            &in_bridge,
            asking(person_accounts::CONNECT_ACCOUNT, connect(&server, port)),
        ))
        .await
        .unwrap(),
    );
    let open = again.open.clone();
    let browser = tokio::task::spawn_blocking(move || browse(&open));
    let _: ConnectedAccount = answered(
        settled(page.invoke(
            &in_bridge,
            asking(
                person_accounts::FINISH_CONNECT_ACCOUNT,
                json!({"signIn": again.id}),
            ),
        ))
        .await
        .unwrap(),
    );
    browser.await.unwrap().expect("the browser came back");
    let vault = workspaces.open_vault(&here).await.unwrap();
    assert_eq!(grants(&vault), 1, "the earlier grant went");

    // An issuer that refuses the address the browser came back to says which to register.
    server.refuse_redirect();
    let refused: SignInStep = answered(
        settled(page.invoke(
            &in_bridge,
            asking(person_accounts::CONNECT_ACCOUNT, connect(&server, port)),
        ))
        .await
        .unwrap(),
    );
    let open = refused.open.clone();
    let browser = tokio::task::spawn_blocking(move || browse(&open));
    let error = settled(page.invoke(
        &in_bridge,
        asking(
            person_accounts::FINISH_CONNECT_ACCOUNT,
            json!({"signIn": refused.id}),
        ),
    ))
    .await
    .unwrap_err();
    browser.await.unwrap().expect("the browser came back");
    let register = format!("Add http://127.0.0.1:{port}/oauth to your app's redirects");
    assert!(
        matches!(&error, ActionCallError::Refused { reason } if reason.contains(&register)),
        "{error:?}"
    );

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
    assert!(
        matches!(&other, ActionCallError::Refused { reason } if reason.contains("not begun here")),
        "{other:?}"
    );
    // The app that began it still finishes it.
    let open = stray.open.clone();
    let browser = tokio::task::spawn_blocking(move || browse(&open));
    let _ = settled(page.invoke(
        &in_bridge,
        asking(
            person_accounts::FINISH_CONNECT_ACCOUNT,
            json!({"signIn": stray.id}),
        ),
    ))
    .await;
    browser.await.unwrap().expect("the browser came back");
}
