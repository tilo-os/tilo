//! A device of a workspace a third device hosts is reached, as an answer to an ask reaches it,
//! with its link to the host up or not; where nothing reaches it, the refusal names it.

use std::time::{Duration, Instant};

use tilo_actions::ActionAddress;
use tilo_actions::remote::asked::RESOLVE;
use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, InvokeRequest};
use tilo_actions::remote::{RemoteActionTarget, WORKSPACE_INVOCATIONS_PROVIDER as INVOCATIONS};
use tilo_peer_state::actions::{ACTIONS_ALPN, offline};
use tilo_remote_actions::asked::NOTHING_ASKED;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::device::hosts::Hosts;
use crate::device::record_actions_tests::copy_of;
use crate::device::ticket_tests::{enrol, login_to, studio};
use crate::device::workspaces::ServiceWorkspaces;

/// Why `phone` refused the laptop's answer to an unasked record, sent through the laptop's hosts:
/// answered in under two seconds, whatever reaches it.
async fn answered(laptop: &ServiceWorkspaces, workspace: &str, phone: [u8; 32]) -> ActionCallError {
    let cert = laptop.device_cert();
    let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(workspace.to_owned());
    let answer = serde_json::Map::from_iter([("resolution".to_owned(), "approve".into())]);
    let target = RemoteActionTarget {
        provider: INVOCATIONS.to_owned(),
        address: ActionAddress::new("unasked", RESOLVE),
    };
    let providers = laptop.copies().providers();
    let phone = providers.device(workspace, phone).unwrap();
    let (request, asked) = (InvokeRequest::plain(target, answer), Instant::now());
    let resolve = ActionProvider::invoke(&phone, &caller, request);
    let answered = tilo_test_env::within("the laptop's answer", resolve).await;
    let took = asked.elapsed();
    assert!(took < Duration::from_secs(2), "{took:?}");
    match answered {
        Some(Err(error)) => error,
        other => panic!("the answer is refused: {other:?}"),
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
    let nothing_asked = ActionCallError::Refused {
        reason: NOTHING_ASKED.into(),
    };
    assert!(host.reached().contains(&device), "the phone's link is up");
    assert_eq!(answered(&laptop, &s.workspace, device).await, nothing_asked);
    host.forget_device(device, &host.reach(device).unwrap());
    assert_eq!(answered(&laptop, &s.workspace, device).await, nothing_asked);

    // Offline, with no link to the host and no address in use, it is said so at once.
    answering.shutdown().await.unwrap();
    phone.shutdown().await;
    let offline_now = answered(&laptop, &s.workspace, device).await;
    assert_eq!(offline_now, offline(device));
}
