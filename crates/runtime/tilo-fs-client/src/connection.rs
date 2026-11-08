//! The framed connection both ends of the filesystem protocol drive.
//!
//! One [`Connection`] is one authenticated stream. Calls carry an id so many
//! may be in flight at once; the serving side answers each from its object
//! table, and an object the client drops is released there, which is what
//! closes a held file and releases native claims.
//!
//! The serving side is a table of [`ServeObject`]s, rooted at the capability
//! the connection was handed.

use std::{
    collections::HashMap,
    future::Future,
    io,
    os::unix::net::UnixStream,
    pin::Pin,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use runtime_portal::NotAvailable;
use tokio::sync::oneshot;
use wire_frame::{FrameReader, Framing};

use crate::wire::{Frame, FsReply, FsRequest, MAX_FRAME, ROOT};

const FRAMING: Framing = Framing::big_endian(MAX_FRAME);

/// A peer that went away, or a frame that did not decode.
pub(crate) fn disconnected(what: &str) -> NotAvailable {
    NotAvailable::ConnectionFailed(io::Error::new(
        io::ErrorKind::ConnectionAborted,
        what.to_owned(),
    ))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A call that waits for its answer.
type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<FsReply, String>>>>;

struct Inner {
    outgoing: tokio::sync::mpsc::UnboundedSender<Frame>,
    pending: Pending,
    transport: UnixStream,
    live: AtomicBool,
    next_call: AtomicU64,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
        // The reader only notices a peer that left; this end must say so.
        let _ = self.transport.shutdown(std::net::Shutdown::Both);
    }
}

/// A client connection; cloning it is cheap and it closes when the last
/// handle, call or task lets go.
pub struct Connection(Arc<Inner>);

impl Connection {
    /// Drive a connected stream on a thread of its own, so callers need no
    /// runtime of their own and calls from any context share one transport.
    ///
    /// # Errors
    ///
    /// Returns a failed connection when the stream cannot be made asynchronous
    /// or the driver thread cannot start.
    pub fn connect(name: &str, stream: UnixStream) -> io::Result<Arc<Self>> {
        let transport = stream.try_clone()?;
        stream.set_nonblocking(true)?;
        let (outgoing, frames) = tokio::sync::mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            outgoing,
            pending: Mutex::new(HashMap::new()),
            transport,
            live: AtomicBool::new(true),
            next_call: AtomicU64::new(1),
        });
        let driveable = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || drive(stream, frames, driveable))?;
        Ok(Arc::new(Self(inner)))
    }

    /// One request to one object, answered or refused. A transport that ended
    /// fails every waiting call instead of blocking it.
    pub(crate) async fn call(
        &self,
        object: u64,
        request: FsRequest,
    ) -> Result<FsReply, NotAvailable> {
        if !self.0.live.load(Ordering::Acquire) {
            return Err(disconnected("the filesystem connection is closed"));
        }
        let call = self.0.next_call.fetch_add(1, Ordering::Relaxed);
        let (answer, receive) = oneshot::channel();
        lock(&self.0.pending).insert(call, answer);
        if self
            .0
            .outgoing
            .send(Frame::Call {
                call,
                object,
                request,
            })
            .is_err()
        {
            lock(&self.0.pending).remove(&call);
            return Err(disconnected("the filesystem connection is closed"));
        }
        match receive.await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(message)) => Err(NotAvailable::Refused(message)),
            Err(_) => Err(disconnected("the filesystem connection ended")),
        }
    }

    /// Whether both ends are still there: a connection whose peer left, or one
    /// this end closed, fails every later call.
    pub fn live(&self) -> bool {
        self.0.live.load(Ordering::Acquire)
    }

    /// Close now instead of when the last handle goes: the serving side sees
    /// the end at once and every waiting call fails.
    pub fn close(&self) {
        self.0.live.store(false, Ordering::Release);
        let _ = self.0.transport.shutdown(std::net::Shutdown::Both);
    }

    /// Tell the serving side an object is no longer held here.
    fn release(&self, object: u64) {
        let _ = self.0.outgoing.send(Frame::Release { object });
    }
}

fn drive(
    stream: UnixStream,
    mut frames: tokio::sync::mpsc::UnboundedReceiver<Frame>,
    connection: std::sync::Weak<Inner>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("filesystem connection could not start: {error}");
            return;
        }
    };
    runtime.block_on(async move {
        let stream = match tokio::net::UnixStream::from_std(stream) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("filesystem stream unusable: {error}");
                return;
            }
        };
        let (read, mut write) = stream.into_split();
        let writer = tokio::spawn(async move {
            while let Some(frame) = frames.recv().await {
                FRAMING.write_value_async(&mut write, &frame).await?;
                tokio::io::AsyncWriteExt::flush(&mut write).await?;
            }
            io::Result::Ok(())
        });
        let mut reader = FrameReader::new(read, FRAMING);
        loop {
            match reader.read_value_async::<Frame>().await {
                Ok(Some(Frame::Reply { call, reply })) => {
                    let Some(connection) = connection.upgrade() else {
                        break;
                    };
                    let answer = lock(&connection.pending).remove(&call);
                    if let Some(answer) = answer {
                        let _ = answer.send(reply);
                    }
                }
                // A caller never receives calls; a release on this side would
                // name an object the peer serves.
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) => {
                    eprintln!("filesystem connection ended: {error}");
                    break;
                }
            }
        }
        // The last handle letting go already shut the stream down; a peer that
        // still left cannot take more calls, so tell everyone still waiting.
        if let Some(connection) = connection.upgrade() {
            connection.live.store(false, Ordering::Release);
            lock(&connection.pending).clear();
        }
        writer.abort();
    });
}

/// A capability on a connection; dropping the last handle releases it.
pub struct Object {
    pub(crate) connection: Arc<Connection>,
    pub(crate) id: u64,
}

impl Object {
    /// The capability the connection was handed over as.
    pub fn root(connection: Arc<Connection>) -> Arc<Self> {
        Arc::new(Self {
            connection,
            id: ROOT,
        })
    }

    /// A capability the serving side answered with.
    pub fn child(connection: Arc<Connection>, id: u64) -> Arc<Self> {
        Arc::new(Self { connection, id })
    }

    /// A capability this object's own connection answered with.
    pub fn capability(&self, id: u64) -> Arc<Self> {
        Self::child(Arc::clone(&self.connection), id)
    }

    /// One request on this capability, answered or refused.
    pub async fn call(&self, request: FsRequest) -> Result<FsReply, NotAvailable> {
        self.connection.call(self.id, request).await
    }
}

impl Drop for Object {
    fn drop(&mut self) {
        if self.id != ROOT {
            self.connection.release(self.id);
        }
    }
}

/// An object the serving side answers calls for.
pub trait ServeObject: Send + Sync + 'static {
    /// Answer one request. `capabilities` registers the objects a reply hands
    /// back, so a returned capability reaches the caller as its id.
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>>;
}

struct Registry {
    objects: RwLock<HashMap<u64, Arc<dyn ServeObject>>>,
    next: AtomicU64,
}

/// The serving side's object table.
#[derive(Clone)]
pub struct Capabilities(Arc<Registry>);

impl Capabilities {
    pub fn new(root: Arc<dyn ServeObject>) -> Self {
        let mut objects = HashMap::new();
        objects.insert(ROOT, root);
        Self(Arc::new(Registry {
            objects: RwLock::new(objects),
            next: AtomicU64::new(ROOT + 1),
        }))
    }

    /// Record `object` and answer the id the peer will call it by.
    pub fn register(&self, object: Arc<dyn ServeObject>) -> u64 {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        self.0
            .objects
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, object);
        id
    }

    fn get(&self, id: u64) -> Option<Arc<dyn ServeObject>> {
        self.0
            .objects
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .cloned()
    }

    /// Forget `object`; its native claims are released as it drops.
    fn release(&self, id: u64) {
        let released = self
            .0
            .objects
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        drop(released);
    }
}

/// Serve `root` to the peer on `stream` until it goes away. Calls are answered
/// concurrently; every object the table holds is released when the connection
/// ends.
///
/// # Errors
///
/// Returns a failed connection when a frame does not decode or the writer stops.
pub async fn serve_connected(
    stream: tokio::net::UnixStream,
    root: Arc<dyn ServeObject>,
) -> io::Result<()> {
    let capabilities = Capabilities::new(root);
    let (read, mut write) = stream.into_split();
    let (outgoing, mut frames) = tokio::sync::mpsc::unbounded_channel::<Frame>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            FRAMING.write_value_async(&mut write, &frame).await?;
            tokio::io::AsyncWriteExt::flush(&mut write).await?;
        }
        io::Result::Ok(())
    });
    let mut reader = FrameReader::new(read, FRAMING);
    let mut ending = None;
    loop {
        match reader.read_value_async::<Frame>().await {
            Ok(Some(Frame::Call {
                call,
                object,
                request,
            })) => {
                let capabilities = capabilities.clone();
                let outgoing = outgoing.clone();
                tokio::spawn(async move {
                    let reply = match capabilities.get(object) {
                        Some(served) => served.dispatch(&capabilities, request).await,
                        None => Err("the capability is no longer held".to_string()),
                    };
                    let _ = outgoing.send(Frame::Reply { call, reply });
                });
            }
            Ok(Some(Frame::Release { object })) => capabilities.release(object),
            Ok(Some(Frame::Reply { .. })) => {}
            Ok(None) => break,
            Err(error) => {
                ending = Some(error);
                break;
            }
        }
    }
    // Closing the queue ends the writer after it flushes what is queued.
    drop(outgoing);
    let written = writer.await.unwrap_or_else(|error| {
        Err(io::Error::other(format!(
            "filesystem writer stopped: {error}"
        )))
    });
    match ending {
        Some(error) => Err(error.into()),
        None => written,
    }
}
