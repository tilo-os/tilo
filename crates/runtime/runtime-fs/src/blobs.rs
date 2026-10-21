//! Document media: the app-facing door to the device content store.
//!
//! A document keeps a [`BlobRef`] and never the bytes. The bytes live in the
//! filesystem daemon's content store, the same store that holds workspace
//! files, and an app reaches them only through a [`DocumentBlobs`] capability
//! scoped to `(room, app)`:
//!
//! - The launcher connects to the daemon's socket on the app's behalf and
//!   writes a handoff frame naming the app and its room before the app ever
//!   holds the descriptor ([`write_handoff`]). The socket itself is outside
//!   every sandbox.
//! - An app reads document media of its room: content put there on this
//!   device, and content a collaborator put there, which the daemon fetches
//!   from that collaborator's device on first read. This is what lets one app
//!   hand content to another in the same room by hash. Anything else (files,
//!   other rooms) reads exactly like content that does not exist: possessing a
//!   hash never grants access.
//! - The desktop session connects with a [`BlobPrincipal::Session`] handoff
//!   and receives [`SessionBlobs`], which only hands out app scopes. The shell
//!   therefore reads content an app asked it to open *through that app's
//!   scope*, never around it.
//!
//! [`BlobClient`] is the synchronous client apps and the shell use; it owns a
//! small thread that drives the Cap'n Proto connection. [`spawn_blob_server`]
//! is the server glue the daemon runs over a [`BlobBackend`].
//!
//! [`DocumentBlobs`]: document_blobs::Client
//! [`SessionBlobs`]: session_blobs::Client

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Read as _};
use std::os::fd::{IntoRawFd, RawFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use capnp::capability::Promise;
use capnp_rpc::rpc_twoparty_capnp;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;
use wire_frame::Framing;

pub use crate::blobs_capnp::{
    ReadStatus as WireReadStatus, blob_upload, document_blobs, session_blobs,
};

/// Where the root filesystem daemon listens for document-media connections.
/// Group-connectable by the signed-in device user only, and never bound into
/// an app sandbox.
pub const DEFAULT_BLOB_SOCKET: &str = "/run/tilo/fs-blobs.sock";
/// Launcher service name; the app finds its descriptor in `TILO_SVC_BLOBS_FD`.
pub const BLOB_SERVICE: &str = "blobs";
/// Prefix of the handoff frame that precedes Cap'n Proto on every connection.
pub const BLOB_HANDOFF_MAGIC: [u8; 4] = *b"QBL1";
pub const BLOB_HANDOFF_PROTOCOL: u16 = 1;
/// Largest payload one `put` message carries; larger content uses `upload`.
pub const MAX_PUT_CHUNK: usize = 4 * 1024 * 1024;
/// Largest range one `get` answers.
pub const MAX_READ_CHUNK: u32 = 4 * 1024 * 1024;
/// Largest single blob the capability accepts or reassembles. Document media
/// is images and attachments; bigger content belongs in a file record.
pub const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;
/// Largest piece of an upload handed to the backend at once. An upload's
/// `write` carries up to [`MAX_PUT_CHUNK`]; the server passes it on in pieces
/// of this size so that what waits between the connection and the store is
/// bounded by pieces, not by whole messages.
///
/// The bound is in pieces at every hop: [`UPLOAD_PIECES_IN_FLIGHT`] here, and
/// the content store queues up to 32 more on their way to its writer
/// (iroh-blobs' import channel). How full that queue runs depends on how far
/// the disk lags the connection, so the piece size is what keeps the worst
/// case small: 64 KiB pieces put at most 2.25 MiB between the connection and
/// the disk, where 256 KiB pieces let a slow writer hold 9 MiB.
pub const UPLOAD_PIECE: usize = 64 * 1024;
/// Pieces of one upload that may wait for the backend before the uploader's
/// next `write` is held back.
pub const UPLOAD_PIECES_IN_FLIGHT: usize = 4;
const HANDOFF_FRAMING: Framing = Framing::little_endian(4096).reject_empty();
const MAX_SCOPE_COMPONENT: usize = 128;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Who a connection acts for, fixed by the handoff frame before the first RPC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobPrincipal {
    /// A sandboxed app in a room: the connection's bootstrap is that scope's
    /// `DocumentBlobs`.
    App(BlobScope),
    /// The desktop session: the bootstrap is `SessionBlobs`.
    Session,
}

/// One app's document media in one room.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobScope {
    room: String,
    app: String,
}

impl BlobScope {
    /// Validate and build a scope. Both parts must be a single path-safe
    /// component (`[A-Za-z0-9._-]`, not only dots, at most 128 bytes): they
    /// key the daemon's index and must not be able to alias one another.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidScope`] for anything else.
    pub fn new(room: impl Into<String>, app: impl Into<String>) -> Result<Self, BlobError> {
        let room = room.into();
        let app = app.into();
        if valid_scope_component(&room) && valid_scope_component(&app) {
            Ok(Self { room, app })
        } else {
            Err(BlobError::InvalidScope)
        }
    }

    pub fn room(&self) -> &str {
        &self.room
    }

    pub fn app(&self) -> &str {
        &self.app
    }
}

fn valid_scope_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCOPE_COMPONENT
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !value.bytes().all(|byte| byte == b'.')
}

/// True for a lowercase 64-character hex BLAKE3 digest, the only hash form
/// the capability accepts.
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A stored blob, as a document records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRef {
    pub hash: String,
    pub len: u64,
    pub mime: String,
}

/// The answer to a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    Ready {
        bytes: Vec<u8>,
        total_len: u64,
    },
    /// This device is fetching the content from a collaborator; ask again.
    Pending,
    /// Not readable through this capability, now or later.
    Unavailable,
}

/// Why a blob operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobError {
    InvalidHash,
    InvalidScope,
    TooLarge {
        len: u64,
        limit: u64,
    },
    /// The service answered and refused.
    Refused(String),
    /// No service, or the connection is gone.
    Disconnected(String),
    /// The caller's own reader failed while its content was being stored.
    Input(String),
}

impl std::fmt::Display for BlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHash => f.write_str("blob hash must be 64 lowercase hex characters"),
            Self::InvalidScope => f.write_str("blob scope names an invalid room or app"),
            Self::TooLarge { len, limit } => {
                write!(f, "blob is {len} bytes; the limit is {limit} bytes")
            }
            Self::Refused(message) => write!(f, "blob service refused: {message}"),
            Self::Disconnected(message) => write!(f, "blob service unavailable: {message}"),
            Self::Input(message) => write!(f, "reading the content to store failed: {message}"),
        }
    }
}

impl std::error::Error for BlobError {}

impl From<capnp::Error> for BlobError {
    fn from(error: capnp::Error) -> Self {
        match error.kind {
            capnp::ErrorKind::Disconnected => Self::Disconnected(error.to_string()),
            _ => Self::Refused(error.extra),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffFrame {
    protocol: u16,
    principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
}

/// Write the handoff that fixes a new connection's principal: the magic, a
/// little-endian `u32` length and a JSON object
/// `{"protocol":1,"principal":"app","room":…,"app_id":…}` or
/// `{"protocol":1,"principal":"session"}`. The launcher writes the same bytes.
///
/// # Errors
///
/// Returns an I/O error when the frame cannot be written.
pub fn write_handoff(stream: &mut impl io::Write, principal: &BlobPrincipal) -> io::Result<()> {
    let frame = match principal {
        BlobPrincipal::App(scope) => HandoffFrame {
            protocol: BLOB_HANDOFF_PROTOCOL,
            principal: "app".into(),
            room: Some(scope.room.clone()),
            app_id: Some(scope.app.clone()),
        },
        BlobPrincipal::Session => HandoffFrame {
            protocol: BLOB_HANDOFF_PROTOCOL,
            principal: "session".into(),
            room: None,
            app_id: None,
        },
    };
    let bytes = serde_json::to_vec(&frame).map_err(io::Error::other)?;
    stream.write_all(&BLOB_HANDOFF_MAGIC)?;
    HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

/// Read and validate a handoff frame (server side).
///
/// # Errors
///
/// Returns an I/O error for a truncated, oversized, unknown or invalid frame.
pub async fn read_handoff<R>(reader: &mut R) -> io::Result<BlobPrincipal>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_owned());
    let mut magic = [0_u8; 4];
    reader.read_exact(&mut magic).await?;
    if magic != BLOB_HANDOFF_MAGIC {
        return Err(invalid("not a document-media handoff"));
    }
    let bytes = HANDOFF_FRAMING
        .read_async(reader)
        .await?
        .ok_or(io::ErrorKind::UnexpectedEof)?;
    let frame: HandoffFrame =
        serde_json::from_slice(&bytes).map_err(|_| invalid("malformed document-media handoff"))?;
    if frame.protocol != BLOB_HANDOFF_PROTOCOL {
        return Err(invalid("unsupported document-media handoff protocol"));
    }
    match (frame.principal.as_str(), frame.room, frame.app_id) {
        ("app", Some(room), Some(app)) => BlobScope::new(room, app)
            .map(BlobPrincipal::App)
            .map_err(|_| invalid("document-media handoff names an invalid scope")),
        ("session", None, None) => Ok(BlobPrincipal::Session),
        _ => Err(invalid("document-media handoff names an unknown principal")),
    }
}

/// The pieces of one streamed upload, in order, as the uploader writes them
/// ([`BlobBackend::put_stream`]). The stream ends (`None`) only once the
/// uploader finished; if the upload is dropped first, or a write is refused,
/// the next piece is an error, so a backend that stops at the first error
/// stores nothing of it.
pub struct BlobChunks {
    pieces: tokio::sync::mpsc::Receiver<Option<Vec<u8>>>,
    ended: bool,
}

/// The writing end of [`BlobChunks`].
#[derive(Clone)]
pub struct BlobChunkSender {
    pieces: tokio::sync::mpsc::Sender<Option<Vec<u8>>>,
}

impl BlobChunks {
    /// A connected pair holding at most `in_flight` pieces between the two.
    pub fn channel(in_flight: usize) -> (BlobChunkSender, Self) {
        let (sender, pieces) = tokio::sync::mpsc::channel(in_flight.max(1));
        (
            BlobChunkSender { pieces: sender },
            Self {
                pieces,
                ended: false,
            },
        )
    }

    /// The next piece, `None` at the uploader's end, or an error when the
    /// upload ended without finishing.
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<io::Result<Vec<u8>>>> {
        if self.ended {
            return Poll::Ready(None);
        }
        let Poll::Ready(piece) = self.pieces.poll_recv(cx) else {
            return Poll::Pending;
        };
        Poll::Ready(match piece {
            Some(Some(piece)) => Some(Ok(piece)),
            Some(None) => {
                self.ended = true;
                None
            }
            None => {
                self.ended = true;
                Some(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the upload ended before it finished",
                )))
            }
        })
    }

    /// [`Self::poll_next`] as a future.
    pub async fn next(&mut self) -> Option<io::Result<Vec<u8>>> {
        std::future::poll_fn(|cx| self.poll_next(cx)).await
    }
}

impl BlobChunkSender {
    /// Hand on one piece, waiting while [`BlobChunks::channel`]'s pieces are
    /// all in flight.
    ///
    /// # Errors
    ///
    /// Returns a refusal when the backend stopped reading.
    pub async fn send(&self, piece: Vec<u8>) -> Result<(), BlobError> {
        self.pieces
            .send(Some(piece))
            .await
            .map_err(|_| BlobError::Refused("the store stopped reading the upload".into()))
    }

    /// End the content cleanly; dropping every sender without this abandons it.
    ///
    /// # Errors
    ///
    /// Returns a refusal when the backend stopped reading.
    pub async fn finish(self) -> Result<(), BlobError> {
        self.pieces
            .send(None)
            .await
            .map_err(|_| BlobError::Refused("the store stopped reading the upload".into()))
    }
}

/// A boxed `Send` future, the shape [`BlobBackend`] answers in.
pub type BlobFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The store behind the capability. Every call carries the scope the
/// connection was authenticated for; the backend decides reachability.
pub trait BlobBackend: Send + Sync + 'static {
    fn put(
        self: Arc<Self>,
        scope: BlobScope,
        bytes: Vec<u8>,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>>;
    /// Store content that arrives in pieces, as an upload writes it. The
    /// backend consumes `chunks` as they come and must not hold all of them:
    /// that is what keeps a 64 MiB upload from costing 64 MiB of heap. A
    /// piece that is an error means the upload was abandoned or refused, and
    /// nothing may be stored.
    fn put_stream(
        self: Arc<Self>,
        scope: BlobScope,
        chunks: BlobChunks,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>>;
    fn read(
        self: Arc<Self>,
        scope: BlobScope,
        hash: String,
        offset: u64,
        max_len: u32,
    ) -> BlobFuture<ReadOutcome>;
    fn has(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<bool>;
    fn size(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<Option<u64>>;
}

/// Serve `listener` on a dedicated thread. Cap'n Proto servers are `!Send`,
/// so connections are driven by a single-threaded `LocalSet`; backend work
/// runs on `runtime`, where the content store and transport live.
///
/// # Errors
///
/// Returns an I/O error if the thread cannot be started.
pub fn spawn_blob_server(
    listener: std::os::unix::net::UnixListener,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
) -> io::Result<std::thread::JoinHandle<()>> {
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("fs-blobs".into())
        .spawn(move || {
            let local_runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("document media server could not start: {error}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&local_runtime, async move {
                let listener = match tokio::net::UnixListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        eprintln!("document media socket unusable: {error}");
                        return;
                    }
                };
                loop {
                    let stream = match listener.accept().await {
                        Ok((stream, _)) => stream,
                        Err(error) => {
                            eprintln!("document media accept failed: {error}");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    };
                    let backend = Arc::clone(&backend);
                    let runtime = runtime.clone();
                    tokio::task::spawn_local(async move {
                        if let Err(error) = serve_connection(stream, backend, runtime).await {
                            eprintln!("document media connection closed: {error}");
                        }
                    });
                }
            });
        })
}

/// Serve one already-accepted connection: read its handoff, then answer
/// Cap'n Proto until the peer goes away. Must run inside a `LocalSet`.
///
/// # Errors
///
/// Returns an error for a missing or invalid handoff, or a failed RPC system.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc servers are Rc-based and run on a LocalSet by construction"
)]
pub async fn serve_connection(
    mut stream: tokio::net::UnixStream,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
) -> io::Result<()> {
    let principal = tokio::time::timeout(HANDOFF_TIMEOUT, read_handoff(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handoff timed out"))??;
    let bootstrap: capnp::capability::Client = match principal {
        BlobPrincipal::App(scope) => {
            let client: document_blobs::Client = capnp_rpc::new_client(ScopedBlobs {
                scope,
                backend,
                runtime,
            });
            client.client
        }
        BlobPrincipal::Session => {
            let client: session_blobs::Client =
                capnp_rpc::new_client(SessionServer { backend, runtime });
            client.client
        }
    };
    runtime_portal::serve_connected_io(stream, bootstrap)
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn on_runtime<T: Send + 'static>(
    runtime: &tokio::runtime::Handle,
    future: BlobFuture<T>,
) -> Result<T, capnp::Error> {
    runtime
        .spawn(future)
        .await
        .map_err(|error| capnp::Error::failed(format!("document media backend failed: {error}")))
}

fn text_param(value: capnp::Result<capnp::text::Reader<'_>>) -> Result<String, capnp::Error> {
    value?
        .to_str()
        .map(str::to_owned)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn checked_hash(value: capnp::Result<capnp::text::Reader<'_>>) -> Result<String, capnp::Error> {
    let hash = text_param(value)?;
    if valid_hash(&hash) {
        Ok(hash)
    } else {
        Err(capnp::Error::failed(BlobError::InvalidHash.to_string()))
    }
}

fn refuse(error: &BlobError) -> capnp::Error {
    capnp::Error::failed(error.to_string())
}

struct ScopedBlobs {
    scope: BlobScope,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl document_blobs::Server for ScopedBlobs {
    fn put(
        self: Rc<Self>,
        params: document_blobs::PutParams,
        mut results: document_blobs::PutResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let request = params.get()?;
            let bytes = request.get_bytes()?;
            if bytes.len() > MAX_PUT_CHUNK {
                return Err(refuse(&BlobError::TooLarge {
                    len: bytes.len() as u64,
                    limit: MAX_PUT_CHUNK as u64,
                }));
            }
            let bytes = bytes.to_vec();
            let mime = text_param(request.get_mime())?;
            let stored = on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).put(self.scope.clone(), bytes, mime),
            )
            .await?
            .map_err(|error| refuse(&error))?;
            write_ref(results.get().init_ref(), &stored);
            Ok(())
        })
    }

    fn upload(
        self: Rc<Self>,
        params: document_blobs::UploadParams,
        mut results: document_blobs::UploadResults,
    ) -> Promise<(), capnp::Error> {
        let mime = match params
            .get()
            .and_then(|request| text_param(request.get_mime()))
        {
            Ok(mime) => mime,
            Err(error) => return Promise::err(error),
        };
        // The backend consumes the upload as it is written, so the store
        // hashes and imports it piece by piece and nothing here ever holds
        // the whole payload.
        let (pieces, chunks) = BlobChunks::channel(UPLOAD_PIECES_IN_FLIGHT);
        let stored = self.runtime.spawn(Arc::clone(&self.backend).put_stream(
            self.scope.clone(),
            chunks,
            mime,
        ));
        results.get().set_upload(capnp_rpc::new_client(Upload {
            pieces: RefCell::new(Some(pieces)),
            stored: RefCell::new(Some(stored)),
            len: Cell::new(0),
            writing: Rc::new(Cell::new(false)),
        }));
        Promise::ok(())
    }

    fn get(
        self: Rc<Self>,
        params: document_blobs::GetParams,
        mut results: document_blobs::GetResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let request = params.get()?;
            let hash = checked_hash(request.get_hash())?;
            let offset = request.get_offset();
            let max_len = request.get_max_len().min(MAX_READ_CHUNK);
            let outcome = on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).read(self.scope.clone(), hash, offset, max_len),
            )
            .await?;
            let mut read = results.get().init_read();
            match outcome {
                ReadOutcome::Ready { bytes, total_len } => {
                    read.set_status(WireReadStatus::Ready);
                    read.set_bytes(&bytes);
                    read.set_total_len(total_len);
                }
                ReadOutcome::Pending => read.set_status(WireReadStatus::Pending),
                ReadOutcome::Unavailable => read.set_status(WireReadStatus::Unavailable),
            }
            Ok(())
        })
    }

    fn has(
        self: Rc<Self>,
        params: document_blobs::HasParams,
        mut results: document_blobs::HasResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let hash = checked_hash(params.get()?.get_hash())?;
            let present = on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).has(self.scope.clone(), hash),
            )
            .await?;
            results.get().set_present(present);
            Ok(())
        })
    }

    fn size(
        self: Rc<Self>,
        params: document_blobs::SizeParams,
        mut results: document_blobs::SizeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let hash = checked_hash(params.get()?.get_hash())?;
            let size = on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).size(self.scope.clone(), hash),
            )
            .await?;
            let mut reply = results.get();
            reply.set_known(size.is_some());
            reply.set_len(size.unwrap_or(0));
            Ok(())
        })
    }
}

fn write_ref(mut target: crate::blobs_capnp::blob_ref::Builder<'_>, stored: &BlobRef) {
    target.set_hash(stored.hash.as_str());
    target.set_len(stored.len);
    target.set_mime(stored.mime.as_str());
}

/// One upload in progress. Each `write` is passed on to the backend in
/// [`UPLOAD_PIECE`]s as soon as it arrives, and is answered once the backend
/// has taken all of it, which is what holds the uploader back. Writes are
/// taken one at a time, as [`BlobClient::put_reader`] sends them; a write
/// pipelined behind another is refused, so what waits here stays bounded by
/// one message whatever the peer sends.
struct Upload {
    /// The backend's end of the stream; `None` once finished or refused.
    pieces: RefCell<Option<BlobChunkSender>>,
    /// The backend storing the stream; it answers once the stream ends.
    stored: RefCell<Option<tokio::task::JoinHandle<Result<BlobRef, BlobError>>>>,
    /// Bytes written so far.
    len: Cell<u64>,
    /// A write is being passed on.
    writing: Rc<Cell<bool>>,
}

impl Upload {
    /// Stop the upload: the backend sees it end without finishing and
    /// stores nothing.
    fn refuse(&self, error: &BlobError) -> capnp::Error {
        self.pieces.borrow_mut().take();
        refuse(error)
    }
}

/// Clears [`Upload::writing`] however the write it guards ends.
struct Writing(Rc<Cell<bool>>);

impl Drop for Writing {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl blob_upload::Server for Upload {
    fn write(
        self: Rc<Self>,
        params: blob_upload::WriteParams,
        _results: blob_upload::WriteResults,
    ) -> Promise<(), capnp::Error> {
        let Some(pieces) = self.pieces.borrow().clone() else {
            return Promise::err(capnp::Error::failed("upload already finished".into()));
        };
        if self.writing.get() {
            return Promise::err(self.refuse(&BlobError::Refused(
                "an upload takes one write at a time".into(),
            )));
        }
        let written = match params
            .get()
            .and_then(crate::blobs_capnp::blob_upload::write_params::Reader::get_bytes)
        {
            Ok(bytes) => bytes.len(),
            Err(error) => return Promise::err(error),
        };
        let len = self.len.get().saturating_add(written as u64);
        if written > MAX_PUT_CHUNK || len > MAX_BLOB_BYTES {
            return Promise::err(self.refuse(&BlobError::TooLarge {
                len,
                limit: MAX_BLOB_BYTES,
            }));
        }
        self.len.set(len);
        self.writing.set(true);
        let writing = Writing(Rc::clone(&self.writing));
        Promise::from_future(async move {
            let _writing = writing;
            // The message stays where the connection read it; only the
            // pieces in flight are copies.
            let bytes = params.get()?.get_bytes()?;
            for piece in bytes.chunks(UPLOAD_PIECE) {
                if let Err(error) = pieces.send(piece.to_vec()).await {
                    return Err(self.refuse(&error));
                }
            }
            Ok(())
        })
    }

    fn finish(
        self: Rc<Self>,
        _params: blob_upload::FinishParams,
        mut results: blob_upload::FinishResults,
    ) -> Promise<(), capnp::Error> {
        if self.writing.get() {
            return Promise::err(self.refuse(&BlobError::Refused(
                "an upload finishes after its last write".into(),
            )));
        }
        let (Some(pieces), Some(stored)) = (
            self.pieces.borrow_mut().take(),
            self.stored.borrow_mut().take(),
        ) else {
            return Promise::err(capnp::Error::failed("upload already finished".into()));
        };
        Promise::from_future(async move {
            // A backend that stopped reading answers why below.
            let ended = pieces.finish().await;
            let stored = stored
                .await
                .map_err(|error| {
                    capnp::Error::failed(format!("document media backend failed: {error}"))
                })?
                .and_then(|stored| ended.map(|()| stored))
                .map_err(|error| refuse(&error))?;
            write_ref(results.get().init_ref(), &stored);
            Ok(())
        })
    }
}

struct SessionServer {
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl session_blobs::Server for SessionServer {
    fn scope(
        self: Rc<Self>,
        params: session_blobs::ScopeParams,
        mut results: session_blobs::ScopeResults,
    ) -> Promise<(), capnp::Error> {
        let scope = params.get().and_then(|request| {
            let room = text_param(request.get_room())?;
            let app = text_param(request.get_app())?;
            BlobScope::new(room, app).map_err(|error| refuse(&error))
        });
        match scope {
            Ok(scope) => {
                results.get().set_blobs(capnp_rpc::new_client(ScopedBlobs {
                    scope,
                    backend: Arc::clone(&self.backend),
                    runtime: self.runtime.clone(),
                }));
                Promise::ok(())
            }
            Err(error) => Promise::err(error),
        }
    }
}

type Reply<T> = std::sync::mpsc::SyncSender<Result<T, BlobError>>;

/// One request to the connection's driver thread. Every job is one round
/// trip: content larger than a message travels as a sequence of upload jobs,
/// so a long transfer never holds the driver while other callers wait.
enum Job {
    Store(StoreJob),
    Read {
        target: u64,
        hash: String,
        offset: u64,
        max_len: u32,
        reply: Reply<ReadOutcome>,
    },
    Has {
        target: u64,
        hash: String,
        reply: Reply<bool>,
    },
    Size {
        target: u64,
        hash: String,
        reply: Reply<Option<u64>>,
    },
    Scope {
        scope: BlobScope,
        reply: Reply<u64>,
    },
}

/// Storing content: one message, or an upload of several.
enum StoreJob {
    Put {
        target: u64,
        bytes: Vec<u8>,
        mime: String,
        reply: Reply<BlobRef>,
    },
    Begin {
        target: u64,
        mime: String,
        reply: Reply<u64>,
    },
    Write {
        upload: u64,
        bytes: Vec<u8>,
        reply: Reply<()>,
    },
    Finish {
        upload: u64,
        reply: Reply<BlobRef>,
    },
    /// Release an upload that will not finish; nothing it wrote is stored.
    Abort {
        upload: u64,
    },
}

/// The root capability of a connection's own principal.
const ROOT: u64 = 0;

struct Worker {
    jobs: tokio::sync::mpsc::UnboundedSender<Job>,
    session: bool,
}

/// A synchronous client for [`DocumentBlobs`](document_blobs::Client).
///
/// Cheap to clone; clones share one connection and its driver thread. Every
/// call blocks for at most one local round trip to the daemon (a remote
/// fetch answers [`ReadOutcome::Pending`] rather than waiting); streaming
/// calls ([`put_reader`](Self::put_reader), [`BlobReader`]) make one round
/// trip per chunk.
#[derive(Clone)]
pub struct BlobClient {
    worker: Arc<Worker>,
    target: u64,
}

impl std::fmt::Debug for BlobClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobClient")
            .field("session", &self.worker.session)
            .field("target", &self.target)
            .finish()
    }
}

impl BlobClient {
    /// The scoped capability the launcher handed this process, if any.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] when no descriptor was inherited.
    pub fn from_environment() -> Result<Self, BlobError> {
        let connection = runtime_portal::connect_service_fd(BLOB_SERVICE).map_err(|error| {
            BlobError::Disconnected(match error {
                runtime_portal::NotAvailable::ServiceFdMissing => {
                    "no document media service".into()
                }
                error => error.to_string(),
            })
        })?;
        Self::start(connection.into_raw_fd(), false)
    }

    /// Wrap a connection whose handoff was already written for an app scope.
    /// Takes ownership of the stream.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] if the driver thread cannot start.
    pub fn from_app_stream(stream: StdUnixStream) -> Result<Self, BlobError> {
        Self::start(stream.into_raw_fd(), false)
    }

    /// Connect to the daemon at `socket` as the desktop session.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] when the socket cannot be reached.
    pub fn connect_session(socket: &Path) -> Result<Self, BlobError> {
        let mut stream = StdUnixStream::connect(socket)
            .map_err(|error| BlobError::Disconnected(format!("{}: {error}", socket.display())))?;
        write_handoff(&mut stream, &BlobPrincipal::Session)
            .map_err(|error| BlobError::Disconnected(error.to_string()))?;
        Self::from_session_stream(stream)
    }

    /// Wrap a connection whose session handoff was already written.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] if the driver thread cannot start.
    pub fn from_session_stream(stream: StdUnixStream) -> Result<Self, BlobError> {
        Self::start(stream.into_raw_fd(), true)
    }

    /// For a session client: the capability of one app in one room.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidScope`] for an invalid scope or a client
    /// that is not a session root, or a connection error.
    pub fn scope(&self, room: &str, app: &str) -> Result<Self, BlobError> {
        if !self.worker.session || self.target != ROOT {
            return Err(BlobError::InvalidScope);
        }
        let scope = BlobScope::new(room, app)?;
        let target = self.call(|reply| Job::Scope { scope, reply })?;
        Ok(Self {
            worker: Arc::clone(&self.worker),
            target,
        })
    }

    fn data_target(&self) -> Result<u64, BlobError> {
        if self.worker.session && self.target == ROOT {
            Err(BlobError::InvalidScope)
        } else {
            Ok(self.target)
        }
    }

    /// Store `bytes`, streaming when they exceed one message.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::TooLarge`] above [`MAX_BLOB_BYTES`], or the
    /// service's refusal.
    pub fn put(&self, bytes: &[u8], mime: &str) -> Result<BlobRef, BlobError> {
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len: bytes.len() as u64,
                limit: MAX_BLOB_BYTES,
            });
        }
        let mut reader = bytes;
        self.put_reader(&mut reader, mime)
    }

    /// Store everything `reader` yields, [`MAX_PUT_CHUNK`] at a time: this
    /// holds one chunk, never the whole content, and hashes it as it goes so
    /// the reference the store answers is checked against the bytes sent.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::TooLarge`] as soon as `reader` yields more than
    /// [`MAX_BLOB_BYTES`] (having read at most one byte past it),
    /// [`BlobError::Input`] when `reader` fails, a refusal when the store
    /// answers a reference for other content, or a connection error. A
    /// failed put stores nothing.
    pub fn put_reader(&self, reader: &mut dyn io::Read, mime: &str) -> Result<BlobRef, BlobError> {
        let target = self.data_target()?;
        let mut reader = reader.take(MAX_BLOB_BYTES.saturating_add(1));
        let mut hasher = blake3::Hasher::new();
        let mut total: u64 = 0;
        let mut upload: Option<OpenUpload<'_>> = None;
        loop {
            let chunk = next_chunk(&mut reader)?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_BLOB_BYTES {
                return Err(BlobError::TooLarge {
                    len: total,
                    limit: MAX_BLOB_BYTES,
                });
            }
            hasher.update(&chunk);
            let last = chunk.len() < MAX_PUT_CHUNK;
            let id = match upload.as_ref().map(|open| open.id) {
                Some(id) => id,
                None if last => {
                    // The whole content fits one message.
                    let mime = mime.to_owned();
                    let stored = self.call(|reply| {
                        Job::Store(StoreJob::Put {
                            target,
                            bytes: chunk,
                            mime,
                            reply,
                        })
                    })?;
                    return checked_ref(stored, &hasher, total);
                }
                None => {
                    let mime = mime.to_owned();
                    let id = self.call(|reply| {
                        Job::Store(StoreJob::Begin {
                            target,
                            mime,
                            reply,
                        })
                    })?;
                    upload = Some(OpenUpload {
                        client: self,
                        id,
                        open: true,
                    });
                    id
                }
            };
            if !chunk.is_empty() {
                self.call(|reply| {
                    Job::Store(StoreJob::Write {
                        upload: id,
                        bytes: chunk,
                        reply,
                    })
                })?;
            }
            if last {
                break;
            }
        }
        let Some(mut open) = upload else {
            return Err(BlobError::Refused("upload was never opened".into()));
        };
        open.open = false;
        let stored = self.call(|reply| {
            Job::Store(StoreJob::Finish {
                upload: open.id,
                reply,
            })
        })?;
        checked_ref(stored, &hasher, total)
    }

    /// Read one range.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`] or a connection error.
    pub fn read(&self, hash: &str, offset: u64, max_len: u32) -> Result<ReadOutcome, BlobError> {
        let target = self.data_target()?;
        if !valid_hash(hash) {
            return Err(BlobError::InvalidHash);
        }
        let hash = hash.to_owned();
        self.call(|reply| Job::Read {
            target,
            hash,
            offset,
            max_len,
            reply,
        })
    }

    /// A reader over `hash` that fetches [`MAX_READ_CHUNK`] ranges as it is
    /// read, or `None` when the content is pending or unavailable. Only the
    /// first range is read before this returns.
    ///
    /// The reader does not hash what it returns: a consumer verifies it (as
    /// [`read_all`](Self::read_all) does) or hands it to something that does.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`], [`BlobError::TooLarge`] for
    /// content above [`MAX_BLOB_BYTES`], or a connection error.
    pub fn open_reader(&self, hash: &str) -> Result<Option<BlobReader>, BlobError> {
        Ok(self.start_read(hash)?.ok())
    }

    fn start_read(&self, hash: &str) -> Result<Result<BlobReader, ReadOutcome>, BlobError> {
        let outcome = self.read(hash, 0, MAX_READ_CHUNK)?;
        if !matches!(outcome, ReadOutcome::Ready { .. }) {
            return Ok(Err(outcome));
        }
        let mut reader = BlobReader {
            client: self.clone(),
            hash: hash.to_owned(),
            total_len: None,
            next_offset: 0,
            chunk: Vec::new(),
            at: 0,
        };
        reader.accept(outcome)?;
        Ok(Ok(reader))
    }

    /// Read the whole content, range by range, and verify it against `hash`.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`], [`BlobError::TooLarge`], a
    /// refusal for content that fails verification, or a connection error.
    pub fn read_all(&self, hash: &str) -> Result<ReadOutcome, BlobError> {
        let mut reader = match self.start_read(hash)? {
            Ok(reader) => reader,
            Err(outcome) => return Ok(outcome),
        };
        let mut bytes = Vec::with_capacity(usize::try_from(reader.len()).unwrap_or(0));
        io::Read::read_to_end(&mut reader, &mut bytes).map_err(|error| blob_error(&error))?;
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            return Err(BlobError::Refused("content does not match its hash".into()));
        }
        let total_len = bytes.len() as u64;
        Ok(ReadOutcome::Ready { bytes, total_len })
    }

    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`] or a connection error.
    pub fn has(&self, hash: &str) -> Result<bool, BlobError> {
        let target = self.data_target()?;
        if !valid_hash(hash) {
            return Err(BlobError::InvalidHash);
        }
        let hash = hash.to_owned();
        self.call(|reply| Job::Has {
            target,
            hash,
            reply,
        })
    }

    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`] or a connection error.
    pub fn size(&self, hash: &str) -> Result<Option<u64>, BlobError> {
        let target = self.data_target()?;
        if !valid_hash(hash) {
            return Err(BlobError::InvalidHash);
        }
        let hash = hash.to_owned();
        self.call(|reply| Job::Size {
            target,
            hash,
            reply,
        })
    }

    fn call<T>(&self, job: impl FnOnce(Reply<T>) -> Job) -> Result<T, BlobError> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.worker
            .jobs
            .send(job(reply))
            .map_err(|_| BlobError::Disconnected("document media connection closed".into()))?;
        answer
            .recv_timeout(CALL_TIMEOUT)
            .map_err(|_| BlobError::Disconnected("document media service did not answer".into()))?
    }

    fn start(fd: RawFd, session: bool) -> Result<Self, BlobError> {
        let (jobs, receiver) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("blob-client".into())
            .spawn(move || drive(fd, session, receiver))
            .map_err(|error| BlobError::Disconnected(error.to_string()))?;
        Ok(Self {
            worker: Arc::new(Worker { jobs, session }),
            target: ROOT,
        })
    }
}

/// Up to one message of content from `reader`; shorter only at its end.
fn next_chunk(reader: &mut impl io::Read) -> Result<Vec<u8>, BlobError> {
    let mut chunk = Vec::with_capacity(MAX_PUT_CHUNK);
    reader
        .take(MAX_PUT_CHUNK as u64)
        .read_to_end(&mut chunk)
        .map_err(|error| BlobError::Input(error.to_string()))?;
    Ok(chunk)
}

/// The reference goes into a document; one naming other bytes would poison
/// every replica that later reads it.
fn checked_ref(stored: BlobRef, sent: &blake3::Hasher, len: u64) -> Result<BlobRef, BlobError> {
    if stored.hash != sent.finalize().to_hex().as_str() || stored.len != len {
        return Err(BlobError::Refused(
            "store answered a reference for other content".into(),
        ));
    }
    Ok(stored)
}

/// An upload that has begun; released on the driver unless it finished.
struct OpenUpload<'a> {
    client: &'a BlobClient,
    id: u64,
    open: bool,
}

impl Drop for OpenUpload<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = self
                .client
                .worker
                .jobs
                .send(Job::Store(StoreJob::Abort { upload: self.id }));
        }
    }
}

/// Content read range by range as it is consumed ([`BlobClient::open_reader`]).
///
/// It holds at most one [`MAX_READ_CHUNK`] range. A blob whose length changes
/// mid-read, a range that makes no progress or content that stops being
/// readable fails the read. The bytes are **not** verified here.
pub struct BlobReader {
    client: BlobClient,
    hash: String,
    total_len: Option<u64>,
    /// Where the range after `chunk` starts.
    next_offset: u64,
    chunk: Vec<u8>,
    /// How much of `chunk` was returned.
    at: usize,
}

impl std::fmt::Debug for BlobReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobReader")
            .field("hash", &self.hash)
            .field("total_len", &self.total_len)
            .field("next_offset", &self.next_offset)
            .finish_non_exhaustive()
    }
}

impl BlobReader {
    /// The content's whole length, as the store reported it.
    pub fn len(&self) -> u64 {
        self.total_len.unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take one answered range as the next chunk.
    fn accept(&mut self, outcome: ReadOutcome) -> Result<(), BlobError> {
        let ReadOutcome::Ready { bytes, total_len } = outcome else {
            return Err(BlobError::Refused(
                "blob stopped being readable mid-read".into(),
            ));
        };
        if total_len > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len: total_len,
                limit: MAX_BLOB_BYTES,
            });
        }
        if *self.total_len.get_or_insert(total_len) != total_len {
            return Err(BlobError::Refused("blob length changed mid-read".into()));
        }
        let end = self.next_offset.saturating_add(bytes.len() as u64);
        if bytes.is_empty() && self.next_offset < total_len {
            return Err(BlobError::Refused("blob read made no progress".into()));
        }
        if end > total_len {
            return Err(BlobError::Refused("blob read ran past its length".into()));
        }
        self.next_offset = end;
        self.chunk = bytes;
        self.at = 0;
        Ok(())
    }
}

impl io::Read for BlobReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.chunk.len() {
            if self.next_offset >= self.len() || buf.is_empty() {
                return Ok(0);
            }
            let outcome = self
                .client
                .read(&self.hash, self.next_offset, MAX_READ_CHUNK)
                .map_err(io_error)?;
            self.accept(outcome).map_err(io_error)?;
        }
        let rest = self.chunk.get(self.at..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf.get_mut(..n)
            .unwrap_or_default()
            .copy_from_slice(rest.get(..n).unwrap_or_default());
        self.at = self.at.saturating_add(n);
        Ok(n)
    }
}

fn io_error(error: BlobError) -> io::Error {
    let kind = match error {
        BlobError::Refused(_) | BlobError::TooLarge { .. } => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

fn blob_error(error: &io::Error) -> BlobError {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<BlobError>())
        .cloned()
        .unwrap_or_else(|| BlobError::Disconnected(error.to_string()))
}

/// What the driver thread holds for one connection.
struct Connection {
    /// The principal's root, for a session connection.
    session_root: Option<session_blobs::Client>,
    scopes: RefCell<HashMap<u64, document_blobs::Client>>,
    uploads: RefCell<HashMap<u64, blob_upload::Client>>,
    next_id: Cell<u64>,
}

impl Connection {
    fn issue_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id.saturating_add(1));
        id
    }

    fn scope(&self, target: u64) -> Result<document_blobs::Client, BlobError> {
        self.scopes
            .borrow()
            .get(&target)
            .cloned()
            .ok_or(BlobError::InvalidScope)
    }

    fn upload(&self, id: u64) -> Result<blob_upload::Client, BlobError> {
        self.uploads
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| BlobError::Refused("no such upload".into()))
    }
}

fn drive(fd: RawFd, session: bool, mut jobs: tokio::sync::mpsc::UnboundedReceiver<Job>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        // Dropping the receiver fails every pending and later call.
        return;
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let Ok(rpc) = runtime_portal::UnixCapnpClient::from_connected_fd(
            fd,
            rpc_twoparty_capnp::Side::Client,
        ) else {
            return;
        };
        let mut scopes = HashMap::new();
        let session_root: Option<session_blobs::Client> = if session {
            Some(rpc.bootstrap())
        } else {
            let root: document_blobs::Client = rpc.bootstrap();
            scopes.insert(ROOT, root);
            None
        };
        let connection = Rc::new(Connection {
            session_root,
            scopes: RefCell::new(scopes),
            uploads: RefCell::new(HashMap::new()),
            next_id: Cell::new(ROOT.saturating_add(1)),
        });
        while let Some(job) = jobs.recv().await {
            let connection = Rc::clone(&connection);
            tokio::task::spawn_local(async move {
                run_job(job, &connection).await;
            });
        }
    });
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc clients are Rc-based and driven on this thread's LocalSet"
)]
async fn run_job(job: Job, connection: &Connection) {
    match job {
        Job::Store(job) => store(job, connection).await,
        Job::Read {
            target,
            hash,
            offset,
            max_len,
            reply,
        } => {
            let result = async {
                let client = connection.scope(target)?;
                let mut request = client.get_request();
                {
                    let mut params = request.get();
                    params.set_hash(hash.as_str());
                    params.set_offset(offset);
                    params.set_max_len(max_len);
                }
                let answer = request.send().promise.await?;
                let read = answer.get()?.get_read()?;
                let status = read
                    .get_status()
                    .map_err(|_| BlobError::Refused("unknown read status".into()))?;
                Ok(match status {
                    WireReadStatus::Ready => ReadOutcome::Ready {
                        bytes: read.get_bytes()?.to_vec(),
                        total_len: read.get_total_len(),
                    },
                    WireReadStatus::Pending => ReadOutcome::Pending,
                    WireReadStatus::Unavailable => ReadOutcome::Unavailable,
                })
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Has {
            target,
            hash,
            reply,
        } => {
            let result = async {
                let client = connection.scope(target)?;
                let mut request = client.has_request();
                request.get().set_hash(hash.as_str());
                let answer = request.send().promise.await?;
                Ok(answer.get()?.get_present())
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Size {
            target,
            hash,
            reply,
        } => {
            let result = async {
                let client = connection.scope(target)?;
                let mut request = client.size_request();
                request.get().set_hash(hash.as_str());
                let answer = request.send().promise.await?;
                let answer = answer.get()?;
                Ok(answer.get_known().then(|| answer.get_len()))
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Scope { scope, reply } => {
            let result = async {
                let root = connection
                    .session_root
                    .as_ref()
                    .ok_or(BlobError::InvalidScope)?;
                let mut request = root.scope_request();
                {
                    let mut params = request.get();
                    params.set_room(scope.room());
                    params.set_app(scope.app());
                }
                let answer = request.send().promise.await?;
                let client = answer.get()?.get_blobs()?;
                let id = connection.issue_id();
                connection.scopes.borrow_mut().insert(id, client);
                Ok(id)
            }
            .await;
            let _ = reply.send(result);
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc clients are Rc-based and driven on this thread's LocalSet"
)]
async fn store(job: StoreJob, connection: &Connection) {
    match job {
        StoreJob::Put {
            target,
            bytes,
            mime,
            reply,
        } => {
            let result = async {
                let client = connection.scope(target)?;
                let mut request = client.put_request();
                {
                    let mut params = request.get();
                    params.set_bytes(&bytes);
                    params.set_mime(mime.as_str());
                }
                drop(bytes);
                let answer = request.send().promise.await?;
                blob_ref_from(answer.get()?.get_ref()?)
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Begin {
            target,
            mime,
            reply,
        } => {
            let result = async {
                let client = connection.scope(target)?;
                let mut request = client.upload_request();
                request.get().set_mime(mime.as_str());
                let answer = request.send().promise.await?;
                let upload = answer.get()?.get_upload()?;
                let id = connection.issue_id();
                connection.uploads.borrow_mut().insert(id, upload);
                Ok(id)
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Write {
            upload,
            bytes,
            reply,
        } => {
            let result = async {
                let upload = connection.upload(upload)?;
                let mut request = upload.write_request();
                request.get().set_bytes(&bytes);
                drop(bytes);
                request.send().promise.await?;
                Ok(())
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Finish { upload, reply } => {
            let result = async {
                let client = connection.upload(upload)?;
                connection.uploads.borrow_mut().remove(&upload);
                let answer = client.finish_request().send().promise.await?;
                blob_ref_from(answer.get()?.get_ref()?)
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Abort { upload } => {
            connection.uploads.borrow_mut().remove(&upload);
        }
    }
}

fn blob_ref_from(reader: crate::blobs_capnp::blob_ref::Reader<'_>) -> Result<BlobRef, BlobError> {
    let hash = reader
        .get_hash()?
        .to_str()
        .map_err(|error| BlobError::Refused(error.to_string()))?
        .to_owned();
    let mime = reader
        .get_mime()?
        .to_str()
        .map_err(|error| BlobError::Refused(error.to_string()))?
        .to_owned();
    Ok(BlobRef {
        hash,
        len: reader.get_len(),
        mime,
    })
}

#[cfg(test)]
mod tests;
