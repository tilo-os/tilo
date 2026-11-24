//! A request another device addresses to this one through the workspace's host is
//! served here, and the answer goes back to the device that asked: a window this
//! session holds is read and acted on that way, and nobody stands in front of it to
//! answer instead.

use std::sync::{Arc, Mutex};

use tilo_actions::remote::{CataloguePage, CatalogueQuery, RemoteActionFrame, RemoteActionMessage};
use tilo_server_protocol::WorkspaceId;
use tilo_session_actions::Heard;
use tilo_session_client::{Cmd, Event, InboundActions, Outbox};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    Device, WORKSPACE, admitted_to, enrol, place, ready, start_host, start_peer, wait_for,
    workspace,
};

/// What the device this test asks answers with: an empty catalogue page, and who
/// asked it, kept for the assertion.
struct Serves {
    /// This device's own outbox: where the answer leaves, on the session it came in on.
    outbox: Arc<dyn Outbox + Send + Sync>,
    asked: Asked,
}

/// Who asked, kept for the assertion: the workspace, the person it was certified
/// for, and the device itself.
type Asked = Mutex<Vec<(WorkspaceId, [u8; 32], [u8; 32])>>;

impl InboundActions for Serves {
    fn take(&self, workspace: WorkspaceId, user: [u8; 32], from: [u8; 32], frame: Vec<u8>) {
        self.asked.lock().unwrap().push((workspace, user, from));
        let Ok(request) = RemoteActionFrame::decode(&frame) else {
            return;
        };
        let RemoteActionMessage::CatalogueRequest { correlation, .. } = request.message else {
            return;
        };
        // The answer is this device's own, to the device that asked: what the
        // session's outbox says on this connection.
        let answer = RemoteActionFrame::new(RemoteActionMessage::CatalogueResponse {
            correlation,
            result: Ok(CataloguePage {
                revision: "window".into(),
                ..CataloguePage::default()
            }),
        })
        .encode()
        .expect("the answer encodes");
        let _ = self.outbox.send(Cmd::RemoteAction(workspace, from, answer));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_addressed_to_this_device_is_served_here_and_answered_to_the_asker() {
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

    // The window's device serves what another device addresses to it.
    let serves = Arc::new(Serves {
        outbox: Arc::clone(window_peer.outbox()),
        asked: Mutex::default(),
    });
    window_peer.serve_inbound(Arc::clone(&serves) as Arc<dyn InboundActions>);

    let heard = caller_peer.events();
    let frame = RemoteActionFrame::new(RemoteActionMessage::CatalogueRequest {
        correlation: 11,
        query: CatalogueQuery::default(),
    })
    .encode()
    .expect("the request encodes");
    caller_peer
        .send(Cmd::RemoteAction(
            workspace(),
            window.cert.device_public_key,
            frame,
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
    let asked = serves.asked.lock().unwrap();
    let (in_workspace, _, from) = asked.first().expect("the window's device served it");
    assert_eq!(*in_workspace, workspace());
    assert_eq!(
        *from, caller.cert.device_public_key,
        "as the device that asked"
    );
}
