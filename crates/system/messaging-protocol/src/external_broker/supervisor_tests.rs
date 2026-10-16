use super::*;
use crate::supervisor::{AccountSupervisor, whatsapp::WhatsappBootstrap};
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_messaging::{AccountState, ServiceAccount};

#[tokio::test]
async fn actual_bootstrap_and_account_lifecycle_reach_messages_without_login_or_sending() {
    let root = tempfile::tempdir().unwrap();
    let owner = tests::owner(crate::Service::Whatsapp);
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = crate::whatsapp_store::EncryptedBackend::open_with_key(
        &root.path().join("sdk"),
        owner.clone(),
        secret_vault::VaultKey::from_bytes([23; 32]),
    )
    .unwrap();
    let bootstrap = WhatsappBootstrap::build(backend, receipt_tests::network(&calls))
        .await
        .unwrap();
    let source = bootstrap.client();
    let fixture = receipt_tests::assemble(root, owner, calls, source);
    let supervisor = AccountSupervisor::new(&fixture.owner).unwrap();
    let account = ServiceAccount::new("whatsapp", &fixture.owner.account);
    let client = supervisor.client();
    assert_eq!(client.account_state(&account), AccountState::Unavailable);
    assert_eq!(
        supervisor.unavailable(AccountState::Ready),
        Err(SendError::InvalidRequest)
    );
    let mut app = receipt_tests::messages_with_client(&fixture, client.clone());
    supervisor.unavailable(AccountState::Linking).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label()
                    == Some(
                        "Linking this account… Confirm the request on your phone."
                    )))
    );
    supervisor.unavailable(AccountState::Locked).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(
                    |(_, node)| node.label() == Some("Unlock this account to use messaging.")
                ))
    );
    assert!(!client.supports(&fixture.route));
    supervisor
        .install_whatsapp(Arc::new(fixture.broker.clone()), bootstrap)
        .await
        .unwrap();
    // An SDK that has not logged in/connected must never advertise readiness.
    assert_eq!(client.account_state(&account), AccountState::NeedsRelink);
    let states = app
        .effect(&messages::MessagesEffect::ListAccountStates)
        .unwrap()
        .value
        .unwrap();
    assert!(
        states
            .as_array()
            .unwrap()
            .iter()
            .any(|state| state["service"] == "whatsapp"
                && state["account"] == fixture.owner.account
                && state["state"] == "needs-relink")
    );
    assert!(!client.supports(&fixture.route));
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label()
                    == Some("Link this account again to resume messaging.")))
    );
    supervisor.unavailable(AccountState::Failed).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| h
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label()
                    == Some(
                        "This account's connection stopped. Check its connection and account setup."
                    )))
    );
    assert_eq!(
        client.account_state(&ServiceAccount::new("signal", &fixture.owner.account)),
        AccountState::Unavailable
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(
        !app.harness()
            .dump_json()
            .contains("private fixture message")
    );
}

#[tokio::test]
async fn supervisor_freezes_link_generation_and_cannot_install_another_owner_or_ready_by_label() {
    let fixture = receipt_tests::fixture().await;
    let mut stale = fixture.owner.clone();
    stale.linking_generation = stale.linking_generation.saturating_add(1);
    let supervisor = AccountSupervisor::new(&stale).unwrap();
    assert_eq!(
        supervisor.install_external(&fixture.broker),
        Err(SendError::Unavailable)
    );
    assert_eq!(
        supervisor.unavailable(AccountState::Ready),
        Err(SendError::InvalidRequest)
    );
    let backend = crate::whatsapp_store::EncryptedBackend::open_with_key(
        &fixture.root.path().join("stale-sdk"),
        fixture.owner.clone(),
        secret_vault::VaultKey::from_bytes([24; 32]),
    )
    .unwrap();
    let bootstrap = WhatsappBootstrap::build(backend, receipt_tests::network(&fixture.calls))
        .await
        .unwrap();
    assert_eq!(
        supervisor
            .install_whatsapp(Arc::new(fixture.broker.clone()), bootstrap)
            .await,
        Err(SendError::Revoked)
    );

    assert_eq!(
        supervisor
            .client()
            .account_state(&ServiceAccount::new("whatsapp", &stale.account)),
        AccountState::Unavailable
    );
    assert!(!supervisor.client().supports(&fixture.route));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}
