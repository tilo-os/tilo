//! The `discord-worker` binary under the connector host, against
//! fake-discord: secrets come from the host's store, events land in its
//! log, webhook tokens go back into its store, CDN bytes into its blobs,
//! and its metrics into the metrics file.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use discord_wire::event::{event_id, source};
use fake_discord::{CHANNEL_GENERAL, FakeDiscord, TOKEN, User};
use serde_json::json;
use tilo_connector::{BlobAccess, ConnectorProcess, WorkerCommand};

const APP: &str = "so.tilo.discord";

#[derive(Default)]
struct Blobs {
    stored: Mutex<HashMap<String, Vec<u8>>>,
}

impl BlobAccess for Blobs {
    fn read(&self, blake3: &str) -> Option<Vec<u8>> {
        self.stored.lock().unwrap().get(blake3).cloned()
    }
    fn store(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        self.stored
            .lock()
            .unwrap()
            .insert(hash.clone(), bytes.to_vec());
        Ok(hash)
    }
    fn publish(&self, _blake3: &str) -> Result<(), String> {
        Ok(())
    }
}

async fn until<T>(mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    loop {
        if let Some(found) = check() {
            return Some(found);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_worker_binary_bridges_through_the_connector_host() {
    let fake = FakeDiscord::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secrets = tilo_secrets::SecretStore::memory(APP);
    secrets
        .item("bot-token", "default")
        .put(TOKEN.as_bytes())
        .await
        .unwrap();
    let blobs = Arc::new(Blobs::default());
    let metrics = dir.path().join("metrics.d").join("bridge.prom");
    let command = WorkerCommand {
        program: env!("CARGO_BIN_EXE_discord-worker").into(),
        args: Vec::new(),
        env: vec![
            ("DISCORD_API_BASE".into(), fake.api_base()),
            ("DISCORD_CDN_BASE".into(), fake.cdn_base()),
            ("DISCORD_ALLOW_LOOPBACK".into(), "1".into()),
        ],
        metrics_file: Some(metrics.clone()),
    };
    let process = ConnectorProcess::start(
        APP,
        command,
        &dir.path().join("connector"),
        secrets.clone(),
        blobs.clone(),
    )
    .await
    .unwrap();

    assert!(
        fake.eventually(tilo_test_env::hang_bound(), |f| f.connected().then_some(()))
            .await
            .is_some(),
        "never connected"
    );
    assert!(
        until(|| process.status().0.then_some(())).await.is_some(),
        "never healthy: {:?}",
        process.status()
    );

    // A Discord message lands in the host's event log.
    let posted = fake.post(CHANNEL_GENERAL, &User::new("3001", "ada"), "from Discord");
    let wanted = event_id::message_created(&posted);
    let events = process.events();
    let found = tilo_test_env::within("the logged event", async {
        loop {
            let log = events.lock().await;
            let found = log
                .after(0, 0)
                .find(|e| e.source == source::MESSAGE_CREATED && e.id == wanted)
                .map(|e| e.payload.clone());
            drop(log);
            if let Some(found) = found {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(found["content"], "from Discord");

    // The webhook token goes into the host's store, not the result.
    let created = process
        .perform(
            json!({"op": {"op": "create-webhook", "channel": CHANNEL_GENERAL, "tilo": "room"}}),
        )
        .await
        .unwrap();
    let webhook = created["webhook_id"].as_str().unwrap().to_owned();
    let token = fake.webhook_token(&webhook).unwrap();
    assert!(!created.to_string().contains(&token));
    let stored = until(|| {
        secrets
            .item("webhook", CHANNEL_GENERAL)
            .get_blocking()
            .ok()
            .flatten()
    })
    .await
    .expect("webhook token stored");
    assert_eq!(stored.expose(), token.as_bytes());

    // A send uses that token, fetched back from the store.
    let sent = process
        .perform(json!({"op": {
            "op": "send",
            "channel": CHANNEL_GENERAL,
            "webhook": webhook,
            "username": "Ada (Tilo)",
            "content": "from Tilo",
            "created_ms": 0,
            "tilo": "msg-1",
        }}))
        .await
        .unwrap();
    let message = sent["message_id"].as_str().unwrap();
    assert_eq!(fake.message(message).unwrap()["content"], "from Tilo");

    // CDN bytes reach the host's blobs before the result naming them.
    let url = fake.put_cdn("avatars/3001/abc.png", "image/png", b"png");
    let asset = process
        .perform(json!({"op": {"op": "fetch-asset", "url": url, "max_bytes": 100}}))
        .await
        .unwrap();
    let blob = asset["blob"].as_str().unwrap();
    assert_eq!(blobs.read(blob).as_deref(), Some(&b"png"[..]));

    // The worker's metrics reach the metrics file.
    let text = until(|| std::fs::read_to_string(&metrics).ok())
        .await
        .expect("metrics file");
    assert!(text.contains("tilo_rooms_bridge_queue_depth "), "{text}");

    process.stop().await;
}
