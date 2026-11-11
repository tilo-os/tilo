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

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tilo_session_client::{Cmd, Outbox};

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::scope::Scope;
use iroh::{Endpoint, RelayMode, SecretKey};
use serde_json::{Value, json};
use test_http::{Reply, Responder};
use tilo_live::live::Track;
use tilo_live::plane::Heard;
use tilo_live::plane::{Join, Plane};
use tilo_live::roster::Roster;
use tilo_live::video::{Codec, Layer, Source, VideoObject};
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, VoiceGrant};
use tilo_session_client::Event;
use tilo_session_client::VoiceEvent;
use tilo_session_host::PeerHandle;
use tilo_test_env::SpawnUnderEnvLockAsync as _;

/// One tick of the native participant: a page publishes its pictures as its
/// encoders make them, and the device at the other end of its call sends its
/// own at the frames a camera makes.
const TICK: Duration = Duration::from_millis(20);
/// A participation lapses without a heartbeat: the native device joins the
/// call again this often, as its shell does.
const HEARTBEAT_TICKS: u64 = 250;
/// The workspace scope's content keys, as its members derive them: what seals
/// every frame the page and the native device exchange.
fn channel_keys(channel: collab_types::ChannelId) -> ChannelContentKeys {
    ChannelGroup::create(channel, MemberKey::generate("alice").unwrap())
        .unwrap()
        .content_keys()
        .unwrap()
}

/// The workspace scope's record: its channel's keys seal the plane's tracks.
fn plane_route(workspace: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: collab_types::scope::MEDIA_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        record: collab_types::scope::WORKSPACE_RECORD.to_owned(),
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

/// The source `web/voice-video.mjs` names.
fn source_name(source: Source) -> &'static str {
    match source {
        Source::Camera => "camera",
        Source::Screen => "screen",
    }
}

/// The layer `web/voice-video.mjs` names.
fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Low => "low",
        Layer::Mid => "mid",
        Layer::High => "high",
    }
}

/// A source from the page's name.
fn source_of(name: &str) -> Option<Source> {
    match name {
        "camera" => Some(Source::Camera),
        "screen" => Some(Source::Screen),
        _ => None,
    }
}

/// A layer from the page's name.
fn layer_of(name: &str) -> Option<Layer> {
    match name {
        "low" => Some(Layer::Low),
        "mid" => Some(Layer::Mid),
        "high" => Some(Layer::High),
        _ => None,
    }
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
                Event::Voice(VoiceEvent::Granted { grant, .. }) => Some(
                    json!({"room": self.workspace, "kind": "granted", "grant": grant_json(&grant)}),
                ),
                Event::Voice(VoiceEvent::CallResult { request, result, .. }) => {
                    Some(match result {
                        Ok(state) => json!({"room": self.workspace, "kind": "callResult", "request": hex::encode(request), "call": {
                            "call": hex::encode(state.call), "epoch": state.epoch, "active": state.active,
                            "allowed": {"publish": state.allowed.publish, "subscribe": state.allowed.subscribe},
                        }}),
                        Err(error) => json!({"room": self.workspace, "kind": "callResult", "request": hex::encode(request), "error": error}),
                    })
                }
                Event::Voice(VoiceEvent::Ended { call, reason, .. }) => Some(
                    json!({"room": self.workspace, "kind": "ended", "call": hex::encode(call), "reason": reason}),
                ),
                Event::CallPicture {
                    from,
                    source,
                    keyframe,
                    object,
                    ..
                } => Some(json!({
                    "room": self.workspace, "kind": "picture", "from": hex::encode(from),
                    "source": source_name(source), "keyframe": keyframe, "object": hex::encode(object),
                })),
                Event::CallDemand {
                    wanted, keyframes, ..
                } => {
                    let mut by_source: Vec<Value> = Vec::new();
                    for (source, layer) in wanted {
                        match by_source.iter_mut().find(|entry| entry["source"] == source_name(source)) {
                            Some(entry) => entry["layers"].as_array_mut().unwrap().push(json!(layer_name(layer))),
                            None => by_source.push(json!({"source": source_name(source), "layers": [layer_name(layer)]})),
                        }
                    }
                    Some(json!({
                        "room": self.workspace, "kind": "demand", "wanted": by_source,
                        "keyframes": keyframes.into_iter().map(source_name).collect::<Vec<_>>(),
                    }))
                }
                Event::Voice(VoiceEvent::Calls { result, .. }) => Some(match result {
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

/// The device at the other end of the page's call: a native session, as one
/// is. It joins the workspace's plane with its own device key, shows the
/// page's camera, sends its own and collects what the page sent. Nothing of
/// it goes through the host: the plane seals with the workspace's keys, and
/// the host only carries what it cannot open.
struct Native {
    _endpoint: Endpoint,
    plane: Plane,
    device: [u8; 32],
    /// The page's device: who its pictures come from.
    page: [u8; 32],
    pictures: Vec<Heard>,
    sequence: u32,
    /// Whether a picture of this device's layer joined its group: a layer
    /// starts with a keyframe, so every send until one lands repeats it.
    started: bool,
    ticks: u64,
}

impl Native {
    async fn start(
        governed: &call::GovernedCall,
        page: [u8; 32],
        keys: &ChannelContentKeys,
    ) -> Self {
        let device = governed.laptop.key.verifying_key().to_bytes();
        assert_ne!(
            device, page,
            "the page's device and the call's device are two devices"
        );
        let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(SecretKey::from_bytes(&governed.laptop.key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let workspace = tilo_live::live::workspace_bytes(&governed.cluster_id)
            .expect("the workspace's record id");
        let scope = Scope::workspace(&governed.cluster_id);
        let grant = tilo_live::grant::Native::new(
            workspace,
            &scope.id(),
            1,
            serde_json::to_vec(&governed.laptop.cert).unwrap(),
            Vec::new(),
        )
        .expect("a native grant");
        let carrier = tilo_live::native::Iroh::new(endpoint.clone(), governed.relay.node_addr());
        let plane = Plane::join(
            &carrier,
            &grant.encode(),
            Join {
                scope,
                device,
                keys: keys.clone(),
                roster: Roster::new([device, page]),
                publish: true,
            },
        )
        .await
        .expect("the native device's plane");
        // The page's camera is shown here: what makes it encode, and at which
        // layer.
        plane.show(page, &[Track::Camera(Layer::Low)]);
        Self {
            _endpoint: endpoint,
            plane,
            device,
            page,
            pictures: Vec::new(),
            sequence: 0,
            started: false,
            ticks: 0,
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
            let object = VideoObject {
                keyframe: !self.started,
                codec: Codec::H264,
                layer: Layer::Low,
                sequence: u64::from(self.sequence),
                timestamp_us: u64::from(self.sequence).saturating_mul(33_000),
                width: 64,
                height: 48,
                payload: vec![0x40; 64],
            };
            self.sequence = self.sequence.saturating_add(1);
            self.started = self.plane.send_picture(Source::Camera, &object).is_ok();
        }
        while let Some(heard) = self.plane.try_recv() {
            if matches!(heard.track, Track::Camera(_) | Track::Screen(_)) {
                self.pictures.push(heard);
            }
        }
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
    let page_device = bob_device.cert.device_public_key;
    let page = Arc::new(Page {
        workspace: governed.cluster_id.clone(),
        peer: support::start_peer(
            governed.iroh(),
            bob_device.cert,
            governed.host.cert.clone(),
            bob_device.key,
        ),
    });
    page.peer.send(Cmd::Join(governed.workspace_id)).unwrap();
    support::ready(&[&page.peer]).await;

    // The plane's frames are sealed with the workspace scope's keys; the page
    // holds them as a workspace member, and so does the device it calls.
    let keys = channel_keys(collab_types::ChannelId::new());
    page.peer
        .send(Cmd::InstallChannelKeys(
            plane_route(&governed.cluster_id),
            keys.clone(),
        ))
        .unwrap();
    let settles = page.peer.barrier().unwrap();
    settles.await.unwrap().unwrap();

    // Alice's own laptop takes part in the call, in the workspace it belongs
    // to: its plane joins on its own device key, as any device's does.
    let mut native = Native::start(&governed, page_device, &keys).await;
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
    let page_host = hex::encode(native.page);
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
        // The page's own device, and the device of the native participant
        // the page's picture comes from.
        if path == "/api/host" {
            return text(page_host.clone());
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
                server_page.peer.send(Cmd::VoiceLeave(workspace)).unwrap();
            } else if op.starts_with("join/") {
                server_page
                    .peer
                    .send(Cmd::VoiceJoin(workspace, call))
                    .unwrap();
            } else if op == "calls" {
                server_page.peer.send(Cmd::VoiceCalls(workspace)).unwrap();
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
                    .send(Cmd::VoiceCall(workspace, id, operation))
                    .unwrap();
            } else if op == "reconnect" {
                server_page.peer.send(Cmd::Reconnect).unwrap();
            } else if op == "pictures" {
                // The pictures the page's encoders made, as they are made:
                // published on the workspace's plane as the page's device.
                let Ok(pictures) = serde_json::from_slice::<Vec<Value>>(&request.body) else {
                    return Some(Reply::status(400));
                };
                let pictures: Vec<tilo_session_client::CallPicture> = pictures
                    .into_iter()
                    .filter_map(|picture| {
                        Some(tilo_session_client::CallPicture {
                            source: source_of(picture.get("source")?.as_str()?)?,
                            layer: layer_of(picture.get("layer")?.as_str()?)?,
                            sequence: picture.get("sequence")?.as_u64()?,
                            keyframe: picture.get("keyframe")?.as_bool()?,
                            timestamp_us: picture.get("timestampUs")?.as_u64()?,
                            width: u16::try_from(picture.get("width")?.as_u64()?).ok()?,
                            height: u16::try_from(picture.get("height")?.as_u64()?).ok()?,
                            payload: hex::decode(picture.get("payload")?.as_str()?).ok()?,
                        })
                    })
                    .collect();
                server_page
                    .peer
                    .send(Cmd::CallPictures(workspace, pictures))
                    .unwrap();
            } else if op == "showing" {
                // What the page draws of the call's devices: the plane reads
                // exactly those layers.
                let Ok(shown) = serde_json::from_slice::<Vec<Value>>(&request.body) else {
                    return Some(Reply::status(400));
                };
                let shown: Vec<tilo_session_client::CallShown> = shown
                    .into_iter()
                    .filter_map(|entry| {
                        let layers = |key: &str| -> Vec<Layer> {
                            entry
                                .get(key)
                                .and_then(Value::as_array)
                                .map(|layers| {
                                    layers
                                        .iter()
                                        .filter_map(|layer| layer_of(layer.as_str()?))
                                        .collect()
                                })
                                .unwrap_or_default()
                        };
                        Some(tilo_session_client::CallShown {
                            device: hex::decode(entry.get("device")?.as_str()?)
                                .ok()?
                                .try_into()
                                .ok()?,
                            camera: layers("camera"),
                            screen: layers("screen"),
                        })
                    })
                    .collect();
                server_page
                    .peer
                    .send(Cmd::CallShowing(workspace, shown))
                    .unwrap();
            } else if let Some(rest) = op.strip_prefix("keyframe/") {
                // A device's picture the page needs to start decoding at.
                let Some((device, source)) = rest.split_once('/') else {
                    return Some(Reply::status(400));
                };
                let Ok(device) = hex::decode(device) else {
                    return Some(Reply::status(400));
                };
                let Ok(device) = <[u8; 32]>::try_from(device) else {
                    return Some(Reply::status(400));
                };
                let Some(source) = source_of(source) else {
                    return Some(Reply::status(400));
                };
                server_page
                    .peer
                    .send(Cmd::CallPictureRestart(workspace, device, source))
                    .unwrap();
            } else {
                return Some(Reply::status(404));
            }
            return text(String::new());
        }
        let file = if path == "/page.html" {
            repository.join("crates/session/host/tests/voice_browsers/page.html")
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
            picture.sender == native.page
                && VideoObject::decode(&picture.body)
                    .is_ok_and(|object| object.keyframe && object.width > 0 && object.height > 0)
        }),
        "the page's picture never reached the native device: {:?}",
        native
            .pictures
            .iter()
            .map(|picture| (hex::encode(picture.sender), picture.body.len()))
            .collect::<Vec<_>>()
    );
    println!("native took {} pictures", native.pictures.len());
    page.peer.close();
    drop(server);
    governed.shutdown().await;
}
