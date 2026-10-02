//! Native surface presence reaches the portable browser session over the
//! production HTTPS/WebTransport carriers, and browser input returns privately.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are assertions"
)]

mod support;

use neighbour_wire::NeighbourMsg;
use tilo_browser_peer::Host;
use tilo_relay_client::{AttributedPresence, Envelope, RelayClient};
use tilo_relay_protocol::legacy_window as wire;
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{enrol, ready, start_host, start_peer, workspace};

fn publish(client: &RelayClient, author: [u8; 32], message: &NeighbourMsg) {
    client
        .send(Envelope::Presence {
            workspace_id: workspace(),
            payload: AttributedPresence {
                author,
                body: postcard::to_allocvec(message).unwrap(),
            }
            .encode()
            .unwrap(),
        })
        .unwrap();
}

async fn input(
    peer: &tilo_browser_peer::PeerHandle,
    request: wire::InputRequest,
) -> Result<bool, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    peer.legacy_input_then(
        request,
        Box::new(move |result| {
            let _ = tx.send(result);
        }),
    )
    .unwrap();
    tilo_test_env::within("the HTTPS input receipt", rx)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one production cross-carrier scenario"
)]
async fn native_window_reaches_browser_and_only_granted_input_returns() {
    let (root, _) = UserIdentity::generate("Legacy viewer", 1);
    let host_device = enrol(&root, "host", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let (http, relay, web) = {
        let host = start_host(
            &root,
            &host_device,
            state.path(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await;
        let http = host.https();
        (http, host.relay, host.web)
    };
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&laptop.key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let native = RelayClient::connect_to_pinned_host(
        endpoint,
        relay.node_addr(),
        laptop.cert.clone(),
        laptop.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    native
        .send(Envelope::Subscribe {
            workspace_id: workspace(),
        })
        .unwrap();
    tilo_test_env::within("the native subscription", async {
        loop {
            if matches!(native.recv().await, Some(Envelope::Ready { .. })) {
                break;
            }
        }
    })
    .await;
    let browser = start_peer(
        Host::Https(http),
        browser_device.cert,
        host_device.cert.clone(),
        browser_device.key,
    );
    browser.join(workspace()).unwrap();
    ready(&[&browser]).await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    browser
        .live_grant_then(
            workspace(),
            Box::new(move |result| {
                let _ = tx.send(result);
            }),
        )
        .unwrap();
    let grant = tilo_test_env::within("the live grant", rx)
        .await
        .unwrap()
        .unwrap();
    let url = format!("https://127.0.0.1:{}/moq/live", web.addr.port())
        .parse()
        .unwrap();
    let trust =
        tilo_moq::carrier::Trust::Fingerprints(vec![web.certificate_sha256.clone().unwrap()]);
    let live = tilo_moq::live::peer::Peer::connect(&url, &trust, &grant.token, None)
        .await
        .unwrap();
    let mut request = wire::InputRequest {
        workspace_id: workspace(),
        publisher: laptop.cert.device_public_key,
        surface: 7,
        events: Vec::new(),
    };
    let refusal = input(&browser, request.clone()).await.unwrap_err();
    assert!(
        refusal == "host unavailable: status 404: no such window",
        "an ungranted window cannot be driven: {refusal}"
    );
    let author = laptop.cert.device_public_key;
    publish(
        &native,
        author,
        &NeighbourMsg::PermissionGrant {
            peer: tilo_peer_state::peer_handle(&host_device.cert.device_public_key),
            capability: "workspace-control:7:granted".into(),
        },
    );
    publish(
        &native,
        author,
        &NeighbourMsg::SurfaceStreamStart {
            surface: 7,
            source_peer: tilo_peer_state::peer_handle(&author),
            width: 640,
            height: 480,
            codec: "h264".into(),
            ts: 1,
            sender_monotonic_ns_at_send: 0,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
            app_id: "org.chromium.Chromium".into(),
        },
    );
    let encoded = [0, 0, 0, 1, 0x65, 0x88, 0x84];
    publish(
        &native,
        author,
        &NeighbourMsg::SurfaceFrame {
            surface: 7,
            source_peer: tilo_peer_state::peer_handle(&author),
            encoded_h264: encoded.to_vec(),
            ts: 5000,
        },
    );
    let path = tilo_moq::live::namespace_path(&grant.room, &hex::encode(author)).unwrap();
    let namespace = tilo_test_env::within("the native stream namespace", live.member(&path))
        .await
        .unwrap();
    let track = |record: &tilo_relay_protocol::live::LiveRecord| {
        format!("ops/{}", hex::encode(record.digest()))
    };
    let mut index = namespace
        .track(&track(&wire::index_record()))
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let expected = wire::Stream::new(7, "org.chromium.Chromium", "h264", 640, 480);
    tilo_test_env::within("the stream index", async {
        loop {
            if let Some(mut group) = index.recv_group().await.unwrap()
                && let Some(frame) = group.read_frame().await.unwrap()
                && wire::Frame::decode(&frame.payload).unwrap()
                    == wire::Frame::Index(vec![expected.clone()])
            {
                break;
            }
        }
    })
    .await;
    // No subsequent native frame is needed: a new browser receives the cached keyframe.
    let mut video = namespace
        .track(&track(&wire::stream_record(7)))
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let mut group = tilo_test_env::within("the replayed video group", video.recv_group())
        .await
        .unwrap()
        .unwrap();
    let start = group.read_frame().await.unwrap().unwrap();
    assert_eq!(
        wire::Frame::decode(&start.payload).unwrap(),
        wire::Frame::Start(expected)
    );
    let chunk = group.read_frame().await.unwrap().unwrap();
    assert!(
        matches!(wire::Frame::decode(&chunk.payload).unwrap(), wire::Frame::Chunk { flags, data, .. }
        if flags == wire::CHUNK_KEY | wire::CHUNK_FIRST | wire::CHUNK_LAST && data == encoded)
    );
    request.events = vec![wire::InputEvent::PointerEnter { surface: 999 }];
    assert!(input(&browser, request.clone()).await.unwrap());
    tilo_test_env::within("private native input", async {
        loop {
            if let Some(Envelope::Presence { payload, .. }) = native.recv().await {
                let attributed = AttributedPresence::decode(&payload).unwrap();
                if let Ok(NeighbourMsg::RemoteInput {
                    peer,
                    surface,
                    event,
                }) = postcard::from_bytes(&attributed.body)
                {
                    assert_eq!(attributed.author, host_device.cert.device_public_key);
                    assert_eq!(
                        peer,
                        tilo_peer_state::peer_handle(&host_device.cert.device_public_key)
                    );
                    assert_eq!(surface, 7);
                    assert_eq!(
                        postcard::from_bytes::<wire::InputEvent>(&event).unwrap(),
                        wire::InputEvent::PointerEnter { surface: 7 }
                    );
                    break;
                }
            }
        }
    })
    .await;
    publish(
        &native,
        author,
        &NeighbourMsg::PermissionGrant {
            peer: tilo_peer_state::peer_handle(&host_device.cert.device_public_key),
            capability: "workspace-control:7:revoked".into(),
        },
    );
    tilo_test_env::within("the revoked stream index", async {
        loop {
            if let Some(mut group) = index.recv_group().await.unwrap()
                && let Some(frame) = group.read_frame().await.unwrap()
                && matches!(wire::Frame::decode(&frame.payload), Ok(wire::Frame::Index(streams)) if streams.is_empty()) {
                    break;
                }
        }
    }).await;
    let refusal = input(&browser, request).await.unwrap_err();
    assert!(
        refusal == "host unavailable: status 404: no such window",
        "revoked input is refused by the production carrier: {refusal}"
    );
    drop(live);
    browser.close();
    native.close().await;
    drop(web);
    relay.shutdown().await;
}
