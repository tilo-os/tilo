//! Direct sync of one workspace replica between two of its devices over an
//! iroh connection, with no room host in between.
//!
//! The connection's endpoint id is the remote device key, so the QUIC
//! handshake already proves who sent every frame: a device that is not a
//! certified, unrevoked device of the replica's roster is refused before a
//! frame is read, and everything it sends is admitted as its own live
//! traffic through [`StatePeer::receive_from`], the same admission a room
//! host's relayed delta takes.
//!
//! Each side opens one unidirectional stream of length-prefixed postcard
//! [`NeighbourMsg`] frames. It first carries what the other may lack: the
//! device's own certificate, and when the device belongs to the workspace's
//! owner, the room governance and a checkpoint of every document. Then it
//! carries every message this device publishes while the link lasts.

use crate::{NeighbourMsg, StatePeer, TransportOrigin};
use ed25519_dalek::SigningKey;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use std::sync::{Arc, Mutex};
use tilo_lock::MutexRecover as _;
use tokio::sync::{broadcast, watch};
use web_time::SystemTime;

/// ALPN of a direct replica link between two devices of one workspace.
pub const DIRECT_ALPN: &[u8] = b"tilo-peer-state-direct/1";

/// The largest frame: a document checkpoint with room to spare.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Messages published while a link is writing; a link further behind than
/// this ends, and the next connection catches up from the checkpoint.
const OUTGOING: usize = 1024;

/// Application close code of a refused connection.
const REFUSED: u32 = 1;

/// A device's replica, shared by the device's engine and its direct links.
///
/// Clone it to hand it to an [`iroh::protocol::Router`] under
/// [`DIRECT_ALPN`] (it is the accepting [`ProtocolHandler`]) and to dial with
/// [`Self::connect`]; every clone is the same replica.
#[derive(Clone)]
pub struct DirectSync(Arc<Inner>);

struct Inner {
    replica: Mutex<StatePeer>,
    key: SigningKey,
    outgoing: broadcast::Sender<NeighbourMsg>,
    changed: watch::Sender<u64>,
}

impl std::fmt::Debug for DirectSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectSync")
            .field(
                "device",
                &hex::encode(self.0.key.verifying_key().as_bytes()),
            )
            .finish_non_exhaustive()
    }
}

impl DirectSync {
    /// Share `replica`, the replica of the device whose signing key is `key`
    /// (and whose iroh endpoint's secret key it is).
    #[must_use]
    pub fn new(replica: StatePeer, key: SigningKey) -> Self {
        Self(Arc::new(Inner {
            replica: Mutex::new(replica),
            key,
            outgoing: broadcast::channel(OUTGOING).0,
            changed: watch::channel(0).0,
        }))
    }

    /// Read the replica.
    pub fn read<R>(&self, read: impl FnOnce(&StatePeer) -> R) -> R {
        read(&self.0.replica.lock_recover())
    }

    /// Make a local change with the device's key, e.g.
    /// `publication_mut().publish_incremental`, and send the message it
    /// returns to every open link.
    pub fn publish(
        &self,
        write: impl FnOnce(&mut StatePeer, &SigningKey, SystemTime) -> Result<NeighbourMsg, String>,
    ) -> Result<(), String> {
        let message = write(
            &mut self.0.replica.lock_recover(),
            &self.0.key,
            SystemTime::now(),
        )?;
        self.0.changed.send_modify(bump);
        // No open link is no error: the next one catches up from the replica.
        let _ = self.0.outgoing.send(message);
        Ok(())
    }

    /// Ticks whenever the replica changed, by a local change or a frame a
    /// link admitted.
    #[must_use]
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.0.changed.subscribe()
    }

    /// Dial the device at `addr` from `endpoint` and sync with it until the
    /// link ends.
    ///
    /// # Errors
    /// The device is not in the roster, refused this one, or the link broke.
    pub async fn connect(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
    ) -> Result<(), String> {
        let connection = endpoint
            .connect(addr, DIRECT_ALPN)
            .await
            .map_err(|error| error.to_string())?;
        self.run(&connection).await
    }

    /// Sync with the device on the other end of `connection` until the link
    /// ends, whichever side dialled.
    async fn run(&self, connection: &Connection) -> Result<(), String> {
        let remote = *connection.remote_id().as_bytes();
        if let Err(reason) = self.admits(remote) {
            connection.close(REFUSED.into(), reason.as_bytes());
            return Err(reason);
        }
        // Subscribe before taking the catch-up, so a change published in
        // between is sent after it rather than lost.
        let live = self.0.outgoing.subscribe();
        let catch_up = self.catch_up()?;
        let send = connection
            .open_uni()
            .await
            .map_err(|error| error.to_string())?;
        let receiving = self.receive_all(remote, connection);
        let sending = send_all(send, catch_up, live);
        tokio::select! {
            result = receiving => result,
            result = sending => result,
        }
    }

    /// Whether `device` may sync with this replica: a certified device of
    /// its roster, unrevoked and unexpired.
    fn admits(&self, device: [u8; 32]) -> Result<(), String> {
        let replica = self.0.replica.lock_recover();
        if replica
            .revocations()
            .iter()
            .any(|entry| entry.revoked_device_public_key == device)
        {
            return Err("the device is revoked".into());
        }
        let certificate = replica
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device);
        drop(replica);
        let certificate = certificate.ok_or("the device is not in the workspace roster")?;
        let now = now_ns();
        if certificate
            .expires_at_ns
            .is_some_and(|expiry| expiry <= now)
        {
            return Err("the device's certificate expired".into());
        }
        Ok(())
    }

    /// What the other device may lack: this device's certificate and the
    /// revocations its person signed, then, from a device of the workspace's
    /// owner, the governance and a checkpoint of every document. A member
    /// device of another person cannot sign checkpoints others' writes
    /// depend on; its own writes reach the other device live.
    fn catch_up(&self) -> Result<Vec<NeighbourMsg>, String> {
        let device = self.0.key.verifying_key().to_bytes();
        let mut replica = self.0.replica.lock_recover();
        let cert = replica
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device)
            .ok_or("this device is not in the workspace roster")?;
        let revocations = replica
            .revocations()
            .into_iter()
            .filter(|entry| entry.user_public_key == cert.user_public_key)
            .collect();
        let mut messages = vec![NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations,
        }];
        if replica.has_governance() && replica.signed_by_owner(&device) {
            messages.extend(
                replica
                    .publication_mut()
                    .bootstrap_messages(&self.0.key, SystemTime::now())?,
            );
        }
        drop(replica);
        Ok(messages)
    }

    /// Admit every frame `remote` sends until its stream ends. A frame the
    /// replica refuses is that message's verdict, not the link's: the link
    /// stays up for the frames after it.
    async fn receive_all(&self, remote: [u8; 32], connection: &Connection) -> Result<(), String> {
        let mut recv = connection
            .accept_uni()
            .await
            .map_err(|error| error.to_string())?;
        while let Some(message) = read_frame(&mut recv).await? {
            let admitted = self.0.replica.lock_recover().receive_from(
                TransportOrigin::RelayDelta(remote),
                message,
                SystemTime::now(),
            );
            if admitted.is_ok_and(|outcome| outcome.changed || outcome.metadata_changed) {
                self.0.changed.send_modify(bump);
            }
        }
        Ok(())
    }
}

impl ProtocolHandler for DirectSync {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.run(&connection)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

fn bump(count: &mut u64) {
    *count = count.wrapping_add(1);
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_nanos()).ok())
        .unwrap_or(u64::MAX)
}

/// Write `catch_up`, then every message published live, to `send`.
async fn send_all(
    mut send: SendStream,
    catch_up: Vec<NeighbourMsg>,
    mut live: broadcast::Receiver<NeighbourMsg>,
) -> Result<(), String> {
    for message in &catch_up {
        write_frame(&mut send, message).await?;
    }
    loop {
        match live.recv().await {
            Ok(message) => write_frame(&mut send, &message).await?,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("the link fell behind the device's changes".into());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn write_frame(send: &mut SendStream, message: &NeighbourMsg) -> Result<(), String> {
    let bytes = postcard::to_allocvec(message).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    let len = u32::try_from(bytes.len()).map_err(|_| "frame too large".to_string())?;
    send.write_all(&len.to_le_bytes())
        .await
        .map_err(|error| error.to_string())?;
    send.write_all(&bytes)
        .await
        .map_err(|error| error.to_string())
}

/// The next frame, or `None` once the other side finished its stream.
async fn read_frame(recv: &mut RecvStream) -> Result<Option<NeighbourMsg>, String> {
    let mut len = [0_u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let len =
        usize::try_from(u32::from_le_bytes(len)).map_err(|_| "frame too large".to_string())?;
    if len > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    let mut bytes = vec![0_u8; len];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    postcard::from_bytes(&bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}
