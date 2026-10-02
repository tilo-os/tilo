//! Two isolated Chromium profiles exchange real microphone Opus through
//! a governed relay call, including mute, reconnect, leave and revocation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test assertions and fixture setup fail loudly"
)]
#[path = "support/call.rs"]
mod call;
mod support;
tilo_automerge_isolate::test_worker!();

use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use test_http::{Reply, Responder};
use tilo_browser_peer::{Host, PeerEvent, PeerHandle, VoiceEvent};
use tilo_moq::client::{
    CapturedOpus, Client, ClientAuthority, Connection, ConnectionScope, Status,
};
use tilo_moq::relay::Direction;
use tilo_relay_protocol::{Envelope, voice::VoiceGrant};

struct Granted;
impl ClientAuthority for Granted {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}
fn carrier(grant: &VoiceGrant, generation: u64) -> Client {
    Client::start(
        Connection {
            scope: ConnectionScope {
                endpoint: grant.endpoint.clone(),
                fingerprints: grant.fingerprints.clone(),
                owner_epoch: grant.participation,
                carrier_generation: generation,
                publication: grant.publication.clone(),
                subscriptions: grant.subscriptions.clone(),
            },
            authorization: grant.token.clone(),
        },
        Arc::new(Granted),
    )
    .unwrap()
}
fn grant_json(grant: &VoiceGrant) -> Value {
    json!({
        "call": hex::encode(grant.call), "epoch": grant.epoch,
        "participation": hex::encode(grant.participation), "token": hex::encode(&grant.token),
        "endpoint": grant.endpoint, "fingerprints": grant.fingerprints,
        "directions": grant.directions, "publication": grant.publication,
        "subscriptions": grant.subscriptions, "expiresAtNs": grant.expires_at_ns,
        "participants": grant.participants.iter().map(|p| json!({
            "namespace": p.namespace, "device": hex::encode(p.device_public_key),
            "user": hex::encode(p.user_public_key), "publishes": p.publishes,
        })).collect::<Vec<_>>(),
    })
}
struct Page {
    room: String,
    peer: PeerHandle,
    last: Mutex<Option<VoiceGrant>>,
}
impl Page {
    fn events(&self) -> Value {
        let events: Vec<Value> = self
            .peer
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::Voice(VoiceEvent::Granted { grant, .. }) => {
                    *self.last.lock().unwrap() = Some((*grant).clone());
                    Some(json!({"room": self.room, "kind": "granted", "grant": grant_json(&grant)}))
                }
                PeerEvent::Voice(VoiceEvent::Ended { call, reason, .. }) => {
                    Some(json!({"room": self.room, "kind": "ended", "call": hex::encode(call), "reason": reason}))
                }
                PeerEvent::Voice(VoiceEvent::Calls {result, ..}) => Some(match result {
                    Ok(calls) => json!({"room": self.room, "kind": "calls", "calls": calls.iter().map(|call| json!({
                        "call": hex::encode(call.state.call), "epoch": call.state.epoch, "active": call.state.active,
                        "app": call.app_id, "record": call.record_id, "channel": call.channel_id,
                    })).collect::<Vec<_>>()}),
                    Err(error) => json!({"room": self.room, "kind": "calls", "error": error}),
                }),
                _ => None,
            })
            .collect();
        json!(events)
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay fixture is consumed by shutdown after the final grant assertion; dropping it at Clippy's suggested location uses a moved value"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_chromium_profiles_exchange_microphone_audio_in_a_governed_call() {
    let room = call::GovernedCall::start().await;
    let (_, bob_device) = room.invite("bob-browser").await;
    let (_, carol_device) = room.invite("carol-browser").await;
    let page = |device: support::Device| {
        Arc::new(Page {
            room: room.room.clone(),
            peer: support::start_peer(
                Host::Https(room.https()),
                device.cert,
                room.host.cert.clone(),
                device.key,
            ),
            last: Mutex::new(None),
        })
    };
    let bob = page(bob_device);
    let carol = page(carol_device);
    bob.peer.join(room.workspace_id).unwrap();
    carol.peer.join(room.workspace_id).unwrap();
    support::ready(&[&bob.peer, &carol.peer]).await;
    room.owner
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: room.call.call,
            participation: None,
        })
        .unwrap();
    let alice = call::native(&room.owner, |event| match event {
        Envelope::VoiceGranted { grant, .. } => Some(*grant),
        _ => None,
    })
    .await;
    let alice_id = hex::encode(alice.participation);
    let mut generation = 1_u64;
    let mut native = carrier(&alice, generation);
    let mut native_grant = alice.clone();
    let mut retry_at = std::time::Instant::now();
    let mut native_reconnects = 0_usize;

    let mut native_received = std::collections::BTreeMap::<String, usize>::new();
    let marks = Arc::new(Mutex::new(Vec::<String>::new()));
    let server_marks = marks.clone();
    let server_bob = bob.clone();
    let server_carol = carol.clone();
    let call_id = hex::encode(room.call.call);
    let room_id = room.room.clone();
    let workspace = room.workspace_id;
    let call = room.call.call;
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../");
    let server = Responder::handle(move |request| {
        let path = request.target.split('?').next().unwrap();
        let text = |body: String| Some(Reply::status(200).body(body));
        if path == "/api/call" {
            return text(call_id.clone());
        }
        if path == "/api/room" {
            return text(room_id.clone());
        }
        if path == "/api/alice" {
            return text(alice_id.clone());
        }
        if let Some(mark) = path.strip_prefix("/api/mark/") {
            server_marks.lock().unwrap().push(mark.into());
            return text(String::new());
        }
        for (who, page) in [("bob", &server_bob), ("carol", &server_carol)] {
            if let Some(op) = path.strip_prefix(&format!("/api/{who}/")) {
                if op == "events" {
                    return text(page.events().to_string());
                }
                if op == "leave" {
                    page.peer.voice_leave(workspace).unwrap();
                } else if op.starts_with("join/") {
                    page.peer.voice_join(workspace, call).unwrap();
                } else if op == "calls" {
                    page.peer.voice_calls(workspace).unwrap();
                } else if op == "reconnect" {
                    page.peer.reconnect().unwrap();
                } else {
                    return Some(Reply::status(404));
                }
                return text(String::new());
            }
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
                .body(std::fs::read(file).unwrap()),
        )
    });
    let url = server.url();
    let driver = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/voice_browsers/driver.mjs");
    let mut browser = tokio::process::Command::new("node")
        .arg(driver)
        .arg(url)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut timestamp = 0_u64;
    let status = tilo_test_env::within("the two microphone browsers", async {
        loop {
            tokio::select! {
                result = browser.wait() => break result.unwrap(),
                event = room.owner.recv() => {
                    if let Some(Envelope::VoiceGranted {grant, ..}) = event
                        && (grant.token != native_grant.token || grant.subscriptions != native_grant.subscriptions) {
                            native.close();
                            native_grant = *grant;
                            generation = generation.checked_add(1).unwrap();
                            native = carrier(&native_grant, generation);
                    }
                }
                _ = tick.tick() => {
                    // The production native client retires its carrier when a
                    // remote publication ends. Its owner must reopen it, just
                    // as the browser owner does after renewal or reconnect.
                    if native.status() == Status::Failed && std::time::Instant::now() >= retry_at {
                        native.close();
                        generation = generation.checked_add(1).unwrap();
                        native_reconnects = native_reconnects.checked_add(1).unwrap();
                        native = carrier(&native_grant, generation);
                        retry_at = std::time::Instant::now().checked_add(Duration::from_millis(250)).unwrap();
                    }
                    if timestamp.is_multiple_of(240_000) {
                        room.owner.send(Envelope::VoiceJoin {workspace_id: room.workspace_id, call: room.call.call, participation: Some(alice.participation)}).unwrap();
                    }
                    let _ = native.enable_publication();
                    let _ = native.try_send(CapturedOpus {timestamp_48k: timestamp, duration_samples: 960, payload: vec![0xf8, 0xff, 0xfe], captured_at: std::time::Instant::now()});
                    timestamp = timestamp.checked_add(960).unwrap();
                    while let Some(frame) = native.try_recv() {
                        assert_eq!(frame.object.duration, 960);
                        assert!(!frame.object.payload.is_empty());
                        let count = native_received.entry(frame.publisher.namespace).or_default();
                        *count = count.checked_add(1).unwrap();
                    }
                }
            }
        }
    }).await;
    assert!(status.success(), "Chromium voice driver failed: {status}");
    let marks = marks.lock().unwrap().clone();
    assert!(marks.iter().any(|mark| mark == "done"));
    for who in ["bob", "carol"] {
        let prefix = format!("{who}=");
        let participation = marks
            .iter()
            .find_map(|mark| mark.strip_prefix(&prefix))
            .unwrap();
        let namespace = format!("{}/{participation}", hex::encode(room.call.call));
        assert!(
            native_received.get(&namespace).copied().unwrap_or_default() > 100,
            "the production native carrier heard too few {who} microphone frames"
        );
    }
    println!("native_received={native_received:?}, native_reconnects={native_reconnects}");
    // A left participant's last token is no longer admitted, independently
    // of its page having closed the local carrier.
    let stale = carrier(carol.last.lock().unwrap().as_ref().unwrap(), 1);
    support::wait_for("Carol's ended grant cannot open a carrier", || async {
        matches!(
            stale.status(),
            Status::Failed | Status::Denied | Status::Closed
        )
        .then_some(())
    })
    .await;
    native.close();
    stale.close();
    bob.peer.close();
    carol.peer.close();
    drop(server);
    room.shutdown().await;
}
