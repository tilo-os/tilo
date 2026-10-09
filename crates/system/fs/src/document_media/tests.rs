use runtime_fs::blobs::{BlobClient, BlobPrincipal, MAX_READ_CHUNK, write_handoff};

use super::*;

fn scope(room: &str, app: &str) -> BlobScope {
    BlobScope::new(room, app).unwrap()
}

#[tokio::test]
async fn stored_media_is_readable_only_by_reference_bounded_and_durable() {
    // Phase `bytes_in_the_store_are_not_readable_without_a_reference`.
    {
        let store = Arc::new(ContentStore::memory());
        let media = DocumentMedia::new(Arc::clone(&store));
        let photos = scope("room-1", "so.tilo.photos");

        let hash = media.store(&photos, b"an image".to_vec()).await.unwrap();
        assert_eq!(
            media.read_range(photos.clone(), hash, 3, 5).await,
            ReadOutcome::Ready {
                bytes: b"image".to_vec(),
                total_len: 8,
            }
        );
        assert!(media.contains(&photos, hash).await);
        assert_eq!(media.len_of(&photos, hash).await, Some(8));

        // Another app in the room may be handed the content by hash.
        assert!(media.contains(&scope("room-1", "so.tilo.chat"), hash).await);

        // Another room: the bytes are right there, and still unreadable, because
        // nothing that room reads references them.
        let other = scope("room-2", "so.tilo.photos");
        assert_eq!(
            media.read_range(other.clone(), hash, 0, u32::MAX).await,
            ReadOutcome::Unavailable
        );
        assert!(!media.contains(&other, hash).await);
        assert_eq!(media.len_of(&other, hash).await, None);

        // Content that reached the store as a file is not document media.
        let file_bytes = store.ingest(Bytes::from_static(b"a file")).await.unwrap();
        assert_eq!(
            media
                .read_range(photos.clone(), file_bytes, 0, u32::MAX)
                .await,
            ReadOutcome::Unavailable
        );
    }
    // Phase `references_survive_a_restart_of_the_store`.
    {
        let dir = tempfile::tempdir().unwrap();
        let photos = scope("room-1", "so.tilo.photos");
        let hash = {
            let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
            let media = DocumentMedia::new(Arc::clone(&store));
            let hash = media.store(&photos, b"durable".to_vec()).await.unwrap();
            // Release the database before reopening it, as a daemon restart does.
            store.store().shutdown().await.unwrap();
            hash
        };
        let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
        let media = DocumentMedia::new(store);
        assert_eq!(
            media.read_range(photos, hash, 0, u32::MAX).await,
            ReadOutcome::Ready {
                bytes: b"durable".to_vec(),
                total_len: 7,
            }
        );
    }
    // Phase `oversized_media_is_refused`.
    {
        let media = DocumentMedia::new(Arc::new(ContentStore::memory()));
        let too_big = vec![0_u8; usize::try_from(MAX_BLOB_BYTES).unwrap() + 1];
        assert!(matches!(
            media
                .store(&scope("room-1", "so.tilo.photos"), too_big)
                .await,
            Err(BlobError::TooLarge { .. })
        ));
    }
}

const PROOF_SOCKET: &str = "TILO_MEDIA_PROOF_SOCKET";
const PROOF_HASH: &str = "TILO_MEDIA_PROOF_HASH";
const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\n pixels put by one application";

fn launcher_connection(socket: &Path, room: &str, app: &str) -> BlobClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    write_handoff(&mut stream, &BlobPrincipal::App(scope(room, app))).unwrap();
    BlobClient::from_app_stream(stream).unwrap()
}

/// The capability end to end: one app process puts an image through the
/// launcher-scoped socket; a second process and the session (as the shell
/// stages launch content) read it; an app in another room holding the same
/// hash reads nothing.
#[test]
fn an_image_put_by_one_process_is_read_by_another_and_the_session_only() {
    if let (Some(socket), Some(hash)) = (
        std::env::var_os(PROOF_SOCKET),
        std::env::var(PROOF_HASH).ok(),
    ) {
        let reader = launcher_connection(Path::new(&socket), "room-1", "so.tilo.photos");
        assert_eq!(
            reader.read_all(&hash).unwrap(),
            ReadOutcome::Ready {
                bytes: IMAGE.to_vec(),
                total_len: IMAGE.len() as u64,
            }
        );
        return;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fs-blobs.sock");
    let _server = {
        let _guard = runtime.enter();
        let media = DocumentMedia::new(Arc::new(ContentStore::memory()));
        serve_socket(&socket, media).unwrap()
    };

    let writer = launcher_connection(&socket, "room-1", "so.tilo.photos");
    let stored = writer.put(IMAGE, "image/png").unwrap();
    assert_eq!(stored.hash, blake3::hash(IMAGE).to_hex().to_string());

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "document_media::tests::an_image_put_by_one_process_is_read_by_another_and_the_session_only",
            "--nocapture",
        ])
        .env(PROOF_SOCKET, &socket)
        .env(PROOF_HASH, &stored.hash)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the second process could not read the image: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // libtest exits 0 when `--exact` selects nothing; require the child ran.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the child ran no test: {stdout}"
    );

    let session = BlobClient::connect_session(&socket).unwrap();
    let shell_view = session.scope("room-1", "so.tilo.photos").unwrap();
    assert!(matches!(
        shell_view.read_all(&stored.hash).unwrap(),
        ReadOutcome::Ready { .. }
    ));

    let stranger = launcher_connection(&socket, "room-2", "so.tilo.chat");
    assert_eq!(
        stranger.read_all(&stored.hash).unwrap(),
        ReadOutcome::Unavailable
    );
    assert!(!stranger.has(&stored.hash).unwrap());
    let elsewhere = session.scope("room-2", "so.tilo.photos").unwrap();
    assert_eq!(
        elsewhere.read_all(&stored.hash).unwrap(),
        ReadOutcome::Unavailable
    );
}

/// The daemon's document media, observed: every ranged read's length is
/// logged, and reads can be made to answer other bytes than were stored.
struct Observed {
    media: Arc<DocumentMedia>,
    reads: std::sync::Mutex<Vec<u32>>,
    puts: std::sync::atomic::AtomicUsize,
    tamper: std::sync::atomic::AtomicBool,
}

impl BlobBackend for Observed {
    fn put(
        self: Arc<Self>,
        scope: BlobScope,
        bytes: Vec<u8>,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Arc::clone(&self.media).put(scope, bytes, mime)
    }

    fn read(
        self: Arc<Self>,
        scope: BlobScope,
        hash: String,
        offset: u64,
        max_len: u32,
    ) -> BlobFuture<ReadOutcome> {
        self.reads.lock().unwrap().push(max_len);
        let tamper = self.tamper.load(std::sync::atomic::Ordering::SeqCst);
        let read = Arc::clone(&self.media).read(scope, hash, offset, max_len);
        Box::pin(async move {
            let mut outcome = read.await;
            if let (true, ReadOutcome::Ready { bytes, .. }) = (tamper, &mut outcome)
                && let Some(byte) = bytes.first_mut()
            {
                *byte ^= 0xff;
            }
            outcome
        })
    }

    fn has(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<bool> {
        Arc::clone(&self.media).has(scope, hash)
    }

    fn size(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<Option<u64>> {
        Arc::clone(&self.media).size(scope, hash)
    }
}

/// The production app path: a `tilo_content::ContentStore` over the device
/// backend, over a launcher-scoped connection to the daemon's document media.
struct DeviceHarness {
    observed: Arc<Observed>,
    store: tilo_content::ContentStore,
    _dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl DeviceHarness {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("fs-blobs.sock");
        // The in-memory store spawns its actor on the ambient runtime.
        let media = {
            let _guard = runtime.enter();
            DocumentMedia::new(Arc::new(ContentStore::memory()))
        };
        let observed = Arc::new(Observed {
            media,
            reads: std::sync::Mutex::default(),
            puts: std::sync::atomic::AtomicUsize::default(),
            tamper: std::sync::atomic::AtomicBool::default(),
        });
        runtime_fs::blobs::spawn_blob_server(
            std::os::unix::net::UnixListener::bind(&socket).unwrap(),
            Arc::clone(&observed) as Arc<dyn BlobBackend>,
            runtime.handle().clone(),
        )
        .unwrap();
        let client = launcher_connection(&socket, "room-1", "so.tilo.photos");
        let store = tilo_content::ContentStore::from_backend(Arc::new(
            tilo_content::DeviceBackend::new(client),
        ));
        Self {
            observed,
            store,
            _dir: dir,
            _runtime: runtime,
        }
    }

    fn reads(&self) -> Vec<u32> {
        self.observed.reads.lock().unwrap().clone()
    }

    /// Read `hash` to its end through the store, as an app does.
    fn read_through(&self, hash: &str) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut self.store.open(hash).unwrap(), &mut bytes)?;
        Ok(bytes)
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect()
}

#[test]
fn device_backend_streams_a_large_put_in_chunks() {
    let harness = DeviceHarness::start();
    let data = pattern(20 * 1024 * 1024);
    let stored = harness
        .store
        .put_reader(&mut data.as_slice(), "video/mp4")
        .unwrap();
    assert_eq!(stored.hash, blake3::hash(&data).to_hex().as_str());
    assert_eq!(stored.len, data.len() as u64);
    assert_eq!(
        harness
            .observed
            .puts
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the chunks were assembled into one stored item"
    );

    // Opening reads one range; the rest is fetched as the reader is consumed.
    let mut reader = harness.store.open(&stored.hash).unwrap();
    assert_eq!(harness.reads(), [MAX_READ_CHUNK]);
    let mut read = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut read).unwrap();
    assert_eq!(read, data);
    let ranges = data.len().div_ceil(MAX_READ_CHUNK as usize);
    assert_eq!(harness.reads(), vec![MAX_READ_CHUNK; ranges]);
}

#[test]
fn device_backend_rejects_oversize_without_buffering() {
    /// Zeros without end, counting how many were pulled.
    struct Endless {
        pulled: usize,
    }
    impl std::io::Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            buf.fill(0);
            self.pulled += buf.len();
            Ok(buf.len())
        }
    }

    let harness = DeviceHarness::start();
    let mut reader = Endless { pulled: 0 };
    assert!(matches!(
        harness.store.put_reader(&mut reader, ""),
        Err(tilo_content::ContentError::ItemTooLarge { .. })
    ));
    let limit = usize::try_from(MAX_BLOB_BYTES).unwrap();
    assert!(
        reader.pulled <= limit + runtime_fs::blobs::MAX_PUT_CHUNK,
        "pulled {} bytes for a {limit}-byte limit",
        reader.pulled
    );
    assert_eq!(
        harness
            .observed
            .puts
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing was stored"
    );
}

#[test]
fn device_backend_open_is_verified_at_eof() {
    let harness = DeviceHarness::start();
    let data = pattern(64 * 1024);
    let hash = harness.store.put(&data).unwrap();

    harness
        .observed
        .tamper
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        harness.read_through(&hash).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData,
        "other bytes than the address names fail at the end"
    );
    assert!(harness.store.read(&hash).is_none());

    // Nothing unverified was cached: the next read goes back to the daemon.
    harness
        .observed
        .tamper
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let before = harness.reads().len();
    assert_eq!(harness.read_through(&hash).unwrap(), data);
    assert!(harness.reads().len() > before);

    // A verified read is cached and served without the daemon.
    let before = harness.reads().len();
    assert_eq!(harness.read_through(&hash).unwrap(), data);
    assert_eq!(harness.reads().len(), before);
}

#[test]
fn device_backend_state_does_not_download() {
    let harness = DeviceHarness::start();
    let data = pattern(9 * 1024 * 1024);
    let hash = harness.store.put(&data).unwrap();
    assert_eq!(
        harness.store.state(&hash),
        tilo_content::ContentState::Resolved
    );
    let absent = blake3::hash(b"never stored").to_hex().to_string();
    assert_eq!(
        harness.store.state(&absent),
        tilo_content::ContentState::Unavailable
    );
    let reads = harness.reads();
    assert_eq!(reads.len(), 2);
    assert!(
        reads.iter().all(|max_len| *max_len == 0),
        "state() asked for content: {reads:?}"
    );
}
