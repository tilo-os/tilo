#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "test assertions and fixture setup fail loudly; the arithmetic and casts are counters \
              over bounded test sample buffers"
)]
#[path = "support/call.rs"]
mod call;
mod support;
tilo_automerge_isolate::test_worker!();

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::scope::Scope;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use serde_json::{Value, json};
use test_http::{Reply, Responder};
use tilo_browser_peer::{Host, PeerEvent, PeerHandle, VoiceEvent};
use tilo_call_lane::picture::VideoObject;
use tilo_call_lane::{
    ALPN, CAMERA_FPS, CAMERA_SIZE, Calls, Hello, Lane, Layer, Peer, Picture, Roster, Sending, Sink,
    Source, Video,
};
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, VoiceGrant};
use tilo_test_env::SpawnUnderEnvLockAsync as _;

/// One tick of the native participant: a page takes a turn every 20 ms, and
/// so does the device at the other end of its call.
const TICK: Duration = Duration::from_millis(20);
/// A participation lapses without a heartbeat: the native device joins the
/// call again this often, as its shell does.
const HEARTBEAT_TICKS: u64 = 250;
/// The channel's content keys, as the call's members derive them from the
/// channel's MLS group: what seals every frame the page and the native
/// device exchange, and what the bridge never holds.
fn channel_keys(channel: collab_types::ChannelId) -> ChannelContentKeys {
    ChannelGroup::create(channel, MemberKey::generate("alice").unwrap())
        .unwrap()
        .content_keys()
        .unwrap()
}

fn camera_frame(fill: u8) -> tilo_call_lane::Frame {
    let (width, height) = (640, 480);
    debug_assert!(width <= CAMERA_SIZE.0 && height <= CAMERA_SIZE.1);
    tilo_call_lane::Frame {
        pixels: Arc::from(vec![fill; (width * height * 4) as usize]),
        width,
        height,
        format: tilo_media::realtime::InputFormat::Rgba,
        captured_at: Instant::now(),
    }
}

/// The grant the page's session hands its app: what `web/voice.mjs` reads.
fn grant_json(grant: &VoiceGrant) -> Value {
    json!({
        "call": hex::encode(grant.call), "epoch": grant.epoch,
        "participation": hex::encode(grant.participation),
        "directions": grant.directions,
        "participants": grant.participants.iter().map(|p| json!({
            "device": hex::encode(p.device_public_key),
            "user": hex::encode(p.user_public_key), "publishes": p.publishes,
        })).collect::<Vec<_>>(),
    })
}

/// The call operation the page's `voiceCall` sends, as the wasm peer reads
/// it: opening a voice channel's call is all this page's controls ask for.
fn call_operation(body: &[u8]) -> Option<CallOperation> {
    let operation: Value = serde_json::from_slice(body).ok()?;
    let text = |key: &str| operation.get(key)?.as_str().map(str::to_owned);
    if operation.get("kind")?.as_str()? != "open" {
        return None;
    }
    Some(CallOperation::Open {
        app_id: text("app")?,
        record_id: text("record").unwrap_or_default(),
        channel_id: text("channel")?,
    })
}

struct Page {
    workspace: String,
    peer: PeerHandle,
}

impl Page {
    fn events(&self) -> Value {
        let events: Vec<Value> = self
            .peer
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::Voice(VoiceEvent::Granted { grant, .. }) => Some(
                    json!({"room": self.workspace, "kind": "granted", "grant": grant_json(&grant)}),
                ),
                PeerEvent::Voice(VoiceEvent::CallResult { request, result, .. }) => {
                    Some(match result {
                        Ok(state) => json!({"room": self.workspace, "kind": "callResult", "request": hex::encode(request), "call": {
                            "call": hex::encode(state.call), "epoch": state.epoch, "active": state.active,
                            "allowed": {"publish": state.allowed.publish, "subscribe": state.allowed.subscribe},
                        }}),
                        Err(error) => json!({"room": self.workspace, "kind": "callResult", "request": hex::encode(request), "error": error}),
                    })
                }
                PeerEvent::Voice(VoiceEvent::Ended { call, reason, .. }) => Some(
                    json!({"room": self.workspace, "kind": "ended", "call": hex::encode(call), "reason": reason}),
                ),
                PeerEvent::Voice(VoiceEvent::Calls { result, .. }) => Some(match result {
                    Ok(calls) => json!({"room": self.workspace, "kind": "calls", "calls": calls.iter().map(|call| json!({
                        "call": hex::encode(call.state.call), "epoch": call.state.epoch, "active": call.state.active,
                        "app": call.app_id, "record": call.record_id, "channel": call.channel_id,
                    })).collect::<Vec<_>>()}),
                    Err(error) => json!({"room": self.workspace, "kind": "calls", "error": error}),
                }),
                _ => None,
            })
            .collect();
        json!(events)
    }
}

/// The device at the other end of the page's call: a real lane member, as a
/// native session is one. It answers the lane the page's host opens, shows a
/// camera and collects the page's pictures. Nothing of it goes through the
/// host: the frames are sealed with the channel's keys, and the host only
/// copies them. The call's voice is not this device's lane: both devices
/// publish it on the workspace's live plane.
struct Native {
    _endpoint: Endpoint,
    _lanes: Router,
    arrivals: tokio::sync::mpsc::UnboundedReceiver<Peer>,
    device: [u8; 32],
    /// The device the page's frames name: the host that bridges it.
    bridge: [u8; 32],
    video: Arc<Video>,
    sending: Arc<tilo_call_lane::Sending>,
    pictures: Vec<Picture>,
    cursor: u64,
    ticks: u64,
}

impl Native {
    async fn start(
        governed: &call::GovernedCall,
        channel: collab_types::ChannelId,
        keys: &ChannelContentKeys,
    ) -> Self {
        let device = governed.laptop.key.verifying_key().to_bytes();
        let bridge = governed.host.cert.device_public_key;
        assert_ne!(
            device, bridge,
            "the page's bridge and the call's device are two devices"
        );
        let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(SecretKey::from_bytes(&governed.laptop.key.to_bytes()))
            .address_lookup(governed.lookup.clone())
            .relay_mode(RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .transport_config(tilo_call_lane::carrier::transport_config())
            .bind()
            .await
            .unwrap();
        let port = endpoint.bound_sockets()[0].port();
        governed.lookup.add_endpoint_info(
            EndpointAddr::new(endpoint.id())
                .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
        );
        let roster = Roster::new([device, bridge]);
        let (calls, arrivals) = Calls::new(
            Hello {
                workspace: governed.cluster_id.clone(),
                channel,
                call: hex::encode(governed.call.call),
                lane: Lane::Video,
            },
            roster.clone(),
        );
        let lanes = Router::builder(endpoint.clone())
            .accept(ALPN, calls)
            .spawn();
        let video = Video::new(device);
        let scope = Scope::channel(governed.cluster_id.clone(), channel);
        video.set_keys(keys, roster, &scope);
        // The page's camera is shown here: what makes it encode, and at which
        // layer.
        video.show(bridge, &[Layer::Low], &[]);
        let sink: Arc<dyn Sink> = Arc::clone(&video) as Arc<dyn Sink>;
        let sending = Arc::new(
            Sending::start(Source::Camera, CAMERA_FPS, &sink).expect("a camera encoder thread"),
        );
        Self {
            _endpoint: endpoint,
            _lanes: lanes,
            arrivals,
            device,
            bridge,
            video,
            sending,
            pictures: Vec::new(),
            cursor: 0,
            ticks: 0,
        }
    }

    /// Take the lane the page's host opened since the last tick.
    fn arrived(&mut self) {
        while let Ok(peer) = self.arrivals.try_recv() {
            self.video.add(peer);
        }
    }

    /// One tick: join the call again when the participation lapses, send a
    /// camera picture, and take what the page sent.
    fn step(
        &mut self,
        client: &tilo_relay::RelayClient,
        workspace_id: u64,
        call: [u8; 32],
        participation: [u8; 32],
    ) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(HEARTBEAT_TICKS) {
            client
                .send(Envelope::VoiceJoin {
                    workspace_id,
                    call,
                    participation: Some(participation),
                })
                .unwrap();
        }
        if self.ticks.is_multiple_of(3) {
            self.sending.publish(camera_frame(0x40));
        }
        let (taken, cursor) = self.video.take(self.cursor, usize::MAX);
        self.pictures.extend(taken);
        self.cursor = cursor;
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the fixture is consumed by shutdown after the final assertion; dropping its guard at Clippy's suggested location uses a moved value"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_and_a_native_device_exchange_pictures_in_a_governed_call() {
    let governed = call::GovernedCall::start().await;
    let (_, bob_device) = governed.invite("bob-browser").await;
    let page = Arc::new(Page {
        workspace: governed.cluster_id.clone(),
        peer: support::start_peer(
            Host::Https(governed.https()),
            bob_device.cert,
            governed.host.cert.clone(),
            bob_device.key,
        ),
    });
    page.peer.join(governed.workspace_id).unwrap();
    support::ready(&[&page.peer]).await;

    // The call's frames are sealed with its channel's content keys; the page
    // holds them as a channel member, and so does the device it calls.
    let channel = collab_types::document_channel_id(call::CHAT, &governed.cluster_id, "");
    let keys = channel_keys(channel);
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: call::CHAT.into(),
        cluster_id: governed.cluster_id.clone(),
        record: String::new(),
    };
    page.peer.install_channel_keys(route, keys.clone()).unwrap();
    let (installed, settles) = tokio::sync::oneshot::channel();
    page.peer
        .barrier_then(Box::new(move |result| {
            let _ = installed.send(result);
        }))
        .unwrap();
    settles.await.unwrap().unwrap();

    // Alice's own laptop takes part in the call, in the workspace it belongs
    // to: the page's grant names it, and its lanes answer the host's.
    let mut native = Native::start(&governed, channel, &keys).await;
    governed
        .owner
        .send(Envelope::VoiceJoin {
            workspace_id: governed.workspace_id,
            call: governed.call.call,
            participation: None,
        })
        .unwrap();
    let alice = call::native(&governed.owner, |event| match event {
        Envelope::VoiceGranted { grant, .. } => Some(*grant),
        _ => None,
    })
    .await;

    let marks = Arc::new(Mutex::new(Vec::<String>::new()));
    let server_marks = Arc::clone(&marks);
    let server_page = Arc::clone(&page);
    let call_id = hex::encode(governed.call.call);
    let workspace_cluster = governed.cluster_id.clone();
    let channel_id = governed.channel.clone();
    let workspace = governed.workspace_id;
    let call = governed.call.call;
    let native_device = hex::encode(native.device);
    let host_device = hex::encode(native.bridge);
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../");
    let server = Responder::handle(move |request| {
        let path = request.target.split('?').next().unwrap();
        let text = |body: String| Some(Reply::status(200).body(body));
        if path == "/api/call" {
            return text(call_id.clone());
        }
        if path == "/api/room" {
            return text(workspace_cluster.clone());
        }
        // The chat's voice channel the governed call is on: its id is the
        // chat's own, minted when the workspace was made.
        if path == "/api/channel" {
            return text(channel_id.clone());
        }
        // The device this page's frames name (its host's), and the device of
        // the native participant the page's picture comes from.
        if path == "/api/host" {
            return text(host_device.clone());
        }
        if path == "/api/native" {
            return text(native_device.clone());
        }
        if let Some(mark) = path.strip_prefix("/api/mark/") {
            server_marks.lock().unwrap().push(mark.into());
            return text(String::new());
        }
        if let Some(op) = path.strip_prefix("/api/bob/") {
            if op == "events" {
                return text(server_page.events().to_string());
            }
            if op == "leave" {
                server_page.peer.voice_leave(workspace).unwrap();
            } else if op.starts_with("join/") {
                server_page.peer.voice_join(workspace, call).unwrap();
            } else if op == "calls" {
                server_page.peer.voice_calls(workspace).unwrap();
            } else if let Some(id) = op.strip_prefix("call/") {
                let Some(operation) = call_operation(&request.body) else {
                    return Some(Reply::status(400));
                };
                let id = hex::decode(id)
                    .ok()
                    .and_then(|id| <[u8; 16]>::try_from(id).ok());
                let Some(id) = id else {
                    return Some(Reply::status(400));
                };
                server_page
                    .peer
                    .voice_call(workspace, id, operation)
                    .unwrap();
            } else if op == "reconnect" {
                server_page.peer.reconnect().unwrap();
            } else if op == "turn" {
                // One turn of the page's call: what the page sealed goes to
                // its host's call lane, and the answer is what came back.
                let Ok(turn) = serde_json::from_slice(&request.body) else {
                    return Some(Reply::status(400));
                };
                let (done, answer) = std::sync::mpsc::channel();
                let completing = Box::new(move |answer| {
                    let _ = done.send(answer);
                });
                if server_page
                    .peer
                    .call_lane_then(&workspace_cluster, turn, completing)
                    .is_err()
                {
                    return Some(Reply::status(503).body("the peer session is closed"));
                }
                return match answer.recv_timeout(Duration::from_secs(20)) {
                    Ok(Ok(answer)) => text(serde_json::to_string(&answer).unwrap_or_default()),
                    Ok(Err(error)) => Some(Reply::status(409).body(error)),
                    Err(_) => Some(Reply::status(504).body("the host did not answer")),
                };
            } else if op == "end" {
                server_page.peer.call_lane_end(&workspace_cluster).unwrap();
            } else {
                return Some(Reply::status(404));
            }
            return text(String::new());
        }
        let file = if path == "/page.html" {
            repository.join("crates/browser/tilo-browser-peer/tests/voice_browsers/page.html")
        } else if let Some(name) = path.strip_prefix("/web/") {
            if name.contains('/') || name.contains("..") {
                return Some(Reply::status(404));
            }
            repository
                .join("crates/browser/tilo-browser-runtime/web")
                .join(name)
        } else {
            return Some(Reply::status(404));
        };
        Some(
            Reply::status(200)
                .header(
                    "Content-Type",
                    if path == "/page.html" {
                        "text/html"
                    } else {
                        "text/javascript"
                    },
                )
                .body(
                    std::fs::read(&file)
                        .unwrap_or_else(|error| panic!("serving {}: {error}", file.display())),
                ),
        )
    });
    let url = server.url();
    let driver = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/voice_browsers/driver.mjs");
    let mut browser = tokio::process::Command::new("node")
        .arg(driver)
        .arg(url)
        .kill_on_drop(true)
        .spawn_locked()
        .unwrap();
    let mut tick = tokio::time::interval(TICK);
    let status = tilo_test_env::within("the page and the native device in one call", async {
        loop {
            tokio::select! {
                result = browser.wait() => break result.unwrap(),
                event = governed.owner.recv() => {
                    if let Some(Envelope::VoiceRefused {reason, ..}) = &event {
                        panic!("the relay refused the native device's heartbeat: {reason}");
                    }
                }
                _ = tick.tick() => {
                    native.arrived();
                    native.step(&governed.owner, governed.workspace_id, governed.call.call, alice.participation);
                }
            }
        }
    }).await;
    assert!(status.success(), "Chromium call driver failed: {status}");
    let marks = marks.lock().unwrap().clone();
    assert!(marks.iter().any(|mark| mark == "done"));

    // The page's camera reached the native device, in a stream that starts at
    // a keyframe a decoder can begin at.
    assert!(
        native.pictures.iter().any(|picture| {
            picture.from == native.bridge
                && VideoObject::decode(&picture.object)
                    .is_ok_and(|object| object.keyframe && object.width > 0 && object.height > 0)
        }),
        "the page's picture never reached the native device: {:?}",
        native
            .pictures
            .iter()
            .map(|picture| (hex::encode(picture.from), picture.object.len()))
            .collect::<Vec<_>>()
    );
    println!("native took {} pictures", native.pictures.len());
    native.video.close();
    page.peer.close();
    drop(server);
    governed.shutdown().await;
}
