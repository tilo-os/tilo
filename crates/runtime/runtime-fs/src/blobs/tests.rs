use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::*;

/// Scope-checked in-memory backend: the protocol's contract, not the
/// daemon's storage.
#[derive(Default)]
struct MemoryBackend {
    bytes: Mutex<HashMap<String, Vec<u8>>>,
    refs: Mutex<HashSet<(String, String)>>,
    pending: Mutex<HashSet<String>>,
}

impl BlobBackend for MemoryBackend {
    fn put(
        self: Arc<Self>,
        scope: BlobScope,
        bytes: Vec<u8>,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        Box::pin(async move {
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let len = bytes.len() as u64;
            self.bytes.lock().unwrap().insert(hash.clone(), bytes);
            self.refs
                .lock()
                .unwrap()
                .insert((scope.room().to_owned(), hash.clone()));
            Ok(BlobRef { hash, len, mime })
        })
    }

    fn read(
        self: Arc<Self>,
        scope: BlobScope,
        hash: String,
        offset: u64,
        max_len: u32,
    ) -> BlobFuture<ReadOutcome> {
        Box::pin(async move {
            if self.pending.lock().unwrap().contains(&hash) {
                return ReadOutcome::Pending;
            }
            if !self
                .refs
                .lock()
                .unwrap()
                .contains(&(scope.room().to_owned(), hash.clone()))
            {
                return ReadOutcome::Unavailable;
            }
            let bytes = self.bytes.lock().unwrap();
            let Some(content) = bytes.get(&hash) else {
                return ReadOutcome::Unavailable;
            };
            let start = usize::try_from(offset).unwrap().min(content.len());
            let end = start.saturating_add(max_len as usize).min(content.len());
            let ready = ReadOutcome::Ready {
                bytes: content[start..end].to_vec(),
                total_len: content.len() as u64,
            };
            drop(bytes);
            ready
        })
    }

    fn has(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<bool> {
        Box::pin(async move {
            self.refs
                .lock()
                .unwrap()
                .contains(&(scope.room().to_owned(), hash))
        })
    }

    fn size(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<Option<u64>> {
        Box::pin(async move {
            if !self
                .refs
                .lock()
                .unwrap()
                .contains(&(scope.room().to_owned(), hash.clone()))
            {
                return None;
            }
            self.bytes
                .lock()
                .unwrap()
                .get(&hash)
                .map(|bytes| bytes.len() as u64)
        })
    }
}

struct Harness {
    socket: std::path::PathBuf,
    backend: Arc<MemoryBackend>,
    _dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl Harness {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("blobs.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let backend = Arc::new(MemoryBackend::default());
        spawn_blob_server(
            listener,
            Arc::clone(&backend) as Arc<dyn BlobBackend>,
            runtime.handle().clone(),
        )
        .unwrap();
        Self {
            socket,
            backend,
            _dir: dir,
            _runtime: runtime,
        }
    }

    fn app(&self, room: &str, app: &str) -> BlobClient {
        let mut stream = StdUnixStream::connect(&self.socket).unwrap();
        write_handoff(
            &mut stream,
            &BlobPrincipal::App(BlobScope::new(room, app).unwrap()),
        )
        .unwrap();
        BlobClient::from_app_stream(stream).unwrap()
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn blob_reads_are_confined_to_one_principal_and_scope() {
    // phase: a_second_connection_of_the_same_scope_reads_what_the_first_put
    {
        let harness = Harness::start();
        let writer = harness.app("room-1", "so.tilo.photos");
        let stored = writer.put(b"portable image bytes", "image/png").unwrap();
        assert_eq!(stored.len, 20);
        assert_eq!(stored.mime, "image/png");

        let reader = harness.app("room-1", "so.tilo.photos");
        assert!(reader.has(&stored.hash).unwrap());
        assert_eq!(reader.size(&stored.hash).unwrap(), Some(20));
        assert_eq!(
            reader.read_all(&stored.hash).unwrap(),
            ReadOutcome::Ready {
                bytes: b"portable image bytes".to_vec(),
                total_len: 20,
            }
        );
        assert_eq!(
            reader.read(&stored.hash, 9, 5).unwrap(),
            ReadOutcome::Ready {
                bytes: b"image".to_vec(),
                total_len: 20,
            }
        );
    }
    // phase: another_room_holding_the_hash_reads_nothing
    {
        let harness = Harness::start();
        let stored = harness
            .app("room-1", "so.tilo.photos")
            .put(b"private", "")
            .unwrap();
        // Apps in one room hand content to each other by hash.
        let neighbour = harness.app("room-1", "so.tilo.chat");
        assert!(neighbour.has(&stored.hash).unwrap());
        let stranger = harness.app("room-2", "so.tilo.photos");
        assert_eq!(
            stranger.read_all(&stored.hash).unwrap(),
            ReadOutcome::Unavailable
        );
        assert!(!stranger.has(&stored.hash).unwrap());
        assert_eq!(stranger.size(&stored.hash).unwrap(), None);
    }
    // phase: the_session_reads_only_through_an_app_scope
    {
        let harness = Harness::start();
        let stored = harness
            .app("room-1", "so.tilo.photos")
            .put(b"launch me", "")
            .unwrap();
        let session = BlobClient::connect_session(&harness.socket).unwrap();
        assert_eq!(session.has(&stored.hash), Err(BlobError::InvalidScope));
        let photos = session.scope("room-1", "so.tilo.photos").unwrap();
        assert!(matches!(
            photos.read_all(&stored.hash).unwrap(),
            ReadOutcome::Ready { .. }
        ));
        let elsewhere = session.scope("room-2", "so.tilo.photos").unwrap();
        assert_eq!(
            elsewhere.read_all(&stored.hash).unwrap(),
            ReadOutcome::Unavailable
        );
        assert_eq!(
            photos.scope("room-1", "x").unwrap_err(),
            BlobError::InvalidScope
        );
        assert_eq!(
            session.scope("../room", "so.tilo.chat").unwrap_err(),
            BlobError::InvalidScope
        );
    }
    // phase: handoff_frames_fix_exactly_one_valid_principal
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let decode = |principal: &BlobPrincipal| {
            let mut frame = Vec::new();
            write_handoff(&mut frame, principal).unwrap();
            runtime.block_on(read_handoff(&mut frame.as_slice()))
        };
        let app = BlobPrincipal::App(BlobScope::new("room-1", "so.tilo.notes").unwrap());
        assert_eq!(decode(&app).unwrap(), app);
        assert_eq!(
            decode(&BlobPrincipal::Session).unwrap(),
            BlobPrincipal::Session
        );

        let raw = |json: &str| {
            let mut frame = BLOB_HANDOFF_MAGIC.to_vec();
            frame.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
            frame.extend_from_slice(json.as_bytes());
            runtime.block_on(read_handoff(&mut frame.as_slice()))
        };
        // The launcher's own spelling of the frame.
        assert_eq!(
            raw(r#"{"protocol":1,"principal":"app","room":"room-1","app_id":"so.tilo.notes"}"#)
                .unwrap(),
            app
        );
        for bad in [
            r#"{"protocol":2,"principal":"session"}"#,
            r#"{"protocol":1,"principal":"session","room":"r"}"#,
            r#"{"protocol":1,"principal":"app","room":"..","app_id":"a"}"#,
            r#"{"protocol":1,"principal":"app","room":"a/b","app_id":"a"}"#,
            r#"{"protocol":1,"principal":"root"}"#,
            r#"{"protocol":1,"principal":"session","extra":true}"#,
        ] {
            assert!(raw(bad).is_err(), "{bad} must be refused");
        }
        let mut wrong_magic = b"QCL1".to_vec();
        wrong_magic.extend_from_slice(&0_u32.to_le_bytes());
        assert!(
            runtime
                .block_on(read_handoff(&mut wrong_magic.as_slice()))
                .is_err()
        );
    }
}

#[test]
fn blob_content_streams_verifies_and_refuses_bad_input_early() {
    // phase: content_larger_than_one_message_streams_and_verifies
    {
        let harness = Harness::start();
        let client = harness.app("room-1", "so.tilo.photos");
        let big: Vec<u8> = (0..(MAX_PUT_CHUNK * 2 + 17))
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let stored = client.put(&big, "application/octet-stream").unwrap();
        assert_eq!(stored.len, big.len() as u64);
        assert_eq!(
            client.read_all(&stored.hash).unwrap(),
            ReadOutcome::Ready {
                total_len: big.len() as u64,
                bytes: big,
            }
        );
    }
    // phase: pending_content_is_reported_not_waited_for
    {
        let harness = Harness::start();
        let client = harness.app("room-1", "so.tilo.photos");
        let hash = blake3::hash(b"on its way").to_hex().to_string();
        harness.backend.pending.lock().unwrap().insert(hash.clone());
        assert_eq!(client.read_all(&hash).unwrap(), ReadOutcome::Pending);
    }
    // phase: malformed_hashes_and_oversized_puts_are_refused_before_the_wire
    {
        let harness = Harness::start();
        let client = harness.app("room-1", "so.tilo.photos");
        assert_eq!(client.has("../../etc/passwd"), Err(BlobError::InvalidHash));
        assert_eq!(
            client.read(&"A".repeat(64), 0, 1),
            Err(BlobError::InvalidHash)
        );
        let too_big = vec![0_u8; usize::try_from(MAX_BLOB_BYTES).unwrap() + 1];
        assert!(matches!(
            client.put(&too_big, ""),
            Err(BlobError::TooLarge { .. })
        ));
    }
}
