//! A device of a workspace a third device hosts is reached, as an answer to an ask reaches it,
//! with its link to the host up or not; where nothing reaches it, the refusal names it.

use tilo_actions::ActionAddress;
use tilo_actions::remote::asked::RESOLVE;
use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, InvokeRequest};
use tilo_actions::remote::{RemoteActionTarget, WORKSPACE_INVOCATIONS_PROVIDER as INVOCATIONS};
use tilo_peer_state::actions::ACTIONS_ALPN;
use tilo_remote_actions::asked::NOTHING_ASKED;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::device::hosts::Hosts;
use crate::device::record_actions_tests::copy_of;
use crate::device::ticket_tests::{enrol, login_to, studio};
use crate::device::workspaces::ServiceWorkspaces;

/// Why `phone` refused the laptop's answer to an unasked record, sent through the laptop's hosts.
async fn answered(laptop: &ServiceWorkspaces, workspace: &str, phone: [u8; 32]) -> String {
    let cert = laptop.device_cert();
    let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(workspace.to_owned());
    let answer = serde_json::Map::from_iter([("resolution".to_owned(), "approve".into())]);
    let target = RemoteActionTarget {
        provider: INVOCATIONS.to_owned(),
        address: ActionAddress::new("unasked", RESOLVE),
    };
    let phone = laptop.copies().providers().device(workspace, phone);
    let resolve = ActionProvider::invoke(
        &phone.unwrap(),
        &caller,
        InvokeRequest::plain(target, answer),
    );
    match tilo_test_env::within("the laptop's answer", resolve).await {
        Some(Err(
            ActionCallError::Refused { reason } | ActionCallError::Unavailable { reason },
        )) => reason,
        other => panic!("the answer is refused with a reason: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_reaches_another_of_a_workspace_a_third_device_hosts_with_or_without_its_link() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let s = studio(&root, state.path()).await;
    let (key, cert) = enrol(&root, "phone", DeviceCapabilities::member());
    s.hosted.enter(&s.workspace, (&cert, &key)).await.unwrap();
    let login = login_to(&s.server, &s.workspace, (&cert, &s.server_cert));
    let phone = copy_of(&state.path().join("phone"), (&key, &cert), &login).await;
    let laptop_dir = state.path().join("laptop");
    let laptop = copy_of(&laptop_dir, (&s.laptop_key, &s.laptop_cert), &s.login).await;
    let _hosts = Hosts::placed(&laptop);
    // The phone answers at its own endpoint, where the laptop finds it as discovery would.
    let answering = iroh::protocol::Router::builder(phone.endpoint());
    let answering = answering
        .accept(ACTIONS_ALPN, phone.copies().action_service())
        .spawn();
    let found = iroh::address_lookup::MemoryLookup::new();
    found.add_endpoint_info(phone.endpoint().addr());
    laptop.endpoint().address_lookup().unwrap().add(found);
    let host = s.server.workspaces().unwrap().action_service();
    let device = cert.device_public_key;

    // The phone answers itself, keeping nothing under the record: its link to the host up, and
    // once the host let it go.
    assert!(host.reached().contains(&device), "the phone's link is up");
    assert_eq!(answered(&laptop, &s.workspace, device).await, NOTHING_ASKED);
    host.forget_device(device, &host.reach(device).unwrap());
    let asked = std::time::Instant::now();
    assert_eq!(answered(&laptop, &s.workspace, device).await, NOTHING_ASKED);
    assert!(
        asked.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        asked.elapsed()
    );

    // Offline, nothing reaches it, and the refusal names it.
    answering.shutdown().await.unwrap();
    phone.shutdown().await;
    let refused = answered(&laptop, &s.workspace, device).await;
    assert!(refused.contains(&hex::encode(&device[..8])), "{refused}");
}
