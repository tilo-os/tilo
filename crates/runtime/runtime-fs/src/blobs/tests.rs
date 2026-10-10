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
    /// Every ranged read served, as `(offset, max_len)`.
    reads: Mutex<Vec<(u64, u32)>>,
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
            self.reads.lock().unwrap().push((offset, max_len));
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

#[test]
fn blob_reader_fetches_ranges_as_it_is_read() {
    let harness = Harness::start();
    let client = harness.app("room-1", "so.tilo.photos");
    let len = MAX_READ_CHUNK as usize * 2 + 3;
    let big: Vec<u8> = (0..len).map(pattern).collect();
    let stored = client.put(&big, "").unwrap();
    harness.backend.reads.lock().unwrap().clear();

    // phase: only_the_first_range_is_read_before_the_reader_is_consumed
    let mut reader = client.open_reader(&stored.hash).unwrap().unwrap();
    assert_eq!(reader.len(), len as u64);
    assert_eq!(
        *harness.backend.reads.lock().unwrap(),
        [(0, MAX_READ_CHUNK)]
    );
    let mut first = vec![0_u8; MAX_READ_CHUNK as usize];
    io::Read::read_exact(&mut reader, &mut first).unwrap();
    assert_eq!(harness.backend.reads.lock().unwrap().len(), 1);
    let mut rest = Vec::new();
    io::Read::read_to_end(&mut reader, &mut rest).unwrap();
    first.extend_from_slice(&rest);
    assert_eq!(first, big);
    assert_eq!(
        *harness.backend.reads.lock().unwrap(),
        [
            (0, MAX_READ_CHUNK),
            (u64::from(MAX_READ_CHUNK), MAX_READ_CHUNK),
            (u64::from(MAX_READ_CHUNK) * 2, MAX_READ_CHUNK),
        ]
    );

    // phase: pending_and_unavailable_content_opens_no_reader
    let pending = blake3::hash(b"on its way").to_hex().to_string();
    harness
        .backend
        .pending
        .lock()
        .unwrap()
        .insert(pending.clone());
    assert!(client.open_reader(&pending).unwrap().is_none());
    let absent = blake3::hash(b"never put").to_hex().to_string();
    assert!(client.open_reader(&absent).unwrap().is_none());

    // phase: read_all_verifies_and_refuses_other_bytes
    let small = client.put(b"twelve bytes", "").unwrap();
    harness
        .backend
        .bytes
        .lock()
        .unwrap()
        .insert(small.hash.clone(), b"twelve bytez".to_vec());
    assert_eq!(
        client.read_all(&small.hash),
        Err(BlobError::Refused("content does not match its hash".into()))
    );
}

type Shared<T> = Arc<Mutex<T>>;

/// A `DocumentBlobs` that keeps content in memory and logs every upload
/// write as it arrives: what the daemon sees of a streamed put.
struct Recording {
    content: Shared<HashMap<String, Vec<u8>>>,
    writes: Shared<Vec<usize>>,
}

impl Recording {
    fn keep(&self, bytes: Vec<u8>, mime: String) -> BlobRef {
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let len = bytes.len() as u64;
        self.content.lock().unwrap().insert(hash.clone(), bytes);
        BlobRef { hash, len, mime }
    }
}

fn settled(result: Result<(), capnp::Error>) -> Promise<(), capnp::Error> {
    match result {
        Ok(()) => Promise::ok(()),
        Err(error) => Promise::err(error),
    }
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl document_blobs::Server for Recording {
    fn put(
        self: Rc<Self>,
        params: document_blobs::PutParams,
        mut results: document_blobs::PutResults,
    ) -> Promise<(), capnp::Error> {
        settled((|| {
            let request = params.get()?;
            let stored = self.keep(
                request.get_bytes()?.to_vec(),
                text_param(request.get_mime())?,
            );
            write_ref(results.get().init_ref(), &stored);
            Ok(())
        })())
    }

    fn upload(
        self: Rc<Self>,
        params: document_blobs::UploadParams,
        mut results: document_blobs::UploadResults,
    ) -> Promise<(), capnp::Error> {
        settled((|| {
            let mime = text_param(params.get()?.get_mime())?;
            results
                .get()
                .set_upload(capnp_rpc::new_client(RecordingUpload {
                    store: Rc::clone(&self),
                    mime,
                    buffer: RefCell::new(Vec::new()),
                }));
            Ok(())
        })())
    }

    fn get(
        self: Rc<Self>,
        params: document_blobs::GetParams,
        mut results: document_blobs::GetResults,
    ) -> Promise<(), capnp::Error> {
        settled((|| {
            let request = params.get()?;
            let hash = text_param(request.get_hash())?;
            let mut read = results.get().init_read();
            let Some(bytes) = self.content.lock().unwrap().get(&hash).cloned() else {
                read.set_status(WireReadStatus::Unavailable);
                return Ok(());
            };
            let start = usize::try_from(request.get_offset())
                .unwrap()
                .min(bytes.len());
            let end = start
                .saturating_add(request.get_max_len() as usize)
                .min(bytes.len());
            read.set_status(WireReadStatus::Ready);
            read.set_bytes(&bytes[start..end]);
            read.set_total_len(bytes.len() as u64);
            Ok(())
        })())
    }
}

struct RecordingUpload {
    store: Rc<Recording>,
    mime: String,
    buffer: RefCell<Vec<u8>>,
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl blob_upload::Server for RecordingUpload {
    fn write(
        self: Rc<Self>,
        params: blob_upload::WriteParams,
        _results: blob_upload::WriteResults,
    ) -> Promise<(), capnp::Error> {
        settled((|| {
            let bytes = params.get()?.get_bytes()?;
            self.store.writes.lock().unwrap().push(bytes.len());
            self.buffer.borrow_mut().extend_from_slice(bytes);
            Ok(())
        })())
    }

    fn finish(
        self: Rc<Self>,
        _params: blob_upload::FinishParams,
        mut results: blob_upload::FinishResults,
    ) -> Promise<(), capnp::Error> {
        let bytes = std::mem::take(&mut *self.buffer.borrow_mut());
        let stored = self.store.keep(bytes, self.mime.clone());
        write_ref(results.get().init_ref(), &stored);
        Promise::ok(())
    }
}

/// An app connection to a [`Recording`] served on its own thread.
struct Recorded {
    client: BlobClient,
    /// The length of every upload write, in arrival order.
    writes: Shared<Vec<usize>>,
    content: Shared<HashMap<String, Vec<u8>>>,
}

fn recording_client() -> Recorded {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let content = Arc::new(Mutex::new(HashMap::new()));
    let recording = Recording {
        content: Arc::clone(&content),
        writes: Arc::clone(&writes),
    };
    let (app_end, server_end) = StdUnixStream::pair().unwrap();
    server_end.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async move {
            let (read_half, write_half) = tokio::net::UnixStream::from_std(server_end)
                .unwrap()
                .into_split();
            let network = twoparty::VatNetwork::new(
                read_half.compat(),
                write_half.compat_write(),
                rpc_twoparty_capnp::Side::Server,
                capnp::message::ReaderOptions::default(),
            );
            let bootstrap: document_blobs::Client = capnp_rpc::new_client(recording);
            // Ends when the app side closes.
            let _ = RpcSystem::new(Box::new(network), Some(bootstrap.client)).await;
        });
    });
    Recorded {
        client: BlobClient::from_app_stream(app_end).unwrap(),
        writes,
        content,
    }
}

fn pattern(index: usize) -> u8 {
    u8::try_from(index % 251).unwrap()
}

/// Yields `len` bytes of [`pattern`] and records how far it ran ahead of
/// what the store had received: the bytes a put was holding.
struct Watched {
    len: usize,
    at: usize,
    received: Shared<Vec<usize>>,
    most_held: usize,
    fail_at: Option<usize>,
}

impl Watched {
    fn new(len: usize, received: &Shared<Vec<usize>>) -> Self {
        Self {
            len,
            at: 0,
            received: Arc::clone(received),
            most_held: 0,
            fail_at: None,
        }
    }
}

impl io::Read for Watched {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.fail_at.is_some_and(|at| self.at >= at) {
            return Err(io::Error::other("the source went away"));
        }
        let n = buf.len().min(self.len.saturating_sub(self.at));
        for (offset, byte) in buf[..n].iter_mut().enumerate() {
            *byte = pattern(self.at.saturating_add(offset));
        }
        self.at = self.at.saturating_add(n);
        let received: usize = self.received.lock().unwrap().iter().sum();
        self.most_held = self.most_held.max(self.at.saturating_sub(received));
        Ok(n)
    }
}

#[test]
fn put_reader_streams_one_chunk_at_a_time_and_checks_the_reference() {
    // phase: content_larger_than_a_message_reaches_the_store_chunk_by_chunk
    {
        let Recorded { client, writes, .. } = recording_client();
        let len = MAX_PUT_CHUNK * 3 + 5;
        let mut reader = Watched::new(len, &writes);
        let stored = client
            .put_reader(&mut reader, "application/octet-stream")
            .unwrap();
        let content: Vec<u8> = (0..len).map(pattern).collect();
        assert_eq!(stored.hash, blake3::hash(&content).to_hex().as_str());
        assert_eq!(stored.len, len as u64);
        assert_eq!(
            *writes.lock().unwrap(),
            [MAX_PUT_CHUNK, MAX_PUT_CHUNK, MAX_PUT_CHUNK, 5],
            "the store saw one write per chunk"
        );
        assert!(
            reader.most_held <= MAX_PUT_CHUNK,
            "a put held {} bytes the store had not received",
            reader.most_held
        );
        assert_eq!(
            client.read_all(&stored.hash).unwrap(),
            ReadOutcome::Ready {
                bytes: content,
                total_len: len as u64,
            }
        );
    }
    // phase: content_of_one_message_or_less_is_one_put
    {
        let Recorded {
            client,
            writes,
            content: store,
        } = recording_client();
        let stored = client
            .put_reader(&mut Watched::new(MAX_PUT_CHUNK - 1, &writes), "")
            .unwrap();
        assert!(writes.lock().unwrap().is_empty());
        assert!(store.lock().unwrap().contains_key(&stored.hash));
        let empty = client.put_reader(&mut io::empty(), "").unwrap();
        assert_eq!(empty.hash, blake3::hash(b"").to_hex().as_str());
        let exact = client
            .put_reader(&mut Watched::new(MAX_PUT_CHUNK, &writes), "")
            .unwrap();
        assert_eq!(*writes.lock().unwrap(), [MAX_PUT_CHUNK]);
        assert_eq!(exact.len, MAX_PUT_CHUNK as u64);
    }
    // phase: oversized_content_is_refused_one_byte_past_the_limit
    {
        let Recorded {
            client,
            writes,
            content: store,
        } = recording_client();
        let limit = usize::try_from(MAX_BLOB_BYTES).unwrap();
        let mut reader = Watched::new(limit + MAX_PUT_CHUNK * 2, &writes);
        assert!(matches!(
            client.put_reader(&mut reader, ""),
            Err(BlobError::TooLarge { .. })
        ));
        assert_eq!(
            reader.at,
            limit + 1,
            "read one byte past the limit, no more"
        );
        assert!(reader.most_held <= MAX_PUT_CHUNK);
        assert!(store.lock().unwrap().is_empty(), "nothing was stored");
    }
    // phase: a_failing_source_stores_nothing
    {
        let Recorded {
            client,
            writes,
            content: store,
        } = recording_client();
        let mut reader = Watched::new(MAX_PUT_CHUNK * 3, &writes);
        reader.fail_at = Some(MAX_PUT_CHUNK + 7);
        assert!(matches!(
            client.put_reader(&mut reader, ""),
            Err(BlobError::Input(_))
        ));
        assert!(store.lock().unwrap().is_empty());
        // The connection stays usable after the abandoned upload.
        assert!(client.put(b"after", "").is_ok());
    }
}
