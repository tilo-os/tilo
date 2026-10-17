//! A page a session streams as a display list (`tilo-paint`) reaches a browser
//! member through the production relay and live carriers, is rebuilt by the
//! member's `RemotePage` exactly as the session sent it, and the member's
//! input on it returns to the session alone.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are assertions; the scenario reads top to bottom"
)]

mod support;

use neighbour_wire::NeighbourMsg;
use tilo_browser_peer::Host;
use tilo_browser_peer::remote_page::RemotePage;
use tilo_paint_stream::wire::Payload;
use tilo_paint_stream::{BlobRef, Color, Frame, Item, ItemKey, Layer, Point, ROOT, Rect};
use tilo_relay_client::{AttributedPresence, Envelope};
use tilo_relay_protocol::legacy_window as wire;
use tilo_relay_session::RelayClient;
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{TestHost, enrol, ready, start_host, start_peer, workspace};

const SURFACE: u64 = 9;

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

/// A page: a coloured box, a heading's text and an image, scrolled by `scroll`.
fn page(seq: u64, scroll: f32, image: [u8; 32]) -> Frame {
    let mut frame = Frame {
        seq,
        viewport: Point { x: 800.0, y: 600.0 },
        background: Color([255, 255, 255, 255]),
        ..Frame::default()
    };
    frame.layers.insert(
        ROOT,
        Layer {
            size: Point { x: 800.0, y: 600.0 },
            clips: true,
            scroll: Point { x: 0.0, y: scroll },
            opacity: 1.0,
            items: vec![
                Item::Fill {
                    key: ItemKey { node: 1, part: 0 },
                    rect: Rect::new(0.0, 0.0, 800.0, 48.0),
                    color: Color([255, 102, 0, 255]),
                    radius: 0.0,
                },
                Item::Text {
                    key: ItemKey { node: 2, part: 16 },
                    rect: Rect::new(16.0, 15.0, 120.0, 18.0),
                    text: "Streamed page".into(),
                    font: tilo_paint_stream::FontSpec {
                        family: "sans-serif".into(),
                        size: 16.0,
                        weight: 400,
                        italic: false,
                    },
                    color: Color([255, 255, 255, 255]),
                },
                Item::Image {
                    key: ItemKey { node: 3, part: 2 },
                    rect: Rect::new(16.0, 64.0, 32.0, 32.0),
                    blob: BlobRef::Hash(image),
                },
            ],
            ..Layer::default()
        },
    );
    frame
}

/// The next encoded frame of a stream group, reassembled from its chunks.
macro_rules! next_payload {
    ($group:expr) => {{
        let mut encoded = Vec::new();
        loop {
            let frame = tilo_test_env::within("a stream frame", $group.read_frame())
                .await
                .unwrap()
                .unwrap();
            match wire::Frame::decode(&frame.payload).unwrap() {
                wire::Frame::Start(stream) => assert_eq!(stream.codec, wire::PAINT),
                wire::Frame::Chunk { flags, data, .. } => {
                    if flags & wire::CHUNK_FIRST != 0 {
                        encoded.clear();
                    }
                    encoded.extend_from_slice(&data);
                    if flags & wire::CHUNK_LAST != 0 {
                        break encoded;
                    }
                }
                other => panic!("unexpected frame in a paint stream: {other:?}"),
            }
        }
    }};
}

#[tokio::test]
async fn a_streamed_page_is_rebuilt_by_the_member_and_its_input_returns() {
    let (root, _) = UserIdentity::generate("Paint viewer", 1);
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

    // The session shares its browser window as a paint stream, and sends a
    // whole page and its image before any member watches.
    let author = laptop.cert.device_public_key;
    let source_peer = tilo_peer_state::peer_handle(&author);
    publish(
        &native,
        author,
        &NeighbourMsg::PermissionGrant {
            peer: tilo_peer_state::peer_handle(&host_device.cert.device_public_key),
            capability: format!("workspace-control:{SURFACE}:granted"),
        },
    );
    publish(
        &native,
        author,
        &NeighbourMsg::SurfaceStreamStart {
            surface: SURFACE,
            source_peer,
            width: 800,
            height: 600,
            codec: wire::PAINT.into(),
            ts: 1,
            sender_monotonic_ns_at_send: 0,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
            app_id: "so.tilo.browser".into(),
            tab: 0,
        },
    );
    let image = b"\x89PNG not really, but bytes".to_vec();
    let hash = *blake3::hash(&image).as_bytes();
    let first = page(1, 0.0, hash);
    let frame = |payload: &Payload, ts: u64| NeighbourMsg::SurfaceFrame {
        surface: SURFACE,
        source_peer,
        encoded_h264: payload.encode().unwrap(),
        ts,
        seq: ts,
    };
    publish(&native, author, &frame(&Payload::Key(first.clone()), 2));
    publish(
        &native,
        author,
        &frame(
            &Payload::Blob {
                hash,
                bytes: image.clone(),
            },
            3,
        ),
    );

    // A member that subscribes now is given that group: the page and image.
    let path = tilo_moq::live::namespace_path(&grant.room, &hex::encode(author)).unwrap();
    let namespace = tilo_test_env::within("the session namespace", live.member(&path))
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
    let listed = wire::Stream::new(SURFACE, "so.tilo.browser", wire::PAINT, 800, 600);
    tilo_test_env::within("the stream index", async {
        loop {
            if let Some(mut group) = index.recv_group().await.unwrap()
                && let Some(frame) = group.read_frame().await.unwrap()
                && wire::Frame::decode(&frame.payload).unwrap()
                    == wire::Frame::Index(vec![listed.clone()])
            {
                break;
            }
        }
    })
    .await;
    let mut stream = namespace
        .track(&track(&wire::stream_record(SURFACE)))
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let mut group = tilo_test_env::within("the replayed group", stream.recv_group())
        .await
        .unwrap()
        .unwrap();
    let remote = RemotePage::new();
    assert!(remote.push(&next_payload!(group)), "the whole page");
    assert!(remote.push(&next_payload!(group)), "its image");
    assert_eq!(remote.frame().as_deref(), Some(&first));
    assert_eq!(remote.blob(&hash).as_deref(), Some(&image));

    // The page scrolls: one small patch, applied to what the member holds.
    let second = page(2, 300.0, hash);
    let scrolled = Payload::Patches(vec![first.diff(&second)]);
    assert!(
        scrolled.encode().unwrap().len() < 256,
        "a scroll is a few bytes"
    );
    publish(&native, author, &frame(&scrolled, 4));
    assert!(remote.push(&next_payload!(group)));
    assert_eq!(remote.frame().as_deref(), Some(&second));

    // The member points at the page: page pixels go to the session alone.
    remote.pointer_motion(20.0, 30.0);
    let request = wire::InputRequest {
        workspace_id: workspace(),
        publisher: author,
        surface: SURFACE,
        events: remote.take_input(),
    };
    assert!(request.valid());
    let (tx, rx) = tokio::sync::oneshot::channel();
    browser
        .legacy_input_then(
            request,
            Box::new(move |result| {
                let _ = tx.send(result);
            }),
        )
        .unwrap();
    assert!(
        tilo_test_env::within("the input answer", rx)
            .await
            .unwrap()
            .unwrap()
    );
    tilo_test_env::within("the session's input", async {
        loop {
            if let Some(Envelope::Presence { payload, .. }) = native.recv().await {
                let attributed = AttributedPresence::decode(&payload).unwrap();
                if let Ok(NeighbourMsg::RemoteInput { surface, event, .. }) =
                    postcard::from_bytes(&attributed.body)
                {
                    assert_eq!(surface, SURFACE);
                    assert_eq!(
                        postcard::from_bytes::<wire::InputEvent>(&event).unwrap(),
                        wire::InputEvent::PointerMotion { x: 20.0, y: 30.0 }
                    );
                    break;
                }
            }
        }
    })
    .await;
    drop(live);
    browser.close();
    native.close().await;
    drop(web);
    relay.shutdown().await;
}
