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
use std::io;
use std::os::fd::{IntoRawFd, RawFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
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
    let (read_half, write_half) = stream.into_split();
    let network = twoparty::VatNetwork::new(
        read_half.compat(),
        write_half.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        capnp::message::ReaderOptions::default(),
    );
    RpcSystem::new(Box::new(network), Some(bootstrap))
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
        results.get().set_upload(capnp_rpc::new_client(Upload {
            scope: self.scope.clone(),
            mime,
            buffer: RefCell::new(Vec::new()),
            finished: Cell::new(false),
            backend: Arc::clone(&self.backend),
            runtime: self.runtime.clone(),
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

struct Upload {
    scope: BlobScope,
    mime: String,
    buffer: RefCell<Vec<u8>>,
    finished: Cell<bool>,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
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
        if self.finished.get() {
            return Promise::err(capnp::Error::failed("upload already finished".into()));
        }
        let bytes = match params
            .get()
            .and_then(crate::blobs_capnp::blob_upload::write_params::Reader::get_bytes)
        {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(error),
        };
        let mut buffer = self.buffer.borrow_mut();
        let len = (buffer.len() as u64).saturating_add(bytes.len() as u64);
        if bytes.len() > MAX_PUT_CHUNK || len > MAX_BLOB_BYTES {
            return Promise::err(refuse(&BlobError::TooLarge {
                len,
                limit: MAX_BLOB_BYTES,
            }));
        }
        buffer.extend_from_slice(bytes);
        drop(buffer);
        Promise::ok(())
    }

    fn finish(
        self: Rc<Self>,
        _params: blob_upload::FinishParams,
        mut results: blob_upload::FinishResults,
    ) -> Promise<(), capnp::Error> {
        if self.finished.replace(true) {
            return Promise::err(capnp::Error::failed("upload already finished".into()));
        }
        let bytes = std::mem::take(&mut *self.buffer.borrow_mut());
        Promise::from_future(async move {
            let stored = on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).put(self.scope.clone(), bytes, self.mime.clone()),
            )
            .await?
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

enum Job {
    Put {
        target: u64,
        bytes: Vec<u8>,
        mime: String,
        reply: Reply<BlobRef>,
    },
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
/// fetch answers [`ReadOutcome::Pending`] rather than waiting).
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
        let target = self.data_target()?;
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len: bytes.len() as u64,
                limit: MAX_BLOB_BYTES,
            });
        }
        let bytes = bytes.to_vec();
        let mime = mime.to_owned();
        self.call(|reply| Job::Put {
            target,
            bytes,
            mime,
            reply,
        })
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

    /// Read the whole content, range by range, and verify it against `hash`.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`], [`BlobError::TooLarge`], a
    /// refusal for content that fails verification, or a connection error.
    pub fn read_all(&self, hash: &str) -> Result<ReadOutcome, BlobError> {
        let mut bytes = Vec::new();
        let mut total = None;
        loop {
            match self.read(hash, bytes.len() as u64, MAX_READ_CHUNK)? {
                ReadOutcome::Ready {
                    bytes: chunk,
                    total_len,
                } => {
                    if total_len > MAX_BLOB_BYTES {
                        return Err(BlobError::TooLarge {
                            len: total_len,
                            limit: MAX_BLOB_BYTES,
                        });
                    }
                    if *total.get_or_insert(total_len) != total_len {
                        return Err(BlobError::Refused("blob length changed mid-read".into()));
                    }
                    if chunk.is_empty() && (bytes.len() as u64) < total_len {
                        return Err(BlobError::Refused("blob read made no progress".into()));
                    }
                    bytes.extend_from_slice(&chunk);
                    if bytes.len() as u64 >= total_len {
                        break;
                    }
                }
                other => return Ok(other),
            }
        }
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
        let Ok(connection) = runtime_portal::UnixCapnpClient::from_connected_fd(
            fd,
            rpc_twoparty_capnp::Side::Client,
        ) else {
            return;
        };
        let scopes: Rc<RefCell<HashMap<u64, document_blobs::Client>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let session_root: Option<session_blobs::Client> = if session {
            Some(connection.bootstrap())
        } else {
            let root: document_blobs::Client = connection.bootstrap();
            scopes.borrow_mut().insert(ROOT, root);
            None
        };
        let next_scope = Rc::new(Cell::new(ROOT.saturating_add(1)));
        while let Some(job) = jobs.recv().await {
            let scopes = Rc::clone(&scopes);
            let session_root = session_root.clone();
            let next_scope = Rc::clone(&next_scope);
            tokio::task::spawn_local(async move {
                run_job(job, &scopes, session_root.as_ref(), &next_scope).await;
            });
        }
    });
}

fn target_client(
    scopes: &RefCell<HashMap<u64, document_blobs::Client>>,
    target: u64,
) -> Result<document_blobs::Client, BlobError> {
    scopes
        .borrow()
        .get(&target)
        .cloned()
        .ok_or(BlobError::InvalidScope)
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc clients are Rc-based and driven on this thread's LocalSet"
)]
async fn run_job(
    job: Job,
    scopes: &RefCell<HashMap<u64, document_blobs::Client>>,
    session_root: Option<&session_blobs::Client>,
    next_scope: &Cell<u64>,
) {
    match job {
        Job::Put {
            target,
            bytes,
            mime,
            reply,
        } => {
            let result = async {
                let client = target_client(scopes, target)?;
                put_on(&client, &bytes, &mime).await
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Read {
            target,
            hash,
            offset,
            max_len,
            reply,
        } => {
            let result = async {
                let client = target_client(scopes, target)?;
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
                let client = target_client(scopes, target)?;
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
                let client = target_client(scopes, target)?;
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
                let root = session_root.ok_or(BlobError::InvalidScope)?;
                let mut request = root.scope_request();
                {
                    let mut params = request.get();
                    params.set_room(scope.room());
                    params.set_app(scope.app());
                }
                let answer = request.send().promise.await?;
                let client = answer.get()?.get_blobs()?;
                let id = next_scope.get();
                next_scope.set(id.saturating_add(1));
                scopes.borrow_mut().insert(id, client);
                Ok(id)
            }
            .await;
            let _ = reply.send(result);
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

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc clients are Rc-based and driven on this thread's LocalSet"
)]
async fn put_on(
    client: &document_blobs::Client,
    bytes: &[u8],
    mime: &str,
) -> Result<BlobRef, BlobError> {
    let expected = blake3::hash(bytes).to_hex().to_string();
    let stored = if bytes.len() <= MAX_PUT_CHUNK {
        let mut request = client.put_request();
        {
            let mut params = request.get();
            params.set_bytes(bytes);
            params.set_mime(mime);
        }
        let answer = request.send().promise.await?;
        blob_ref_from(answer.get()?.get_ref()?)?
    } else {
        let mut request = client.upload_request();
        request.get().set_mime(mime);
        let answer = request.send().promise.await?;
        let upload = answer.get()?.get_upload()?;
        for chunk in bytes.chunks(MAX_PUT_CHUNK) {
            let mut write = upload.write_request();
            write.get().set_bytes(chunk);
            write.send().promise.await?;
        }
        let answer = upload.finish_request().send().promise.await?;
        blob_ref_from(answer.get()?.get_ref()?)?
    };
    // The reference goes into a document; one naming other bytes would
    // poison every replica that later reads it.
    if stored.hash != expected || stored.len != bytes.len() as u64 {
        return Err(BlobError::Refused(
            "store answered a reference for other content".into(),
        ));
    }
    Ok(stored)
}

#[cfg(test)]
mod tests;
