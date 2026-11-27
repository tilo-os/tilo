//! A request another device addresses to this one through the workspace's host is
//! answered here, by this device's own action service, and the answer goes back to
//! the device that asked: a device that dialled its host, behind a NAT or on a phone,
//! is reached through that host. The host passes the request on as it was signed and
//! judges nothing of it; this device decides it on that signature, so a caller the
//! workspace gives nothing is refused here.

use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, CataloguePage, CatalogueQuery, RemoteActionFrame, RemoteActionMessage,
    TestProvider,
};
use tilo_peer_state::actions::ActionService;
use tilo_peer_state::direct::Replica;
use tilo_server_client::link::{Dialer, Purpose};
use tilo_session_actions::Heard;
use tilo_session_client::{Cmd, Event, InboundActions, Outbox as _};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    Device, WORKSPACE, admitted_to, enrol, place, ready, start_host, start_peer, wait_for,
    workspace,
};

fn catalogue(correlation: u64) -> Vec<u8> {
    RemoteActionFrame::new(RemoteActionMessage::CatalogueRequest {
        correlation,
        query: CatalogueQuery::default(),
    })
    .encode()
    .expect("the request encodes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_through_the_host_is_decided_and_answered_by_the_device_it_names() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let caller = enrol(&root, "visa agent", DeviceCapabilities::member());
    let window = enrol(&root, "Browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    place(&host.server, (&window.cert, &window.key)).await;
    let start = |device: &Device| {
        start_peer(
            host.iroh(),
            device.cert.clone(),
            host_device.cert.clone(),
            device.key.clone(),
        )
    };
    let (caller_peer, window_peer) = (start(&caller), start(&window));
    for peer in [&caller_peer, &window_peer] {
        peer.join_placed(WORKSPACE, "Workspace").unwrap();
    }
    ready(&[&caller_peer, &window_peer]).await;
    admitted_to(&caller_peer, workspace()).await;
    admitted_to(&window_peer, workspace()).await;

    // The window's device answers with its own action service, over its own copy of the
    // workspace, in which the caller is one of Ada's devices.
    let asked: Arc<Mutex<Vec<Option<[u8; 32]>>>> = Arc::default();
    let listed = Arc::clone(&asked);
    let dispatcher = TestProvider::new().with_catalogue(move |caller, _| {
        listed
            .lock()
            .unwrap()
            .push(caller.device_public_key().copied());
        Ok(CataloguePage {
            revision: "window".into(),
            ..CataloguePage::default()
        })
    });
    let service = ActionService::new(window.key.clone(), Arc::new(dispatcher));
    service.hold(Replica::new(
        tilo_peer_state::fixture::Founding::by(&(window.key.clone(), window.cert.clone()))
            .named(WORKSPACE)
            .with([&caller.cert])
            .at(tilo_clock::wall())
            .exact(),
    ));
    window_peer.serve_inbound(Arc::new(service) as Arc<dyn InboundActions>);

    let heard = caller_peer.events();
    caller_peer
        .send(Cmd::RemoteAction(
            workspace(),
            window.cert.device_public_key,
            catalogue(11),
        ))
        .unwrap();
    let answer = wait_for("the window's answer at the device that asked", || {
        let answered = heard.drain().into_iter().find_map(|event| match event {
            Event::Remote(Heard::Action {
                workspace_id,
                from,
                frame,
                ..
            }) if workspace_id == workspace() && from == window.cert.device_public_key => {
                Some(RemoteActionFrame::decode(&frame).ok()?.message)
            }
            Event::Remote(Heard::Undeliverable { refusal, .. }) => {
                panic!("the request did not reach the window: {refusal:?}")
            }
            _ => None,
        });
        async move { answered }
    })
    .await;
    let RemoteActionMessage::CatalogueResponse {
        correlation,
        result,
    } = answer
    else {
        panic!("the window's device answers with its listing: {answer:?}");
    };
    assert_eq!(correlation, 11);
    assert_eq!(
        result
            .expect("the window answered its own listing")
            .revision,
        "window"
    );
    assert_eq!(
        *asked.lock().unwrap(),
        [Some(caller.cert.device_public_key)],
        "the window's dispatcher served it as the device that signed it, never as the host"
    );

    // A device of someone the workspace gives nothing asks the window the same way, through
    // the host: the host passes it on, and the window refuses it on its signature.
    let (stranger_root, _) = UserIdentity::generate("Mallory", 1);
    let stranger = enrol(&stranger_root, "laptop", DeviceCapabilities::member());
    let dialer = Dialer::new(host.iroh(), Some(stranger.key.to_bytes()));
    let link = dialer
        .link(Purpose::Actions)
        .await
        .expect("the host's action service answers anyone");
    // The stranger's device answers on its link as any device does: the host asks a key it
    // knows nothing of which certificate it answers under, and it names none.
    ActionService::new(
        stranger.key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    )
    .serve_link(Arc::new(link.clone()));
    let asked_at = std::time::Instant::now();
    let answer = tilo_session_client::session::ask_action(
        &link,
        Arc::new(stranger.key.clone()),
        stranger.cert.device_public_key,
        &workspace().to_string(),
        window.cert.device_public_key,
        RemoteActionFrame::decode(&catalogue(12)).unwrap().message,
    )
    .await
    .expect("the host carried the window's answer back");
    let answer = RemoteActionFrame::decode(&answer).unwrap().message;
    assert_eq!(
        answer,
        RemoteActionMessage::CatalogueResponse {
            correlation: 12,
            result: Err(ActionCallError::NotPermitted),
        },
        "the window refuses a caller its workspace gives nothing"
    );
    assert!(
        asked_at.elapsed() < tilo_peer_state::actions::REPLY_DEADLINE,
        "refused at once, not after a deadline"
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "nothing of the stranger's reached the window's dispatcher"
    );
    dialer.close().await;
}

/// A device that serves nothing passed on through its host refuses such a request at once,
/// naming itself: its caller is told so, never left waiting out the host's deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_serves_nothing_through_its_host_refuses_at_once() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let caller = enrol(&root, "visa agent", DeviceCapabilities::member());
    let window = enrol(&root, "Browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    place(&host.server, (&window.cert, &window.key)).await;
    let start = |device: &Device| {
        start_peer(
            host.iroh(),
            device.cert.clone(),
            host_device.cert.clone(),
            device.key.clone(),
        )
    };
    let (caller_peer, window_peer) = (start(&caller), start(&window));
    for peer in [&caller_peer, &window_peer] {
        peer.join_placed(WORKSPACE, "Workspace").unwrap();
    }
    ready(&[&caller_peer, &window_peer]).await;
    admitted_to(&caller_peer, workspace()).await;
    admitted_to(&window_peer, workspace()).await;

    let heard = caller_peer.events();
    let asked = std::time::Instant::now();
    caller_peer
        .send(Cmd::RemoteAction(
            workspace(),
            window.cert.device_public_key,
            catalogue(13),
        ))
        .unwrap();
    let answer = wait_for("the window's refusal at the device that asked", || {
        let answered = heard.drain().into_iter().find_map(|event| match event {
            Event::Remote(Heard::Action { from, frame, .. })
                if from == window.cert.device_public_key =>
            {
                Some(RemoteActionFrame::decode(&frame).ok()?.message)
            }
            _ => None,
        });
        async move { answered }
    })
    .await;
    let refusal = neighbour_wire::actions::unpassed(
        window.cert.device_public_key,
        neighbour_wire::actions::ANSWERS_NONE_PASSED,
    );
    assert_eq!(
        answer,
        RemoteActionMessage::CatalogueResponse {
            correlation: 13,
            result: Err(ActionCallError::Refused { reason: refusal }),
        }
    );
    assert!(
        asked.elapsed() < tilo_peer_state::actions::REPLY_DEADLINE,
        "refused at once, not after the host's deadline"
    );
}
