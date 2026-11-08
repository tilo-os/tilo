use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

pub use tilo_fs_client::blobs::*;
use tilo_fs_client::connection::{Capabilities, ServeObject, serve_connected};
use tilo_fs_client::wire::{FsReply, FsRequest};

const HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);

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
    pub async fn send(&self, piece: Vec<u8>) -> Result<(), BlobError> {
        self.pieces
            .send(Some(piece))
            .await
            .map_err(|_| BlobError::Refused("the store stopped reading the upload".into()))
    }

    /// End the content cleanly; dropping every sender without this abandons it.
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

    /// Keep a copy of the public content `name` on this device for `scope`'s
    /// app ([`PublicRead`]). A backend that keeps none refuses.
    fn public_keep(
        self: Arc<Self>,
        scope: BlobScope,
        name: String,
    ) -> BlobFuture<Result<(), BlobError>> {
        let _ = (scope, name);
        Box::pin(async { Err(no_public_content()) })
    }

    /// `scope`'s app no longer keeps the public content `name`.
    fn public_release(
        self: Arc<Self>,
        scope: BlobScope,
        name: String,
    ) -> BlobFuture<Result<(), BlobError>> {
        let _ = (scope, name);
        Box::pin(async { Err(no_public_content()) })
    }

    /// Read the current copy of the public content `name`.
    fn public_read(
        self: Arc<Self>,
        name: String,
        offset: u64,
        max_len: u32,
    ) -> BlobFuture<Result<PublicRead, BlobError>> {
        let _ = (name, offset, max_len);
        Box::pin(async { Err(no_public_content()) })
    }

    /// Check every kept public copy against where it came from and take a
    /// newer one: how many changed.
    fn refresh_public(self: Arc<Self>) -> BlobFuture<u32> {
        Box::pin(async { 0 })
    }

    /// The session names the agent behind this device's next writes to some
    /// paths ([`BlobClient::attribute_writes`]). Only a session connection
    /// reaches this; a backend without write attribution refuses it.
    fn attribute_writes(&self, attribution: WriteAttribution) -> Result<(), BlobError> {
        let _ = attribution;
        Err(BlobError::Refused("write attribution unsupported".into()))
    }
}

/// The refusal of a backend that keeps no public content.
fn no_public_content() -> BlobError {
    BlobError::Refused("this device keeps no public content".into())
}

/// Serve `listener` on a dedicated thread.
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
            local_runtime.block_on(async move {
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
                    tokio::spawn(async move {
                        if let Err(error) = serve_connection(stream, backend, runtime).await {
                            eprintln!("document media connection closed: {error}");
                        }
                    });
                }
            });
        })
}

/// Serve one already-accepted connection: read its handoff, then answer
/// frames until the peer goes away.
pub async fn serve_connection(
    mut stream: tokio::net::UnixStream,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
) -> io::Result<()> {
    let principal = tokio::time::timeout(HANDOFF_TIMEOUT, read_handoff(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handoff timed out"))??;
    let root: Arc<dyn ServeObject> = match principal {
        BlobPrincipal::App(scope) => Arc::new(ScopedBlobs {
            scope,
            backend,
            runtime,
        }),
        BlobPrincipal::Session => Arc::new(SessionServer { backend, runtime }),
    };
    serve_connected(stream, root).await
}

async fn on_runtime<T: Send + 'static>(
    runtime: &tokio::runtime::Handle,
    future: BlobFuture<T>,
) -> Result<T, String> {
    runtime
        .spawn(future)
        .await
        .map_err(|error| format!("document media backend failed: {error}"))
}

fn checked_hash(hash: String) -> Result<String, String> {
    if valid_hash(&hash) {
        Ok(hash)
    } else {
        Err(BlobError::InvalidHash.to_string())
    }
}

struct ScopedBlobs {
    scope: BlobScope,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
}

impl ServeObject for ScopedBlobs {
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        let capabilities = capabilities.clone();
        Box::pin(async move {
            let scope = &self.scope;
            match request {
                FsRequest::Put { bytes, mime } => {
                    if bytes.len() > MAX_PUT_CHUNK {
                        return Err(BlobError::TooLarge {
                            len: bytes.len() as u64,
                            limit: MAX_PUT_CHUNK as u64,
                        }
                        .to_string());
                    }
                    let stored = on_runtime(
                        &self.runtime,
                        Arc::clone(&self.backend).put(scope.clone(), bytes, mime),
                    )
                    .await?
                    .map_err(|error| error.to_string())?;
                    Ok(FsReply::Blob(stored))
                }
                FsRequest::Upload { mime } => {
                    // The backend consumes the upload as it is written, so the
                    // store hashes and imports it piece by piece and nothing
                    // here ever holds the whole payload.
                    let (pieces, chunks) = BlobChunks::channel(UPLOAD_PIECES_IN_FLIGHT);
                    let stored = self.runtime.spawn(Arc::clone(&self.backend).put_stream(
                        scope.clone(),
                        chunks,
                        mime,
                    ));
                    let (ended, writing) = tokio::sync::watch::channel(false);
                    let upload = Arc::new(Upload {
                        state: tokio::sync::Mutex::new(Some(Uploading {
                            pieces,
                            stored,
                            len: 0,
                            writing,
                        })),
                        ended,
                    });
                    Ok(FsReply::Handle(Some(capabilities.register(upload))))
                }
                FsRequest::Get {
                    hash,
                    offset,
                    max_len,
                } => {
                    let hash = checked_hash(hash)?;
                    let max_len = max_len.min(MAX_READ_CHUNK);
                    let outcome = on_runtime(
                        &self.runtime,
                        Arc::clone(&self.backend).read(scope.clone(), hash, offset, max_len),
                    )
                    .await?;
                    Ok(FsReply::BlobRead(outcome))
                }
                FsRequest::Has { hash } => {
                    let hash = checked_hash(hash)?;
                    let present = on_runtime(
                        &self.runtime,
                        Arc::clone(&self.backend).has(scope.clone(), hash),
                    )
                    .await?;
                    Ok(FsReply::Present(present))
                }
                FsRequest::Size { hash } => {
                    let hash = checked_hash(hash)?;
                    let size = on_runtime(
                        &self.runtime,
                        Arc::clone(&self.backend).size(scope.clone(), hash),
                    )
                    .await?;
                    Ok(FsReply::Size(size))
                }
                FsRequest::PublicContent { name } => {
                    let copy = Arc::new(PublicContentServer {
                        scope: scope.clone(),
                        name,
                        backend: Arc::clone(&self.backend),
                        runtime: self.runtime.clone(),
                    });
                    Ok(FsReply::Handle(Some(capabilities.register(copy))))
                }
                _ => Err("unexpected document media request".into()),
            }
        })
    }
}

/// One public copy, as an app's scope reaches it.
struct PublicContentServer {
    scope: BlobScope,
    name: String,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
}

impl ServeObject for PublicContentServer {
    fn dispatch(
        self: Arc<Self>,
        _: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            match request {
                FsRequest::PublicKeep => on_runtime(
                    &self.runtime,
                    Arc::clone(&self.backend).public_keep(self.scope.clone(), self.name.clone()),
                )
                .await?
                .map_err(|error| error.to_string())
                .map(|()| FsReply::Empty),
                FsRequest::PublicRelease => on_runtime(
                    &self.runtime,
                    Arc::clone(&self.backend).public_release(self.scope.clone(), self.name.clone()),
                )
                .await?
                .map_err(|error| error.to_string())
                .map(|()| FsReply::Empty),
                FsRequest::PublicRead { offset, max_len } => {
                    let max_len = max_len.min(MAX_READ_CHUNK);
                    let read = on_runtime(
                        &self.runtime,
                        Arc::clone(&self.backend).public_read(self.name.clone(), offset, max_len),
                    )
                    .await?
                    .map_err(|error| error.to_string())?;
                    Ok(FsReply::PublicRead {
                        read: read.outcome,
                        hash: read.hash.unwrap_or_default(),
                    })
                }
                _ => Err("unexpected public content request".into()),
            }
        })
    }
}

struct Uploading {
    /// The backend's end of the stream; dropped unfinished when refused.
    pieces: BlobChunkSender,
    /// The backend storing the stream; it answers once the stream ends.
    stored: tokio::task::JoinHandle<Result<BlobRef, BlobError>>,
    /// Bytes written so far.
    len: u64,
    /// Set when the upload is refused while this write holds it.
    writing: tokio::sync::watch::Receiver<bool>,
}

/// One upload in progress. Each `write` is passed on to the backend in
/// [`UPLOAD_PIECE`]s as soon as it arrives, and is answered once the backend
/// has taken all of it, which is what holds the uploader back. Writes are
/// taken one at a time, as [`BlobClient::put_reader`] sends them; a write
/// pipelined behind another is refused, so what waits here stays bounded by
/// one message whatever the peer sends.
struct Upload {
    /// `None` once finished or refused.
    state: tokio::sync::Mutex<Option<Uploading>>,
    /// Ends the write in flight when a call is refused behind it.
    ended: tokio::sync::watch::Sender<bool>,
}

impl Upload {
    fn busy(message: &str) -> String {
        message.into()
    }

    /// Refuse a call that arrived while a write holds the upload: that write
    /// stops, and the backend sees the stream end without finishing, so the
    /// upload stores nothing.
    fn refuse(&self, message: &str) -> String {
        self.ended.send_replace(true);
        message.into()
    }
}

impl ServeObject for Upload {
    fn dispatch(
        self: Arc<Self>,
        _: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            match request {
                FsRequest::UploadWrite { bytes } => {
                    let Ok(mut state) = self.state.try_lock() else {
                        return Err(self.refuse("an upload takes one write at a time"));
                    };
                    let Some(upload) = state.as_mut() else {
                        return Err(Self::busy("upload already finished"));
                    };
                    let len = upload.len.saturating_add(bytes.len() as u64);
                    if bytes.len() > MAX_PUT_CHUNK || len > MAX_BLOB_BYTES {
                        *state = None;
                        return Err(BlobError::TooLarge {
                            len,
                            limit: MAX_BLOB_BYTES,
                        }
                        .to_string());
                    }
                    upload.len = len;
                    for piece in bytes.chunks(UPLOAD_PIECE) {
                        if *upload.writing.borrow_and_update() {
                            *state = None;
                            return Err(Self::busy("upload already finished"));
                        }
                        let sent = tokio::select! {
                            biased;
                            _ = upload.writing.changed() => false,
                            sent = upload.pieces.send(piece.to_vec()) => sent.is_ok(),
                        };
                        if !sent {
                            *state = None;
                            return Err(Self::busy("upload already finished"));
                        }
                    }
                    Ok(FsReply::Empty)
                }
                FsRequest::UploadFinish => {
                    let Ok(mut state) = self.state.try_lock() else {
                        return Err(self.refuse("an upload finishes after its last write"));
                    };
                    let Some(upload) = state.take() else {
                        return Err(Self::busy("upload already finished"));
                    };
                    drop(state);
                    // A backend that stopped reading answers why below.
                    let ended = upload.pieces.finish().await;
                    let stored = upload
                        .stored
                        .await
                        .map_err(|error| format!("document media backend failed: {error}"))?
                        .and_then(|stored| ended.map(|()| stored))
                        .map_err(|error| error.to_string())?;
                    Ok(FsReply::Blob(stored))
                }
                _ => Err("unexpected upload request".into()),
            }
        })
    }
}

struct SessionServer {
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
}

impl ServeObject for SessionServer {
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        let capabilities = capabilities.clone();
        Box::pin(async move {
            match request {
                FsRequest::SessionScope { workspace, app } => {
                    let scope =
                        BlobScope::new(workspace, app).map_err(|error| error.to_string())?;
                    let blobs = Arc::new(ScopedBlobs {
                        scope,
                        backend: Arc::clone(&self.backend),
                        runtime: self.runtime.clone(),
                    });
                    Ok(FsReply::Scope(capabilities.register(blobs)))
                }
                FsRequest::SessionRefreshPublic => {
                    let refreshed =
                        on_runtime(&self.runtime, Arc::clone(&self.backend).refresh_public())
                            .await?;
                    Ok(FsReply::Refreshed(refreshed))
                }
                FsRequest::SessionAttributeWrites {
                    paths,
                    actor,
                    label,
                    color,
                    ttl_ms,
                } => {
                    if paths.len() > MAX_ATTRIBUTION_PATHS {
                        return Err("too many attributed paths".into());
                    }
                    let attribution = WriteAttribution {
                        paths,
                        actor,
                        label,
                        color,
                        ttl: Duration::from_millis(u64::from(ttl_ms)),
                    };
                    self.backend
                        .attribute_writes(attribution)
                        .map_err(|error| error.to_string())?;
                    Ok(FsReply::Empty)
                }
                _ => Err("unexpected session media request".into()),
            }
        })
    }
}

#[cfg(test)]
mod tests;
