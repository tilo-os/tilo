#![allow(clippy::unwrap_used, reason = "in a test these are assertions")]

mod support;

use neighbour_wire::NeighbourMsg;
use tilo_browser_peer::Host;
use tilo_media::realtime::negotiate;
use tilo_media::{CodecCapabilities, StreamCodec};
use tilo_moq::live::peer::MemberOps;
use tilo_relay_client::{AttributedPresence, Envelope};
use tilo_relay_protocol::legacy_window as wire;
use tilo_relay_session::RelayClient;
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{TestHost, enrol, ready, start_host, start_peer, workspace};

/// The next frame `ops` delivers, waited on as long as the caller's bound
/// allows. Seal frames and failed groups never reach it: [`MemberOps`]
/// skips them as the page does.
async fn next_frame(ops: &mut MemberOps) -> wire::Frame {
    loop {
        if let Some(frame) = ops.next(std::time::Duration::from_secs(1)).await {
            return wire::Frame::decode(&frame).unwrap();
        }
    }
}

/// The frame after the replayed `start`: a renewed subscription starts at
/// the latest group again, so its start frame can repeat.
async fn after_start(video: &mut MemberOps, start: &wire::Frame) -> wire::Frame {
    loop {
        let frame = next_frame(video).await;
        if frame != *start {
            return frame;
        }
    }
}

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

/// The evdev strokes a US keyboard types `text` with: Shift around capitals
/// and shifted punctuation.
fn typing(text: &str) -> Vec<wire::InputEvent> {
    const SHIFT: u32 = 42;
    let key = |keysym, pressed| wire::InputEvent::Key {
        keysym,
        pressed,
        modifiers: 0,
    };
    let mut events = Vec::new();
    for c in text.chars() {
        let (code, shift) = match c {
            ' ' => (57, false),
            ',' => (51, false),
            '.' => (52, false),
            '!' => (2, true),
            c => {
                const ROWS: [(&str, u32); 3] =
                    [("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)];
                let lower = c.to_ascii_lowercase();
                let code = ROWS
                    .iter()
                    .find_map(|(row, first)| {
                        row.find(lower)
                            .map(|at| first.checked_add(u32::try_from(at).unwrap()).unwrap())
                    })
                    .unwrap();
                (code, c.is_ascii_uppercase())
            }
        };
        if shift {
            events.push(key(SHIFT, true));
        }
        events.extend([key(code, true), key(code, false)]);
        if shift {
            events.push(key(SHIFT, false));
        }
    }
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_window_reaches_browser_and_only_granted_input_returns() {
    Box::pin(native_window_scenario()).await;
}

#[allow(
    clippy::too_many_lines,
    reason = "one production cross-carrier scenario"
)]
async fn native_window_scenario() {
    let (root, _) = UserIdentity::generate("Legacy viewer", 1);
    let host_device = enrol(&root, "host", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web } = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let http = support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());
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
            tab: 0,
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
            seq: 0,
        },
    );
    let path = tilo_moq::live::namespace_path(&grant.room, &hex::encode(author)).unwrap();
    let track = |record: &tilo_relay_protocol::live::LiveRecord| {
        format!("ops/{}", hex::encode(record.digest()))
    };
    let mut index = MemberOps::follow(&live, &path, &track(&wire::index_record()));
    let expected = wire::Stream::new(7, "org.chromium.Chromium", "h264", 640, 480);
    tilo_test_env::within("the stream index", async {
        while next_frame(&mut index).await != wire::Frame::Index(vec![expected.clone()]) {}
    })
    .await;
    // No subsequent native frame is needed: a new browser receives the cached keyframe.
    let mut video = MemberOps::follow(&live, &path, &track(&wire::stream_record(7)));
    let (start, chunk) = tilo_test_env::within("the replayed video group", async {
        let start = next_frame(&mut video).await;
        let chunk = after_start(&mut video, &start).await;
        (start, chunk)
    })
    .await;
    assert_eq!(start, wire::Frame::Start(expected));
    assert!(matches!(chunk, wire::Frame::Chunk { flags, data, .. }
        if flags == wire::CHUNK_KEY | wire::CHUNK_FIRST | wire::CHUNK_LAST && data == encoded));
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
    // A full batch of typing reaches the session whole and in order, the
    // doubled letters and every release included.
    let typed = typing("Hello, World! example.com");
    assert!(typed.len() > wire::MAX_INPUT_EVENTS / 2);
    for batch in typed.chunks(wire::MAX_INPUT_EVENTS) {
        request.events = batch.to_vec();
        assert!(input(&browser, request.clone()).await.unwrap());
    }
    let mut received = Vec::new();
    tilo_test_env::within("every typed stroke at the session", async {
        while received.len() < typed.len() {
            if let Some(Envelope::Presence { payload, .. }) = native.recv().await
                && let Ok(NeighbourMsg::RemoteInput { event, .. }) =
                    postcard::from_bytes(&AttributedPresence::decode(&payload).unwrap().body)
            {
                received.push(postcard::from_bytes::<wire::InputEvent>(&event).unwrap());
            }
        }
    })
    .await;
    assert_eq!(received, typed);
    publish(
        &native,
        author,
        &NeighbourMsg::PermissionGrant {
            peer: tilo_peer_state::peer_handle(&host_device.cert.device_public_key),
            capability: "workspace-control:7:revoked".into(),
        },
    );
    tilo_test_env::within("the revoked stream index", async {
        while next_frame(&mut index).await != wire::Frame::Index(Vec::new()) {}
    })
    .await;
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

/// The next codec advert the host's device sends `native`, parsed as a
/// session's `media_caps::record` parses it.
async fn host_advert(native: &RelayClient, host: [u8; 32]) -> CodecCapabilities {
    tilo_test_env::within("the host's codec advert", async {
        loop {
            let Some(Envelope::Presence { payload, .. }) = native.recv().await else {
                continue;
            };
            let attributed = AttributedPresence::decode(&payload).unwrap();
            if let Ok(NeighbourMsg::MediaCapabilities {
                peer,
                hardware_decode,
                software_decode,
                hardware_encode,
                software_encode,
            }) = postcard::from_bytes(&attributed.body)
            {
                assert_eq!(attributed.author, host);
                assert_eq!(peer, tilo_peer_state::peer_handle(&host));
                return CodecCapabilities {
                    hardware_decode: CodecCapabilities::parse_labels(&hardware_decode),
                    software_decode: CodecCapabilities::parse_labels(&software_decode),
                    hardware_encode: CodecCapabilities::parse_labels(&hardware_encode),
                    software_encode: CodecCapabilities::parse_labels(&software_encode),
                    hardware_decode_max: std::collections::BTreeMap::new(),
                };
            }
        }
    })
    .await
}

/// A stock session has no H.264 encoder (no hardware one, no codec pack),
/// only software VP9. It shares a window with the host for a browser
/// member: while the member's page has not said what it decodes the host
/// claims H.264 only and the session has nothing to send; once the page
/// says it decodes VP9, the session streams VP9 and the page receives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one production cross-carrier scenario"
)]
async fn a_session_without_an_h264_encoder_streams_vp9_to_a_browser_that_decodes_it() {
    let (root, _) = UserIdentity::generate("Legacy viewer", 1);
    let host_device = enrol(&root, "host", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web } = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let http = support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());
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
    let host = host_device.cert.device_public_key;
    let stock_session = CodecCapabilities {
        software_encode: [StreamCodec::Vp9].into(),
        software_decode: [StreamCodec::Vp9].into(),
        ..CodecCapabilities::default()
    };
    let with_codec_pack = CodecCapabilities {
        software_encode: [StreamCodec::H264, StreamCodec::Vp9].into(),
        ..stock_session.clone()
    };
    // The member is in the live plane but has not said: H.264 only, which
    // the stock session cannot encode.
    let advert = host_advert(&native, host).await;
    assert_eq!(
        advert.software_decode,
        [StreamCodec::H264].into(),
        "a page that never said counts as H.264 only"
    );
    assert_eq!(negotiate(&stock_session, &[advert]), None);

    let (tx, rx) = tokio::sync::oneshot::channel();
    browser
        .legacy_codecs_then(
            wire::CodecsRequest {
                workspace_id: workspace(),
                decode: vec!["h264".into(), "vp9".into(), "av1".into()],
            },
            Box::new(move |result| {
                let _ = tx.send(result);
            }),
        )
        .unwrap();
    assert!(
        tilo_test_env::within("the codec advert receipt", rx)
            .await
            .unwrap()
            .unwrap()
    );
    let advert = tilo_test_env::within("the advert naming VP9", async {
        loop {
            let advert = host_advert(&native, host).await;
            if advert.software_decode.contains(&StreamCodec::Vp9) {
                return advert;
            }
        }
    })
    .await;
    assert_eq!(
        advert.software_decode,
        [StreamCodec::H264, StreamCodec::Vp9, StreamCodec::Av1].into()
    );
    assert!(
        advert.hardware_decode.is_empty(),
        "a browser's hardware is unknown"
    );
    assert_eq!(
        negotiate(&with_codec_pack, std::slice::from_ref(&advert)).map(|n| n.codec),
        Some(StreamCodec::H264),
        "H.264 stays the choice when the session can encode it"
    );
    let chosen = negotiate(&stock_session, &[advert]).unwrap().codec;
    assert_eq!(chosen, StreamCodec::Vp9);

    let url = format!("https://127.0.0.1:{}/moq/live", web.addr.port())
        .parse()
        .unwrap();
    let trust =
        tilo_moq::carrier::Trust::Fingerprints(vec![web.certificate_sha256.clone().unwrap()]);
    let live = tilo_moq::live::peer::Peer::connect(&url, &trust, &grant.token, None)
        .await
        .unwrap();
    let author = laptop.cert.device_public_key;
    publish(
        &native,
        author,
        &NeighbourMsg::PermissionGrant {
            peer: tilo_peer_state::peer_handle(&host),
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
            codec: chosen.wire_label().into(),
            ts: 1,
            sender_monotonic_ns_at_send: 0,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
            app_id: "org.chromium.Chromium".into(),
            tab: 0,
        },
    );
    // The first bytes of a libvpx key frame.
    let key_frame = [0x82, 0x49, 0x83, 0x42, 0x00, 0x00, 0xf0, 0x00];
    publish(
        &native,
        author,
        &NeighbourMsg::SurfaceFrame {
            surface: 7,
            source_peer: tilo_peer_state::peer_handle(&author),
            encoded_h264: key_frame.to_vec(),
            ts: 5000,
            seq: 0,
        },
    );
    let path = tilo_moq::live::namespace_path(&grant.room, &hex::encode(author)).unwrap();
    let track = |record: &tilo_relay_protocol::live::LiveRecord| {
        format!("ops/{}", hex::encode(record.digest()))
    };
    let mut index = MemberOps::follow(&live, &path, &track(&wire::index_record()));
    let expected = wire::Stream::new(7, "org.chromium.Chromium", wire::VP9, 640, 480);
    tilo_test_env::within("the VP9 stream index", async {
        while next_frame(&mut index).await != wire::Frame::Index(vec![expected.clone()]) {}
    })
    .await;
    let mut video = MemberOps::follow(&live, &path, &track(&wire::stream_record(7)));
    let (start, chunk) = tilo_test_env::within("the replayed VP9 group", async {
        let start = next_frame(&mut video).await;
        let chunk = after_start(&mut video, &start).await;
        (start, chunk)
    })
    .await;
    assert_eq!(start, wire::Frame::Start(expected));
    assert!(matches!(chunk, wire::Frame::Chunk { flags, data, .. }
        if flags == wire::CHUNK_KEY | wire::CHUNK_FIRST | wire::CHUNK_LAST && data == key_frame));
    drop(live);
    browser.close();
    native.close().await;
    drop(web);
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cached_static_windows_survive_concurrent_viewer_handoffs() {
    let mut scenarios = tokio::task::JoinSet::new();
    for _ in 0..4 {
        scenarios.spawn(async {
            for _ in 0..8 {
                Box::pin(native_window_scenario()).await;
            }
        });
    }
    while let Some(result) = scenarios.join_next().await {
        result.expect("every original cross-carrier cached-window scenario completes");
    }
}
