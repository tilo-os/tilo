use capnp::capability::Promise;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

pub use tilo_fs_client::blobs::*;

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
    ///
    /// # Errors
    ///
    /// Returns the backend's refusal.
    fn attribute_writes(&self, attribution: WriteAttribution) -> Result<(), BlobError> {
        let _ = attribution;
        Err(BlobError::Refused("write attribution unsupported".into()))
    }
}

/// The refusal of a backend that keeps no public content.
fn no_public_content() -> BlobError {
    BlobError::Refused("this device keeps no public content".into())
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
            write_read(results.get().init_read(), &outcome);
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

    fn public_content(
        self: Rc<Self>,
        params: document_blobs::PublicContentParams,
        mut results: document_blobs::PublicContentResults,
    ) -> Promise<(), capnp::Error> {
        match params
            .get()
            .and_then(|request| text_param(request.get_name()))
        {
            Ok(name) => {
                results
                    .get()
                    .set_content(capnp_rpc::new_client(PublicContentServer {
                        scope: self.scope.clone(),
                        name,
                        backend: Arc::clone(&self.backend),
                        runtime: self.runtime.clone(),
                    }));
                Promise::ok(())
            }
            Err(error) => Promise::err(error),
        }
    }
}

/// One public copy, as an app's scope reaches it.
struct PublicContentServer {
    scope: BlobScope,
    name: String,
    backend: Arc<dyn BlobBackend>,
    runtime: tokio::runtime::Handle,
}

fn write_read(mut read: crate::blobs_capnp::blob_read::Builder<'_>, outcome: &ReadOutcome) {
    match outcome {
        ReadOutcome::Ready { bytes, total_len } => {
            read.set_status(WireReadStatus::Ready);
            read.set_bytes(bytes);
            read.set_total_len(*total_len);
        }
        ReadOutcome::Pending => read.set_status(WireReadStatus::Pending),
        ReadOutcome::Unavailable => read.set_status(WireReadStatus::Unavailable),
    }
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl public_content::Server for PublicContentServer {
    fn keep(
        self: Rc<Self>,
        _params: public_content::KeepParams,
        _results: public_content::KeepResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).public_keep(self.scope.clone(), self.name.clone()),
            )
            .await?
            .map_err(|error| refuse(&error))
        })
    }

    fn release(
        self: Rc<Self>,
        _params: public_content::ReleaseParams,
        _results: public_content::ReleaseResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).public_release(self.scope.clone(), self.name.clone()),
            )
            .await?
            .map_err(|error| refuse(&error))
        })
    }

    fn read(
        self: Rc<Self>,
        params: public_content::ReadParams,
        mut results: public_content::ReadResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let request = params.get()?;
            let offset = request.get_offset();
            let max_len = request.get_max_len().min(MAX_READ_CHUNK);
            let read = on_runtime(
                &self.runtime,
                Arc::clone(&self.backend).public_read(self.name.clone(), offset, max_len),
            )
            .await?
            .map_err(|error| refuse(&error))?;
            let mut reply = results.get();
            write_read(reply.reborrow().init_read(), &read.outcome);
            reply.set_hash(read.hash.as_deref().unwrap_or_default());
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

    fn refresh_public_content(
        self: Rc<Self>,
        _params: session_blobs::RefreshPublicContentParams,
        mut results: session_blobs::RefreshPublicContentResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let refreshed =
                on_runtime(&self.runtime, Arc::clone(&self.backend).refresh_public()).await?;
            results.get().set_refreshed(refreshed);
            Ok(())
        })
    }

    fn attribute_writes(
        self: Rc<Self>,
        params: session_blobs::AttributeWritesParams,
        _results: session_blobs::AttributeWritesResults,
    ) -> Promise<(), capnp::Error> {
        let attribution = params.get().and_then(|request| {
            let rows = request.get_paths()?;
            if rows.len() as usize > MAX_ATTRIBUTION_PATHS {
                return Err(capnp::Error::failed("too many attributed paths".into()));
            }
            let paths = rows.iter().map(text_param).collect::<Result<Vec<_>, _>>()?;
            let [_, red, green, blue] = request.get_color().to_be_bytes();
            Ok(WriteAttribution {
                paths,
                actor: text_param(request.get_actor())?,
                label: text_param(request.get_label())?,
                color: [red, green, blue],
                ttl: Duration::from_millis(u64::from(request.get_ttl_ms())),
            })
        });
        match attribution {
            Ok(attribution) => match self.backend.attribute_writes(attribution) {
                Ok(()) => Promise::ok(()),
                Err(error) => Promise::err(refuse(&error)),
            },
            Err(error) => Promise::err(error),
        }
    }
}

#[cfg(test)]
mod tests;
