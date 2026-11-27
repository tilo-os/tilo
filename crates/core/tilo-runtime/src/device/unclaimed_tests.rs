#![allow(
    clippy::significant_drop_tightening,
    reason = "a test keeps what it serves until it ends, as the daemon does"
)]

//! A computer nobody set up is handed a ticket at its start: it spends it once, and the person's
//! certificate for its key is filed under the person; a later start leaves the ticket alone.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_device_identity::login::{ISSUE, LOG_IN, Place};

use super::first_workspace::FirstWorkspace;
use super::ticket::Tickets;
use super::ticket_tests::{ask_device, endpoint, root_in, serve};
use super::unclaimed::{TICKET_CREDENTIAL, claimed, take_handed_ticket};
use crate::device::services::tests::isolated_root;

const HANDED_TICKET_TEST: &str =
    "device::unclaimed_tests::a_handed_ticket_is_spent_once_and_files_the_persons_certificate";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one machine handed a ticket, then a second person, read top to bottom"
)]
async fn a_handed_ticket_is_spent_once_and_files_the_persons_certificate() {
    let Some(root) = isolated_root(HANDED_TICKET_TEST) else {
        return;
    };
    let workspace = tilo_desktop_model::WorkspaceId::new();
    let (issuer, state, issuer_key, issuer_cert, _paths) =
        super::workspaces::ServiceWorkspaces::test_hosting_person(workspace).await;
    let tickets = Tickets::new(
        (issuer_key.clone(), issuer_cert.clone()),
        issuer.endpoint(),
        None,
        root_in(state.path().to_path_buf()),
        Some(FirstWorkspace::founded(Arc::clone(&issuer), workspace)),
        Some(Arc::clone(&issuer)),
        Place::at(
            state.path().join("issuer"),
            tilo_device_identity::IdentityPaths::under(state.path().join("issuer-keys")),
        ),
        issuer.copies().action_service(),
    );
    let router = serve(&issuer, issuer.copies().action_service());
    let printed = ask_device(&tickets, ISSUE, json!({"capability": "member"}))
        .await
        .expect("the person's device issues a ticket");

    // A new machine: its device key, and nothing else. The isolated child starts with a person
    // of its own, which this test's machine is not.
    let _ = std::fs::remove_dir_all(tilo_device_identity::persist::collaboration_identity_root());
    if let Ok(dir) = tilo_identity::store::account_identity_dir_from_env() {
        let _ = std::fs::remove_dir_all(dir);
    }
    let keys = tilo_api::calls::device_keys();
    let key = SigningKey::from_bytes(&[0x5a; 32]);
    // Its daemon spends the ticket at its start, holding the key it took.
    let _machine_daemon = super::ticket_tests::the_daemons_key(&keys, &key);
    let place = Place::here(keys.clone()).unwrap();
    let credentials = root.join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    std::fs::write(
        credentials.join(TICKET_CREDENTIAL),
        format!("{}\n", printed["ticket"].as_str().unwrap()),
    )
    .unwrap();
    let own = crate::device::services::this_account_uid();
    assert!(
        !claimed(&place, &key, &root, own),
        "no identity names the key yet"
    );
    let machine_endpoint = endpoint(&key).await;
    let machine_service = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    assert_eq!(
        take_handed_ticket(&place, &key, &root, own, None)
            .await
            .map(|taken| taken.is_ok()),
        None,
        "a start handed no ticket spends none"
    );

    let device = take_handed_ticket(&place, &key, &root, own, Some(&credentials))
        .await
        .expect("an unclaimed machine spends the ticket it was handed")
        .unwrap_or_else(|why| panic!("the issuer certifies the machine: {why:#}"));
    let person = issuer_cert.user_public_key;
    assert_eq!(device.cert.user_public_key, person);
    assert_eq!(
        device.cert.device_public_key,
        key.verifying_key().to_bytes()
    );
    let filed =
        tilo_device_identity::persist::read_device_cert(&keys.device_cert_for_user(&person))
            .expect("the certificate is filed under the person it names");
    assert_eq!(filed, device.cert);
    assert!(
        claimed(&place, &key, &root, own),
        "the person's certificate names the key now"
    );

    // A second start: the ticket beside the certificate is left unspent, so its being spent
    // already says nothing.
    assert!(
        take_handed_ticket(&place, &key, &root, own, Some(&credentials))
            .await
            .is_none(),
        "a claimed machine leaves a handed ticket alone"
    );

    // A second person logs in on the same machine with a ticket of their own: the one device key
    // gets a second certificate, filed under them, and the first stays.
    let second = tilo_desktop_model::WorkspaceId::new();
    let (second_service, second_state, second_key, second_cert, _) =
        super::workspaces::ServiceWorkspaces::test_hosting_person(second).await;
    let second_tickets = Tickets::new(
        (second_key.clone(), second_cert.clone()),
        second_service.endpoint(),
        None,
        root_in(second_state.path().to_path_buf()),
        Some(FirstWorkspace::founded(Arc::clone(&second_service), second)),
        Some(Arc::clone(&second_service)),
        Place::at(
            second_state.path().join("issuer"),
            tilo_device_identity::IdentityPaths::under(second_state.path().join("issuer-keys")),
        ),
        second_service.copies().action_service(),
    );
    let second_router = serve(&second_service, second_service.copies().action_service());
    let second_printed = ask_device(&second_tickets, ISSUE, json!({"capability": "member"}))
        .await
        .expect("the second person's device issues a ticket");
    let machine_daemon = Tickets::new(
        (key.clone(), device.cert.clone()),
        machine_endpoint.clone(),
        None,
        Arc::new(|| Ok(None)),
        None,
        None,
        place.clone(),
        machine_service.clone(),
    );
    ask_device(
        &machine_daemon,
        LOG_IN,
        json!({"ticket": second_printed["ticket"], "agent": "tilo"}),
    )
    .await
    .unwrap_or_else(|why| panic!("the second person's device certifies the machine: {why}"));
    let theirs = |cert: &tilo_user_identity::DeviceCert| {
        tilo_device_identity::persist::read_device_cert(
            &keys.device_cert_for_user(&cert.user_public_key),
        )
        .unwrap()
    };
    assert_eq!(
        theirs(&second_cert).device_public_key,
        key.verifying_key().to_bytes()
    );
    assert_eq!(
        theirs(&issuer_cert),
        device.cert,
        "the first person's certificate stays"
    );

    machine_endpoint.close().await;
    let _ = second_router.shutdown().await;
    let _ = router.shutdown().await;
}

/// A connection of this process's own account, as the daemon reads it.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn this_account(_: &std::os::unix::net::UnixStream) -> std::io::Result<crate::seats::Peer> {
    Ok(crate::seats::Peer {
        uid: rustix::process::geteuid().as_raw(),
        pid: None,
    })
}

/// The device key signs as the device for another program only through the daemon: a connection of
/// this device's own account or of its person is answered with a signature under the device's
/// public key, and a stranger's account, another person's key and a founding link are refused.
/// The request names no device: the daemon's own key is the only one it can sign with.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_device_key_signs_only_for_its_own_system_and_its_person() {
    use crate::seats::{Seat, Seated, Seats};
    use tilo_actions::remote::{
        ActionCallError, ActionProvider as _, Caller, InvokeRequest, ReceiptStatus,
    };
    use tilo_actions::{ActionAddress, InvocationId};
    use tilo_api::daemon::admission::{Admission, Admitted};
    use tilo_device_identity::login::{PROVIDER, RESOURCE, SIGN};

    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[0x2f; 32]);
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    let place = Place::at(dir.path().join("login"), keys);
    let own = rustix::process::geteuid().as_raw();
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(own, false)));
    let answered = crate::device::unclaimed::DeviceOnly::new(key.clone(), place);
    let seats = Arc::new(Seats::new(admission, answered.clone(), this_account));
    let message = b"the device signs this";
    let ask = |admitted: Admitted| {
        let seated = Seated::new(
            Seat {
                uid: own,
                pid: None,
                admitted,
                app: None,
                founding: false,
            },
            Arc::clone(&seats),
        );
        let request = InvokeRequest {
            asks: false,
            invocation: InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: ActionAddress::new(RESOURCE, SIGN),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(
                json!({"message": hex::encode(message)})
                    .as_object()
                    .cloned()
                    .expect("a sign asks with an object"),
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        answered
            .seated(seated)
            .invoke(&Caller::local_system(), request)
    };

    // This device's own account and its person: answered with the device's signature.
    for admitted in [Admitted::Device, Admitted::Person([0x31; 32])] {
        let receipt = ask(admitted)
            .await
            .expect("a call is answered")
            .expect("the device signs for its own system and its person");
        let ReceiptStatus::Completed { output } = receipt.status else {
            panic!("the device signed: {receipt:?}");
        };
        let signature = output
            .value
            .as_ref()
            .and_then(|value| value.get("signature"))
            .and_then(serde_json::Value::as_str)
            .expect("the answer carries a signature");
        let signature = ed25519_dalek::Signature::from_bytes(
            &<[u8; 64]>::try_from(hex::decode(signature).unwrap()).unwrap(),
        );
        key.verifying_key()
            .verify_strict(message, &signature)
            .expect("the signature is the device key's");
    }

    // Any other account, another person's key and a founding link: refused, and no signature.
    for admitted in [
        Admitted::Stranger,
        Admitted::Key {
            person: [0x32; 32],
            key: [0x33; 32],
        },
        Admitted::Founding { key: [0x34; 32] },
    ] {
        let refused = ask(admitted)
            .await
            .expect("a refusal is an answer")
            .expect_err("the device key is not theirs to sign with");
        assert!(
            matches!(refused, ActionCallError::Refused { .. }),
            "{admitted:?} must be refused: {refused:?}"
        );
    }
}
