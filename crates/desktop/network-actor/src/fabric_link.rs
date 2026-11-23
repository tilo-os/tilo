//! The way the person's device fabric takes to one of their other devices:
//! the connection of the session between the two ([`super::device_sessions`]).
//! Each frame goes on a bidirectional stream of its own
//! ([`super::read_personal_fabric`] reads them on the other side), in the
//! order they were sent; a file transfer streams on the same connection.
use iroh::EndpointId;
use iroh::endpoint::Connection;
use std::time::Duration;
use tokio::sync::mpsc;

/// Frames waiting for one device. Beyond it the device is not keeping up,
/// and each side asks the other for its fabric anew when it is back.
const FRAMES: usize = 64;
/// How long one frame may take to write.
const FRAME_WRITE: Duration = Duration::from_millis(100);

/// The fabric's way to one device, with the task that writes its frames in
/// order.
pub(super) struct FabricLink {
    frames: mpsc::Sender<Vec<u8>>,
    connection: Connection,
    task: tokio::task::JoinHandle<()>,
}

impl FabricLink {
    pub(super) fn new(connection: Connection) -> Self {
        let (frames, mut queued) = mpsc::channel::<Vec<u8>>(FRAMES);
        let writer = connection.clone();
        let task = tokio::spawn(async move {
            while let Some(bytes) = queued.recv().await {
                send_frame(writer.remote_id(), &writer, &bytes).await;
            }
        });
        Self {
            frames,
            connection,
            task,
        }
    }

    /// Queue `bytes`, one encoded frame, for the device.
    pub(super) fn send(&self, bytes: Vec<u8>) {
        if self.frames.try_send(bytes).is_err() {
            tracing::debug!(peer = %self.connection.remote_id(), "personal fabric frames to a device are backed up; one is dropped");
        }
    }

    /// The connection, for a transfer streaming on it.
    pub(super) fn connection(&self) -> Connection {
        self.connection.clone()
    }
}

impl Drop for FabricLink {
    fn drop(&mut self) {
        self.task.abort();
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
