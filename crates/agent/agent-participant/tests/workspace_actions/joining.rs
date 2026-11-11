//! A page whose first request goes out as it joins. A host takes no action
//! from a session it has not subscribed to the workspace yet, so a frame sent
//! in the moment between asking to join and being admitted was refused and
//! never answered. The session holds what it owes a workspace it joined until
//! that workspace admits it, and the request is answered then.
use super::*;
use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
use tilo_session_core::remote::Heard;
use tilo_session_host::PeerEvent;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_sent_as_the_page_joins_is_answered_once_it_is_admitted() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _participant = participant::Participant::start(&hosted.relay, &root).await;

    let peer = start_peer(
        Host::at(hosted.relay.node_addr()),
        device.cert.clone(),
        host.cert.clone(),
        device.key.clone(),
    );
    let events = peer.events();
    let workspace = tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap();

    // The page joins and asks the workspace for its catalogue at once, as a
    // page does when it opens on the workspace's records: the join is a round
    // trip to the host, and the request is made before its answer.
    peer.join_placed(WORKSPACE, "Workspace").unwrap();
    let asked = RemoteActionFrame::new(RemoteActionMessage::CatalogueRequest {
        correlation: 1,
        query: CatalogueQuery::default(),
    })
    .encode()
    .unwrap();
    peer.send_remote_action(workspace, host.cert.device_public_key, asked)
        .unwrap();

    // The host answers the request once it admits the page: a request refused
    // in the meantime is one the page never hears of again.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        for event in events.drain() {
            let PeerEvent::Remote(heard) = event else {
                continue;
            };
            match heard {
                Heard::Action { from, frame, .. } => {
                    assert_eq!(from, host.cert.device_public_key);
                    assert!(
                        matches!(
                            RemoteActionFrame::decode(&frame).unwrap().message,
                            RemoteActionMessage::CatalogueResponse { .. }
                        ),
                        "the host answered something else"
                    );
                    return;
                }
                Heard::Undeliverable { reason, .. } => {
                    panic!("the host refused a request the page made as it joined: {reason}")
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "the host never answered the page's first request"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
