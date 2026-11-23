//! A connector's secret lives in the workspace's vault and reaches the worker that signs in
//! with it; no record and nothing a program reads holds it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion"
)]

use std::sync::Arc;

use super::*;
use crate::workspaces::fixtures::{TestGroupKeys, governor};

/// The secret the connector signs in with: made up, and nobody's.
const TOKEN: &str = "discord-bot-token-made-up";

/// The connector's secret is kept in the workspace's vault, and the store the worker reads
/// answers it there and nowhere else: not through a record, and not through an item the
/// connector never named.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connectors_secret_reaches_the_worker_from_the_vault_and_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let keys = Arc::new(TestGroupKeys::default());
    let host = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = host.workspaces();
    workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Discord").await.unwrap().workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: tilo_app_id::apps::PASSWORDS.into(),
        cluster_id: workspace.clone(),
        document: tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT.into(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();

    let connectors = Connectors::new();
    let store = connectors.secrets(&workspaces, &workspace, "so.tilo.discord");
    assert!(
        store
            .item("bot-token", "default")
            .get()
            .await
            .unwrap()
            .is_none(),
        "the vault keeps nothing yet"
    );

    // What the person does in Passwords: the token is an entry of the workspace's vault, named
    // for the connector so the vault says where it came from.
    let title = person_vault::connector_title("so.tilo.discord", "bot-token", "default");
    let place =
        accounts::vault_items::Place::parse(&person_vault::connector_place("so.tilo.discord"))
            .unwrap();
    let kept = workspaces
        .changing_vault(&workspace, |items| {
            items
                .keep(
                    None,
                    NewItem::new(
                        ItemKind::Value,
                        vec![place],
                        String::new(),
                        TOKEN.to_owned(),
                    ),
                    &title,
                )
                .map(|facts| ItemRef::from_id(facts.id.as_str()).unwrap())
        })
        .await
        .unwrap();

    let released = store
        .item("bot-token", "default")
        .get()
        .await
        .unwrap()
        .expect("the worker is handed the secret");
    assert_eq!(released.expose(), TOKEN.as_bytes());
    assert!(
        store.get("password/default").await.unwrap().is_none(),
        "a purpose the worker may not ask for is none"
    );
    assert!(
        store.get("bot-token/other").await.unwrap().is_none(),
        "an item the vault does not hold for this connector is none"
    );
    let elsewhere = Connectors::new().secrets(&workspaces, &workspace, "so.tilo.other");
    assert!(
        elsewhere
            .item("bot-token", "default")
            .get()
            .await
            .unwrap()
            .is_none(),
        "another connector is handed nothing of this one's"
    );

    // The record a program follows names the item, never the secret: the reference is what a
    // person's change carries and what a program reads.
    let referenced = kept.to_string();
    assert!(!referenced.contains(TOKEN));
    let record = format!(r#"{{"title":"Bridge","token":{referenced:?}}}"#);
    assert!(
        !record.contains(TOKEN),
        "no record holds the secret a connector signs in with"
    );
    host.shutdown().await;
}
