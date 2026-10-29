//! The direct link the person's device fabric takes to one of their other
//! devices: a connection of the device plane (`SURFACE_STREAM_ALPN`), dialled
//! when a frame is first sent and closed once it has carried nothing for
//! [`LINK_IDLE`]: an open connection keeps both devices waking for its
//! keep-alives, and the person's topics already travel on their sessions.
//! Each frame goes on a bidirectional stream of its own
//! ([`super::read_personal_fabric`] reads them on the other side), in the
//! order they were sent; a file transfer streams on the same connection and
//! keeps it open while it lasts.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::sync::mpsc;

use super::{InternalEvent, SURFACE_STREAM_ALPN, read_personal_fabric};

/// Frames waiting for one device. Beyond it the device is not keeping up,
/// and each side asks the other for its fabric anew when it is back.
const FRAMES: usize = 64;
/// How long bringing the connection up may take.
const DIAL: Duration = Duration::from_secs(10);
/// How long one frame may take to write.
const FRAME_WRITE: Duration = Duration::from_millis(100);
/// How long a link with no frame to write and no transfer on it stays open:
/// one keep-alive interval of the connection, so an idle link closes
/// rather than keep pinging.
const LINK_IDLE: Duration = Duration::from_secs(5);

/// The link to one device, with the task that writes its frames in order.
pub(super) struct FabricLink {
    frames: mpsc::Sender<Vec<u8>>,
    dialer: Dialer,
    task: tokio::task::JoinHandle<()>,
}

impl FabricLink {
    pub(super) fn new(
        endpoint: Endpoint,
        addr: EndpointAddr,
        internal_tx: mpsc::Sender<InternalEvent>,
        incarnations: Arc<AtomicU64>,
    ) -> Self {
        let dialer = Dialer {
            endpoint,
            addr,
            connection: Arc::default(),
            streams: Arc::default(),
            internal_tx,
            incarnations,
        };
        let (frames, mut queued) = mpsc::channel::<Vec<u8>>(FRAMES);
        let writer = dialer.clone();
        let task = tokio::spawn(async move {
            loop {
                let next = if writer.is_open().await {
                    tokio::time::timeout(LINK_IDLE, queued.recv()).await
                } else {
                    Ok(queued.recv().await)
                };
                let bytes = match next {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => break,
                    Err(_) => {
                        writer.close_if_unused().await;
                        continue;
                    }
                };
                match writer.connection().await {
                    Some(connection) => send_frame(writer.addr.id, &connection, &bytes).await,
                    None => {
                        tracing::debug!(peer = %writer.addr.id, "no direct link to carry a personal fabric frame");
                    }
                }
            }
        });
        Self {
            frames,
            dialer,
            task,
        }
    }

    /// Queue `bytes`, one encoded frame, for the device.
    pub(super) fn send(&self, bytes: Vec<u8>) {
        if self.frames.try_send(bytes).is_err() {
            tracing::debug!(peer = %self.dialer.addr.id, "personal fabric frames to a device are backed up; one is dropped");
        }
    }

    /// What brings the link's connection up, for a stream of its own.
    pub(super) fn dialer(&self) -> Dialer {
        self.dialer.clone()
    }
}

impl Drop for FabricLink {
    fn drop(&mut self) {
        self.task.abort();
        if let Ok(mut connection) = self.dialer.connection.try_lock()
            && let Some(connection) = connection.take()
        {
            connection.close(0_u32.into(), b"no longer a member");
        }
    }
}

/// Brings one device's connection up, or answers the one already up.
#[derive(Clone)]
pub(super) struct Dialer {
    endpoint: Endpoint,
    addr: EndpointAddr,
    connection: Arc<tokio::sync::Mutex<Option<Connection>>>,
    /// Transfers streaming on the connection now ([`Lease`]).
    streams: Arc<AtomicUsize>,
    internal_tx: mpsc::Sender<InternalEvent>,
    incarnations: Arc<AtomicU64>,
}

impl Dialer {
    /// The open connection for a transfer of its own, which keeps the link
    /// open until the returned [`Lease`] is dropped.
    pub(super) async fn lease(&self) -> Option<(Connection, Lease)> {
        let lease = Lease(Arc::clone(&self.streams));
        lease.0.fetch_add(1, Ordering::AcqRel);
        Some((self.connection().await?, lease))
    }

    async fn is_open(&self) -> bool {
        self.connection
            .lock()
            .await
            .as_ref()
            .is_some_and(|connection| connection.close_reason().is_none())
    }

    /// Close the connection unless a transfer streams on it.
    async fn close_if_unused(&self) {
        let mut held = self.connection.lock().await;
        if self.streams.load(Ordering::Acquire) == 0
            && let Some(connection) = held.take()
        {
            connection.close(0_u32.into(), b"idle");
        }
    }

    /// The open connection to the device, dialled now when there is none.
    /// What the device sends back on it is read like what arrives on one it
    /// dialled.
    pub(super) async fn connection(&self) -> Option<Connection> {
        let mut held = self.connection.lock().await;
        if let Some(connection) = held.as_ref()
            && connection.close_reason().is_none()
        {
            return Some(connection.clone());
        }
        let connection = tokio::time::timeout(
            DIAL,
            self.endpoint
                .connect(self.addr.clone(), SURFACE_STREAM_ALPN),
        )
        .await
        .ok()?
        .inspect_err(|error| {
            tracing::debug!(peer = %self.addr.id, %error, "direct link to a device failed to open");
        })
        .ok()?;
        let incarnation = self.incarnations.fetch_add(1, Ordering::Relaxed).max(1);
        read_personal_fabric(connection.clone(), self.internal_tx.clone(), incarnation);
        *held = Some(connection.clone());
        Some(connection)
    }
}

/// A transfer streaming on a link's connection, which stays open while it
/// lasts.
pub(super) struct Lease(Arc<AtomicUsize>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Write one frame to `peer` on a bidirectional stream of `connection`.
async fn send_frame(peer: EndpointId, connection: &Connection, bytes: &[u8]) {
    let send = async {
        let (mut stream, _) = connection.open_bi().await.map_err(|_| ())?;
        stream.write_all(bytes).await.map_err(|_| ())?;
        stream.finish().map_err(|_| ())?;
        Ok::<(), ()>(())
    };
    if !matches!(tokio::time::timeout(FRAME_WRITE, send).await, Ok(Ok(()))) {
        tracing::debug!(peer = %peer, "personal fabric frame not sent");
    }
}
