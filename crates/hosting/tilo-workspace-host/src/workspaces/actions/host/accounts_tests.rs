//! The person's own mail accounts as a live read: a window that follows
//! them is sent the whole listing again when they change, woken by the
//! change rather than asking for it again.
//!
//! A page keeps no vault and no mail connection, so its window reads the
//! accounts the host keeps, and another window's add or removal must reach
//! it when it happens.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay handle a test binds is kept until the test ends; dropping it earlier closes the host it drives"
)]

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::person_accounts::{self, MailAccounts};
use tilo_actions::remote::{ActionHost as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::HostWorkspaceActions;
use super::super::tests::{caller, settled};
use crate::workspaces::tests::{governor, test_host};

/// A device of the person `root`.
fn key_of(root: &SigningKey, label: &str) -> DeviceCert {
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// A request of one of the accounts actions, with `argument` when it has one.
fn asking(action: &str, argument: Value) -> InvokeRequest {
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
    }
}

/// `address` kept in the person's own store, as a window's add keeps one.
fn keep(root: &std::path::Path, address: &str) {
    let mut store = accounts::AccountStore::load_in(root);
    let mut account = accounts::Account::new(address, accounts::email::TYPE_ID);
    account.config = json!({
        "email": address,
        "imap_host": "imap.example.test",
        "imap_port": 993,
        "smtp_host": "smtp.example.test",
        "smtp_port": 465,
    });
    store.upsert(account);
    store.save_in(root).expect("the store saves");
}

/// The listing the follow `id` reads now, as the window is sent it.
fn listed(
    host: &impl tilo_actions::remote::ActionHost,
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

/// A window following the person's accounts is sent the whole listing
/// again when another window removes one: no timer asks the host, and the
/// wake is the change.
#[tokio::test]
async fn a_window_following_the_accounts_is_sent_them_again_when_they_change() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    let device = key_of(&root, "studio");
    let person = root.verifying_key().to_bytes();
    let keeping = workspaces.person_settings_root(&person);
    keep(&keeping, "ada@example.test");
    keep(&keeping, "bob@example.test");
    let page = host.in_workspace_as(
        workspaces.create("Mail").await.unwrap().workspace_id,
        &device,
    );
    let caller = caller(&device);

    // The window follows them: the host reads them as they are.
    let receipt = settled(page.invoke(&caller, asking(person_accounts::WATCH, json!({}))))
        .await
        .unwrap();
    let id = receipt.invocation;
    assert!(
        matches!(receipt.status, ReceiptStatus::Accepted),
        "a follow stays open: {:?}",
        receipt.status
    );
    assert_eq!(listed(&page, &caller, id).accounts.len(), 2);

    // Another window removes one through the same host, and this one is
    // woken by that change rather than asking again.
    let waking = page.next_change(&caller, &id).expect("a follower is woken");
    settled(page.invoke(
        &caller,
        asking(
            person_accounts::REMOVE,
            json!({"account": "bob@example.test"}),
        ),
    ))
    .await
    .unwrap();
    settled(waking).await;

    let now = listed(&page, &caller, id);
    assert_eq!(
        now.accounts
            .iter()
            .map(|account| account.address.as_str())
            .collect::<Vec<_>>(),
        ["ada@example.test"]
    );
}
