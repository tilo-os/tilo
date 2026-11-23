use runtime_fs::blobs::{BlobActor, BlobClient, MAX_READ_CHUNK, write_handoff};
use tilo_test_env::SpawnUnderEnvLock as _;

use super::*;

fn scope(workspace: &str, app: &str) -> BlobScope {
    BlobScope::new(workspace, app).unwrap()
}

#[test]
#[allow(clippy::too_many_lines, reason = "a journey is its steps")]
fn an_app_puts_media_and_every_caller_reads_only_what_it_may() {
    use std::io::{Seek, SeekFrom, Write};

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fs-blobs.sock");
    let store = {
        let _guard = runtime.enter();
        Arc::new(ContentStore::memory())
    };
    let _server = {
        let _guard = runtime.enter();
        serve_socket(&socket, DocumentMedia::new(Arc::clone(&store))).unwrap()
    };

    // The photos app keeps an image and a streamed video as this device's content; every
    // in-workspace app reads them by address.
    let photos = scope("workspace-1", "so.tilo.photos");
    let writer = launcher_connection(&socket, "workspace-1", "so.tilo.photos");
    let session = BlobClient::connect_session(&socket).unwrap();
    let image_hash = writer.put(IMAGE, "image/png").unwrap().hash;
    assert_eq!(image_hash, blake3::hash(IMAGE).to_hex().to_string());
    let video = pattern(3 * 1024 * 1024);
    let uploads = session.scope("workspace-1", "so.tilo.photos").unwrap();
    let mut upload = uploads.begin_upload("video/mp4").unwrap();
    for piece in video.chunks(512 * 1024 + 7) {
        upload.append(piece).unwrap();
    }
    let video_hash = upload.finish().unwrap().hash;
    for (hash, bytes) in [(&image_hash, IMAGE.to_vec()), (&video_hash, video.clone())] {
        let chat = launcher_connection(&socket, "workspace-1", "so.tilo.chat");
        assert_eq!(
            chat.read_all(hash).unwrap(),
            ReadOutcome::Ready {
                bytes: bytes.clone(),
                total_len: bytes.len() as u64,
            },
            "another app in the workspace reads it"
        );
        assert!(chat.has(hash).unwrap());
        assert_eq!(
            chat.read(hash, 7, 11).unwrap(),
            ReadOutcome::Ready {
                bytes: bytes[7..18].to_vec(),
                total_len: bytes.len() as u64,
            },
            "a range reads only its bytes"
        );
    }
    for (workspace, app) in [
        ("workspace-2", "so.tilo.photos"),
        ("workspace-2", "so.tilo.chat"),
    ] {
        let elsewhere = launcher_connection(&socket, workspace, app);
        assert_eq!(
            elsewhere.read_all(&image_hash).unwrap(),
            ReadOutcome::Unavailable
        );
        assert!(!elsewhere.has(&image_hash).unwrap(), "{workspace}/{app}");
    }
    assert!(matches!(
        session
            .scope("workspace-1", "so.tilo.photos")
            .unwrap()
            .read_all(&image_hash)
            .unwrap(),
        ReadOutcome::Ready { .. }
    ));

    // An upload abandoned before it finishes keeps nothing.
    let abandoned = b"never finished";
    let mut upload = uploads.begin_upload("application/octet-stream").unwrap();
    upload.append(abandoned).unwrap();
    drop(upload);
    let abandoned_hash = blake3::hash(abandoned).to_hex().to_string();
    assert!(!writer.has(&abandoned_hash).unwrap());
    assert_eq!(
        writer.read_all(&abandoned_hash).unwrap(),
        ReadOutcome::Unavailable
    );

    // The empty content is readable only where a workspace stored it.
    let empty = Hash::from(*blake3::hash(b"").as_bytes());
    for unreferenced in [
        scope("workspace-1", "so.tilo.photos"),
        scope("workspace-2", "so.tilo.photos"),
    ] {
        assert_eq!(
            runtime.block_on(DocumentMedia::new(Arc::clone(&store)).read_range(
                unreferenced,
                empty,
                0,
                u32::MAX
            )),
            ReadOutcome::Unavailable,
            "the empty content was read before anyone stored it"
        );
    }
    let stored = writer.put(&[], "application/octet-stream").unwrap();
    assert_eq!(
        stored.hash,
        empty.to_hex(),
        "photos stored the empty content"
    );
    assert!(writer.has(&empty.to_hex()).unwrap());

    // The largest attachment is refused whole, not buffered.
    let too_big = vec![0_u8; usize::try_from(MAX_BLOB_BYTES).unwrap() + 1];
    assert!(matches!(
        runtime.block_on(DocumentMedia::new(Arc::clone(&store)).store(&photos, too_big)),
        Err(BlobError::TooLarge { .. })
    ));

    // A durable store reads back after a restart, and only the ranges it reads are trusted.
    let durable = tempfile::tempdir().unwrap();
    let data = pattern(9 * 1024 * 1024 + 12_345);
    let media_hash = {
        let store = Arc::new(
            runtime
                .block_on(ContentStore::open(durable.path()))
                .unwrap(),
        );
        let media = DocumentMedia::new(Arc::clone(&store));
        let hash = runtime
            .block_on(media.store(&photos, data.clone()))
            .unwrap();
        runtime.block_on(store.shutdown()).unwrap();
        hash
    };
    let store = Arc::new(
        runtime
            .block_on(ContentStore::open(durable.path()))
            .unwrap(),
    );
    let media = DocumentMedia::new(Arc::clone(&store));
    assert!(!store.is_cached(media_hash));
    let total_len = data.len() as u64;
    for (offset, max_len) in [
        (0, 0),
        (16_380, 10),
        (4 * 1024 * 1024 - 7, MAX_READ_CHUNK),
        (total_len - 123, MAX_READ_CHUNK),
        (total_len, 10),
        (total_len + 1, 10),
        (u64::MAX, u32::MAX),
        (0, u32::MAX),
    ] {
        let start = usize::try_from(offset.min(total_len)).unwrap();
        let end = (start + max_len as usize).min(data.len());
        assert_eq!(
            runtime.block_on(media.read_range(photos.clone(), media_hash, offset, max_len)),
            ReadOutcome::Ready {
                bytes: data[start..end].to_vec(),
                total_len
            },
            "range {offset} + {max_len}"
        );
        assert!(
            !store.is_cached(media_hash),
            "range {offset} + {max_len} cached the blob"
        );
    }
    assert_eq!(
        runtime.block_on(media.read_range(
            scope("workspace-2", "so.tilo.photos"),
            media_hash,
            0,
            1
        )),
        ReadOutcome::Unavailable
    );
    assert_eq!(
        runtime.block_on(store.local(media_hash)).unwrap().as_ref(),
        data
    );
    assert!(!store.is_cached(media_hash));
    runtime.block_on(store.shutdown()).unwrap();

    // A range read verifies the groups it reads: the good byte before the damage still reads,
    // the damaged one does not, and nothing claims the blob on those reads.
    let damaged = data.len() - 100;
    let path = durable.path().join("files/data").join(media_hash.to_hex());
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(damaged as u64)).unwrap();
    file.write_all(&[data[damaged] ^ 0xff]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let store = Arc::new(
        runtime
            .block_on(ContentStore::open(durable.path()))
            .unwrap(),
    );
    let media = DocumentMedia::new(Arc::clone(&store));
    assert_eq!(
        runtime.block_on(media.read_range(photos.clone(), media_hash, 0, 10)),
        ReadOutcome::Ready {
            bytes: data[..10].to_vec(),
            total_len: data.len() as u64
        }
    );
    assert_eq!(
        runtime.block_on(media.read_range(photos.clone(), media_hash, damaged as u64, 1)),
        ReadOutcome::Unavailable
    );
    assert_eq!(
        runtime.block_on(media.read_range(photos.clone(), media_hash, 0, 0)),
        ReadOutcome::Ready {
            bytes: Vec::new(),
            total_len: data.len() as u64
        }
    );
    assert!(!store.is_cached(media_hash));
    runtime.block_on(store.shutdown()).unwrap();

    // A chunk of a file-backed blob is read from its entry in the file store, and a damage to
    // that entry is refused.
    let inner = Arc::new(ContentStore::memory());
    let data = pattern(3 * 1024 * 1024);
    let content = runtime
        .block_on(inner.write_content(&data))
        .unwrap()
        .unwrap();
    let manifest = runtime.block_on(inner.manifest(content)).unwrap();
    let chunk = manifest.chunks()[0];
    let hash = Hash::from(chunk.hash);
    assert!(
        inner.files().get_inline(hash.as_bytes()).is_none(),
        "the chunk lives in the file-backed entry, not inline"
    );
    runtime
        .block_on(document_reference(&inner, &photos, hash))
        .unwrap();
    let media = DocumentMedia::new(Arc::clone(&inner));
    assert_eq!(
        runtime.block_on(media.read_range(photos.clone(), hash, 7, 11)),
        ReadOutcome::Ready {
            bytes: data[7..18].to_vec(),
            total_len: u64::from(chunk.len)
        }
    );
    assert!(!inner.is_cached(hash));
    inner
        .files()
        .write_at(content.as_bytes(), 7, &[data[7] ^ 0xff])
        .unwrap();
    assert_eq!(
        runtime.block_on(media.read_range(photos, hash, 7, 11)),
        ReadOutcome::Unavailable
    );
}

const PROOF_SOCKET: &str = "TILO_MEDIA_PROOF_SOCKET";
const PROOF_HASH: &str = "TILO_MEDIA_PROOF_HASH";
const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\n pixels put by one application";

fn launcher_connection(socket: &Path, workspace: &str, app: &str) -> BlobClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    write_handoff(&mut stream, &BlobActor::App(scope(workspace, app))).unwrap();
    BlobClient::from_app_stream(stream).unwrap()
}

#[test]
fn an_image_put_by_one_process_is_read_by_another_and_the_session_only() {
    if let (Some(socket), Some(hash)) = (
        std::env::var_os(PROOF_SOCKET),
        std::env::var(PROOF_HASH).ok(),
    ) {
        let reader = launcher_connection(Path::new(&socket), "workspace-1", "so.tilo.photos");
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

    let writer = launcher_connection(&socket, "workspace-1", "so.tilo.photos");
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
        .output_locked()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the second process could not read the image: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the child ran no test: {stdout}"
    );

    let session = BlobClient::connect_session(&socket).unwrap();
    let shell_view = session.scope("workspace-1", "so.tilo.photos").unwrap();
    assert!(matches!(
        shell_view.read_all(&stored.hash).unwrap(),
        ReadOutcome::Ready { .. }
    ));

    let stranger = launcher_connection(&socket, "workspace-2", "so.tilo.chat");
    assert_eq!(
        stranger.read_all(&stored.hash).unwrap(),
        ReadOutcome::Unavailable
    );
    assert!(!stranger.has(&stored.hash).unwrap());
    let elsewhere = session.scope("workspace-2", "so.tilo.photos").unwrap();
    assert_eq!(
        elsewhere.read_all(&stored.hash).unwrap(),
        ReadOutcome::Unavailable
    );
}

#[test]
fn content_put_through_one_workspaces_scope_is_unavailable_through_another_workspaces() {
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
    let session = BlobClient::connect_session(&socket).unwrap();
    let content = pattern(runtime_fs::blobs::MAX_PUT_CHUNK + 4321);
    let mut upload = session
        .scope("workspace-a", "tilo.content")
        .unwrap()
        .begin_upload("application/octet-stream")
        .unwrap();
    for piece in content.chunks(512 * 1024 + 7) {
        upload.append(piece).unwrap();
    }
    assert_eq!(upload.len(), content.len() as u64);
    let stored = upload.finish().unwrap();
    assert_eq!(stored.hash, blake3::hash(&content).to_hex().to_string());
    assert_eq!(stored.len, content.len() as u64);

    let notes = launcher_connection(&socket, "workspace-a", "so.tilo.notes");
    assert_eq!(
        notes.read_all(&stored.hash).unwrap(),
        ReadOutcome::Ready {
            bytes: content.clone(),
            total_len: content.len() as u64,
        }
    );
    for (workspace, app) in [
        ("workspace-b", "tilo.content"),
        ("workspace-b", "so.tilo.notes"),
    ] {
        let elsewhere = launcher_connection(&socket, workspace, app);
        assert_eq!(
            elsewhere.read_all(&stored.hash).unwrap(),
            ReadOutcome::Unavailable,
            "{workspace}/{app}"
        );
        assert!(!elsewhere.has(&stored.hash).unwrap(), "{workspace}/{app}");
    }

    let abandoned = b"never finished";
    let mut upload = session
        .scope("workspace-a", "tilo.content")
        .unwrap()
        .begin_upload("application/octet-stream")
        .unwrap();
    upload.append(abandoned).unwrap();
    drop(upload);
    let hash = blake3::hash(abandoned).to_hex().to_string();
    assert!(!notes.has(&hash).unwrap());
    assert_eq!(notes.read_all(&hash).unwrap(), ReadOutcome::Unavailable);
}

struct OneVersion(Vec<u8>, Arc<std::sync::atomic::AtomicUsize>);

impl crate::public_content::Origin for OneVersion {
    fn fetch(
        &self,
        _url: &'static str,
        since: Option<crate::public_content::Validator>,
        _limit: u64,
    ) -> runtime_fs::blobs::BlobFuture<Result<crate::public_content::Answer, String>> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let validator = crate::public_content::Validator {
            etag: Some("\"v1\"".into()),
            last_modified: None,
        };
        let answer = if since.as_ref() == Some(&validator) {
            crate::public_content::Answer::Unchanged
        } else {
            crate::public_content::Answer::Changed {
                bytes: self.0.clone(),
                validator,
            }
        };
        Box::pin(async move { Ok(answer) })
    }
}

#[test]
fn a_public_copy_is_one_per_device_for_apps_in_every_workspace() {
    const NAME: &str = "gutenberg-catalogue";
    let catalogue = b"Text#,Type,Title\n1,Text,The Declaration of Independence\n".to_vec();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fs-blobs.sock");
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _guard = runtime.enter();
    let store = Arc::new(ContentStore::memory());
    let _server = {
        let public = PublicContent::open(
            &dir.path().join("public-content"),
            Arc::clone(store.files()),
            Arc::new(OneVersion(catalogue.clone(), Arc::clone(&asked))),
        )
        .unwrap();
        serve_socket(
            &socket,
            DocumentMedia::with_public(Arc::clone(&store), public),
        )
        .unwrap()
    };
    let asked = || asked.load(std::sync::atomic::Ordering::SeqCst);

    let books = launcher_connection(&socket, "workspace-1", "so.tilo.ebook");
    let search = launcher_connection(&socket, "workspace-2", "so.tilo.search");
    assert_eq!(
        books.public_read_all(NAME).unwrap(),
        Err(ReadOutcome::Unavailable),
        "nobody keeps it yet"
    );
    assert!(books.public_keep("https://example.org/anything").is_err());

    books.public_keep(NAME).unwrap();
    let (bytes, hash) = arrived(&books, NAME);
    assert_eq!(bytes, catalogue);
    assert_eq!(hash, blake3::hash(&catalogue).to_hex().to_string());

    search.public_keep(NAME).unwrap();
    assert_eq!(
        search.public_read_all(NAME).unwrap(),
        Ok((catalogue.clone(), hash.clone())),
        "another workspace reads the same copy"
    );

    let session = BlobClient::connect_session(&socket).unwrap();
    assert_eq!(session.refresh_public().unwrap(), 0, "fetched just now");
    assert!(
        books.refresh_public().is_err(),
        "only the session refreshes"
    );

    books.public_release(NAME).unwrap();
    search.public_release(NAME).unwrap();
    assert_eq!(
        books.public_read_all(NAME).unwrap(),
        Ok((catalogue.clone(), hash.clone())),
        "released, it is still here: reopening Books downloads nothing"
    );
    assert_eq!(asked(), 1);

    store.files().set_budget(0);
    assert_eq!(store.files().evict_to_budget(&[]), catalogue.len() as u64);
    store.files().set_budget(u64::MAX);
    assert_eq!(arrived(&books, NAME), (catalogue, hash));
    assert_eq!(asked(), 2, "evicted, it is fetched again on the next read");
}

fn arrived(client: &BlobClient, name: &str) -> (Vec<u8>, String) {
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(10))
        .unwrap();
    loop {
        match client.public_read_all(name).unwrap() {
            Ok(read) => return read,
            Err(outcome) => {
                assert_eq!(
                    outcome,
                    ReadOutcome::Pending,
                    "a kept copy is here or on its way"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "the copy never arrived"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

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

    fn put_stream(
        self: Arc<Self>,
        scope: BlobScope,
        chunks: runtime_fs::blobs::BlobChunks,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        let stored = Arc::clone(&self.media).put_stream(scope, chunks, mime);
        Box::pin(async move {
            let stored = stored.await;
            if stored.is_ok() {
                self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            stored
        })
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

struct DeviceHarness {
    observed: Arc<Observed>,
    daemon_store: Arc<ContentStore>,
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
        let daemon_store = {
            let _guard = runtime.enter();
            Arc::new(ContentStore::memory())
        };
        let media = DocumentMedia::new(Arc::clone(&daemon_store));
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
        let client = launcher_connection(&socket, "workspace-1", "so.tilo.photos");
        let store = tilo_content::ContentStore::from_backend(Arc::new(
            tilo_content_device::DeviceBackend::new(client),
        ));
        Self {
            observed,
            daemon_store,
            store,
            _dir: dir,
            _runtime: runtime,
        }
    }

    fn reads(&self) -> Vec<u32> {
        self.observed.reads.lock().unwrap().clone()
    }

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

    let hash = stored.hash.parse().unwrap();
    harness.daemon_store.forget_cached(hash);
    let mut reader = harness.store.open(&stored.hash).unwrap();
    assert_eq!(harness.reads(), [MAX_READ_CHUNK]);
    let mut read = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut read).unwrap();
    assert_eq!(read, data);
    let ranges = data.len().div_ceil(MAX_READ_CHUNK as usize);
    assert_eq!(harness.reads(), vec![MAX_READ_CHUNK; ranges]);
    assert!(!harness.daemon_store.is_cached(hash));
}

#[test]
fn device_backend_rejects_oversize_without_buffering() {
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

tilo_test_env::scenario! {
    fn the_device_backend_verifies_reads_and_answers_state_without_downloading;

    phase device_backend_open_is_verified_at_eof;
    {
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

        harness
            .observed
            .tamper
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let before = harness.reads().len();
        assert_eq!(harness.read_through(&hash).unwrap(), data);
        assert!(harness.reads().len() > before);

        let before = harness.reads().len();
        assert_eq!(harness.read_through(&hash).unwrap(), data);
        assert_eq!(harness.reads().len(), before);
    }

    phase device_backend_state_does_not_download;
    {
        let harness = DeviceHarness::start();
        let data = pattern(9 * 1024 * 1024);
        let hash = harness.store.put(&data).unwrap();
        let daemon_hash = hash.parse().unwrap();
        harness.daemon_store.forget_cached(daemon_hash);
        assert_eq!(
            harness.store.state(&hash),
            tilo_content::ContentState::Resolved
        );
        assert!(!harness.daemon_store.is_cached(daemon_hash));
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
}
