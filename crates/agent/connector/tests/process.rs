#![cfg(feature = "test-worker")]
#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_connector::{BlobAccess, ConnectorProcess, PerformError, WorkerCommand};

#[derive(Default)]
struct Blobs {
    stored: Mutex<Vec<Vec<u8>>>,
    published: Mutex<Vec<String>>,
}

impl BlobAccess for Blobs {
    fn read(&self, blake3: &str) -> Option<Vec<u8>> {
        (blake3 == "known").then(|| b"bytes".to_vec())
    }
    fn store(&self, bytes: &[u8]) -> Result<String, String> {
        self.stored.lock().unwrap().push(bytes.to_vec());
        Ok(blake3::hash(bytes).to_hex().to_string())
    }
    fn publish(&self, blake3: &str) -> Result<(), String> {
        self.published.lock().unwrap().push(blake3.to_owned());
        Ok(())
    }
}

fn command() -> WorkerCommand {
    WorkerCommand {
        program: env!("CARGO_BIN_EXE_scripted-connector-worker").into(),
        args: Vec::new(),
        env: Vec::new(),
        metrics_file: None,
    }
}

fn secrets() -> tilo_secrets::SecretStore {
    let store = tilo_secrets::SecretStore::memory("so.tilo.test-connector");
    store
        .item("bot-token", "default")
        .put_blocking(b"token")
        .unwrap();
    store
}

async fn start(dir: &std::path::Path, blobs: Arc<Blobs>) -> ConnectorProcess {
    ConnectorProcess::start("so.tilo.test-connector", command(), dir, secrets(), blobs)
        .await
        .unwrap()
}

#[tokio::test]
async fn performs_publish_and_attach_first() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = Arc::new(Blobs::default());
    let process = start(dir.path(), blobs.clone()).await;
    let result = process
        .perform(
            serde_json::json!({"op": {"op": "send"}, "publish": ["avatar"], "attach": ["known"]}),
        )
        .await
        .unwrap();
    assert_eq!(result["op"], "send");
    assert_eq!(*blobs.published.lock().unwrap(), ["avatar"]);
    let missing = process
        .perform(serde_json::json!({"op": {"op": "send"}, "attach": ["unknown"]}))
        .await;
    assert_eq!(
        missing,
        Err(PerformError::Refused("missing attachment".into()))
    );
    let refused = process
        .perform(serde_json::json!({"op": {"op": "refuse"}}))
        .await;
    assert_eq!(
        refused,
        Err(PerformError::Refused("Missing Permissions".into()))
    );
    process.stop().await;
}

#[tokio::test]
async fn events_secrets_and_checkpoints_reach_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let process = start(dir.path(), Arc::new(Blobs::default())).await;
    process
        .subscribe()
        .wait_for(|last| *last >= 1)
        .await
        .unwrap();
    assert_eq!(
        process.events().lock().await.after(0, 0).next().unwrap().id,
        "boot"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while process.status() != (true, "token checked".to_owned()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let checkpoint: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("checkpoint.json")).unwrap())
            .unwrap();
    assert_eq!(checkpoint, serde_json::json!({"seen": 3}));
    process.stop().await;
}

#[tokio::test]
async fn a_crash_fails_the_pending_perform_and_the_worker_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let process = start(dir.path(), Arc::new(Blobs::default())).await;
    let crashed = process
        .perform(serde_json::json!({"op": {"op": "crash"}}))
        .await;
    assert!(matches!(crashed, Err(PerformError::Unavailable(_))));
    let again = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(value) = process
                .perform(serde_json::json!({"op": {"op": "ping"}}))
                .await
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(again["op"], "ping");
    let boots = process
        .events()
        .lock()
        .await
        .after(0, 0)
        .filter(|e| e.id == "boot")
        .count();
    assert_eq!(
        boots, 1,
        "the restarted worker's repeated event is deduplicated"
    );
    process.stop().await;
}

#[tokio::test]
async fn downloaded_blobs_are_stored_before_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = Arc::new(Blobs::default());
    let process = start(dir.path(), blobs.clone()).await;
    process
        .perform(serde_json::json!({"op": {"op": "download"}}))
        .await
        .unwrap();
    assert_eq!(*blobs.stored.lock().unwrap(), [b"downloaded".to_vec()]);
    process.stop().await;
}
