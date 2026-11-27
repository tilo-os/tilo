//! Clipboard continuity between two of one person's devices, over the action service each
//! daemon already serves: what is copied on one is kept by the other, and a device of another
//! person neither receives it nor offers its own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_device_identity::login::{ISSUE, LOG_IN, Place};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, NoActions};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};

use super::*;
use crate::device::ticket::Tickets;
use crate::device::ticket_tests::{self, Issuing};

/// Two of one person's devices on this machine, holding one workspace both are members of: the
/// second logged in with a ticket the first issued.
pub(in crate::device) struct TwoDevices {
    pub(in crate::device) person: [u8; 32],
    /// The workspace both are members of: where the copies are shared.
    pub(in crate::device) workspace: String,
    pub(in crate::device) first: Arc<ServiceWorkspaces>,
    pub(in crate::device) first_cert: DeviceCert,
    /// The first device's own actions, its root-only ones among them.
    pub(in crate::device) first_tickets: Tickets,
    pub(in crate::device) second: Arc<ServiceWorkspaces>,
    pub(in crate::device) second_cert: DeviceCert,
    /// The first device's action service, served by the daemon that issued the ticket.
    pub(in crate::device) first_router: iroh::protocol::Router,
    /// The second device's action service.
    pub(in crate::device) _second_router: iroh::protocol::Router,
    /// The key the second device's daemon holds, as it does until the device ends.
    pub(in crate::device) _second_daemon: tilo_device_identity::persist::DeviceKeyLock,
    pub(in crate::device) state: tempfile::TempDir,
}

pub(in crate::device) async fn two_devices() -> TwoDevices {
    let Issuing {
        workspace,
        issuer,
        state,
        issuer_cert: _,
        tickets,
        router,
    } = ticket_tests::issuing().await;
    let first_cert = issuer.device_cert();
    let person = first_cert.user_public_key;
    let printed = ticket_tests::ask_device(&tickets, ISSUE, json!({}))
        .await
        .expect("the first device issues a ticket");
    let text = printed["ticket"].as_str().expect("a ticket").to_owned();

    // A second computer with no certificate of the person's logs in with it.
    let second_key = SigningKey::from_bytes(&[0x73; 32]);
    let (_, placeholder) = ticket_tests::enrol(
        &SigningKey::from_bytes(&[0x74; 32]),
        "the desk",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let second_endpoint = ticket_tests::endpoint(&second_key).await;
    let place = Place::at(
        state.path().join("desk"),
        tilo_device_identity::IdentityPaths::under(state.path().join("desk-keys")),
    );
    let second_daemon = ticket_tests::the_daemons_key(place.keys(), &second_key);
    let desk = Tickets::new(
        (second_key.clone(), placeholder),
        second_endpoint.clone(),
        None,
        ticket_tests::no_root(),
        None,
        None,
        place,
        ActionService::new(second_key.clone(), Arc::new(NoActions)),
    );
    ticket_tests::ask_device(
        &desk,
        LOG_IN,
        json!({"ticket": text, "agent": "Codex", "label": "Desk"}),
    )
    .await
    .expect("the first device certifies the second");
    let login = desk
        .logged_in()
        .borrow()
        .clone()
        .expect("the second device starts again as the device it became");
    second_endpoint.close().await;

    // Its daemon starts again as that device, and holds the workspace the login entered.
    let second = ServiceWorkspaces::open_for_test(
        &state.path().join("desk-service"),
        &second_key,
        &login.cert,
        &tilo_api::daemon::enter::server_of(&login)
            .await
            .expect("the second device's server"),
    )
    .await
    .expect("the second device's service opens");
    let entered = tilo_test_env::within(
        "the second device enters",
        tilo_api::daemon::enter::connect(&second_key, &login, Some(second.lent())),
    )
    .await
    .expect("the second device enters");
    tilo_test_env::within(
        "the second device holds the workspace",
        second.hold(&entered),
    )
    .await;
    let second_cert = second.device_cert();
    let second_router = iroh::protocol::Router::builder(second.endpoint())
        .accept(ACTIONS_ALPN, second.copies().action_service())
        .spawn();
    TwoDevices {
        person,
        workspace: workspace.to_string(),
        first: issuer,
        first_cert,
        first_tickets: tickets,
        second,
        second_cert,
        first_router: router,
        _second_router: second_router,
        _second_daemon: second_daemon,
        state,
    }
}

/// Where `workspaces`' endpoint is bound on this machine, as one of the person's devices says
/// it.
pub(in crate::device) fn answers_at(
    workspaces: &Arc<ServiceWorkspaces>,
) -> Vec<std::net::SocketAddr> {
    workspaces
        .endpoint()
        .addr()
        .addrs
        .iter()
        .filter_map(|address| match address {
            iroh::TransportAddr::Ip(socket) => Some(*socket),
            _ => None,
        })
        .collect()
}

/// The clipboard continuity of `workspaces`, serving the person's devices record that lists
/// each device of `known` with where its endpoint answers.
async fn clipboard(
    workspaces: &Arc<ServiceWorkspaces>,
    known: &[(DeviceCert, Vec<std::net::SocketAddr>)],
) -> Arc<Clipboard> {
    tilo_workspace_service::workspaces::person_workspace::found(workspaces.copies())
        .await
        .expect("the person's own workspace");
    let person = workspaces.device_cert().user_public_key;
    let workspace = tilo_server_protocol::servers::person_workspace_id(&person);
    for (cert, addrs) in known {
        person_workspace::apply_on(
            workspaces.copies(),
            &workspace,
            DEVICES,
            Devices::keeping(cert).expect("a device certificate encodes"),
        )
        .await
        .expect("the person's record takes a device");
        let (key, certificate, addrs) = (
            hex::encode(cert.device_public_key),
            hex::encode(postcard::to_allocvec(cert).expect("a certificate encodes")),
            hex::encode(postcard::to_allocvec(addrs).expect("an address list encodes")),
        );
        person_workspace::apply_on(
            workspaces.copies(),
            &workspace,
            DEVICES,
            Devices::KEEP_FACT.with((key, certificate, "endpoint".into(), addrs)),
        )
        .await
        .expect("the person's record says where a device answers");
    }
    let record = || {
        let copies = workspaces.copies().clone();
        crate::device::workspaces::block_on_owned(async move {
            person_workspace::devices::of(&copies, &person).await
        })
    };
    until("the person's record lists every device", || {
        (record().listed(&person).len() == known.len()).then_some(())
    })
    .await;
    until("the record says where every device answers", || {
        let devices = record().certified_at_with_endpoints(&person, tilo_clock::unix_now_ns());
        (devices.iter().all(|(_, addrs)| !addrs.is_empty()) && devices.len() == known.len())
            .then_some(())
    })
    .await;
    let clipboard = Clipboard::new(workspaces);
    clipboard.register(&workspaces.copies().providers());
    clipboard
}

/// One call of the clipboard's provider, as `caller` asks it.
async fn ask(
    clipboard: &Clipboard,
    caller: &Caller,
    action: &str,
    arguments: Value,
) -> Result<Value, ActionCallError> {
    let receipt = clipboard
        .invoke(
            caller,
            InvokeRequest {
                asks: false,
                invocation: InvocationId::new(),
                target: RemoteActionTarget {
                    provider: PROVIDER.to_owned(),
                    address: ActionAddress::new(RESOURCE, action),
                },
                argument: match arguments {
                    Value::Object(fields) => InvokeArgument::Json(fields),
                    _ => InvokeArgument::None,
                },
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        )
        .await
        .expect("the provider answers")?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(ActionCallError::Refused {
            reason: error.to_string(),
        }),
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        other => Err(ActionCallError::Refused {
            reason: format!("{other:?}"),
        }),
    }
}

/// One copy as a device shares it.
fn copy(text: &str, created_unix_ms: u64) -> Value {
    json!({
        "mime": "text/plain;charset=utf-8",
        "bytes": hex::encode(text),
        "created_unix_ms": created_unix_ms,
    })
}

/// Wait for `done`, a moment at a time, for [`DEADLINE`].
async fn until<T>(what: &str, mut done: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(answer) = done() {
            return answer;
        }
        assert!(std::time::Instant::now() < deadline, "waited for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copy_on_one_device_reaches_the_other_and_never_a_stranger() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = two_devices().await;
    let known = [
        (devices.first_cert.clone(), answers_at(&devices.first)),
        (devices.second_cert.clone(), answers_at(&devices.second)),
    ];
    let first = clipboard(&devices.first, &known).await;
    let second = clipboard(&devices.second, &known).await;
    let first_caller = Caller::authenticated(devices.person, devices.first_cert.device_public_key)
        .in_workspace(devices.workspace.clone());
    let second_caller =
        Caller::authenticated(devices.person, devices.second_cert.device_public_key)
            .in_workspace(devices.workspace.clone());

    // A copy that is too large, or is secret, is refused before it leaves.
    let too_large = json!({
        "mime": "text/plain;charset=utf-8",
        "bytes": hex::encode(vec![b'a'; MAX_ITEM_BYTES.saturating_add(1)]),
    });
    assert!(
        ask(&first, &first_caller, SHARE, json!({"copies": [too_large]}))
            .await
            .is_err(),
        "a copy over the bound is refused"
    );
    let secret = json!({"mime": tilo_clipboard::SECRET_MIME, "bytes": hex::encode("hunter2")});
    assert!(
        ask(&first, &first_caller, SHARE, json!({"copies": [secret]}))
            .await
            .is_err(),
        "a secret copy never leaves the device it was copied on"
    );

    // A share names at most what the clipboard keeps.
    let many: Vec<Value> = (0..=MAX_ITEMS as u64)
        .map(|at| copy("one of many", at))
        .collect();
    assert!(
        ask(&first, &first_caller, SHARE, json!({"copies": many}))
            .await
            .is_err(),
        "a share over the count is refused"
    );

    // What is copied here is shared as this device's copy.
    ask(
        &first,
        &first_caller,
        SHARE,
        json!({"copies": [copy("made earlier", 10), copy("made here", 20)]}),
    )
    .await
    .expect("the copies are shared");

    // They reach the person's other device, newest first, marked with the device they came from.
    let reading = until("the other device takes the copies", || {
        let reading = second.reading();
        (reading.copies.len() == 2).then_some(reading)
    })
    .await;
    assert_eq!(
        reading.copies[0].origin,
        hex::encode(devices.first_cert.device_public_key),
        "the copy says which device made it"
    );
    assert!(
        hex::decode(&reading.copies[0].bytes).expect("hex") == b"made here"
            && hex::decode(&reading.copies[1].bytes).expect("hex") == b"made earlier",
        "newest first, as the clipboard lists them: {reading:?}"
    );

    // What a device offers now replaces what it offered before.
    ask(
        &first,
        &first_caller,
        SHARE,
        json!({"copies": [copy("made here", 20)]}),
    )
    .await
    .expect("the one copy is shared");
    let replaced = until("the other device takes only the newest offer", || {
        let reading = second.reading();
        (reading.copies.len() == 1).then_some(reading)
    })
    .await;
    assert!(
        hex::decode(&replaced.copies[0].bytes).expect("hex") == b"made here",
        "the share is what the device offers now: {replaced:?}"
    );

    // A device of another person never takes what the person copied: it is no device of theirs.
    let stranger = Caller::authenticated([9; 32], [8; 32]).in_workspace(devices.workspace.clone());
    let refused = ask(
        &second,
        &stranger,
        RECEIVE,
        json!({"device": hex::encode([8; 32]), "copies": [copy("not yours", 30)]}),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionCallError::NotPermitted)),
        "a device of another person is refused: {refused:?}"
    );
    assert!(
        second
            .reading()
            .copies
            .iter()
            .all(|copy| copy.origin != hex::encode([8; 32])),
        "nothing it offered is kept"
    );

    // Nor can a device put a copy in another of the person's mouths.
    let forged = ask(
        &second,
        &second_caller,
        RECEIVE,
        json!({
            "device": hex::encode(devices.first_cert.device_public_key),
            "copies": [copy("not the first device's", 40)],
        }),
    )
    .await;
    assert!(
        forged.is_err(),
        "copies naming another device than the sender are refused: {forged:?}"
    );
    assert!(
        second
            .reading()
            .copies
            .iter()
            .all(|copy| hex::decode(&copy.bytes).expect("hex") != b"not the first device's"),
        "the forged copy is not kept"
    );
}
