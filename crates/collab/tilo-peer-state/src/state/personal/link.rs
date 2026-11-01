//! One person's documents between two of their devices over iroh, with
//! nothing in between.
//!
//! The connection's endpoint id is the remote device key, so the QUIC
//! handshake already proves which device sent every frame. A device is
//! admitted when the certificate its first frame carries was signed by the
//! identity root the store opens on, is current, is not revoked and is not a
//! store node's; everything it says is then admitted as its own live write. A
//! device that presents anything else is refused the link.
//!
//! Each side opens one unidirectional stream of length-prefixed postcard
//! [`NeighbourMsg`] frames. It first carries what the other may lack: this
//! device's certificate, the revocations its identity signed, and a snapshot
//! of every document it holds. Then it carries every write this device
//! publishes while the link lasts. Whoever holds a document can hand over its
//! whole state, so a device added later catches up from any of its siblings.

use super::PeerPersonal;
use crate::NeighbourMsg;
use crate::direct::{OUTGOING, REFUSED, bump, read_frame, send_all};
use ed25519_dalek::SigningKey;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use web_time::SystemTime;

/// ALPN of a personal store's link between two devices of one person.
pub const PERSONAL_ALPN: &[u8] = b"tilo-personal-store/1";

/// A device's store, shared by the device's engine and its links to the
/// person's other devices.
///
/// Clone it to hand it to an [`iroh::protocol::Router`] under
/// [`PERSONAL_ALPN`] (it is the accepting [`ProtocolHandler`]) and to dial
/// with [`Self::connect`]; every clone is the same store.
#[derive(Clone)]
pub struct PersonalSync(Arc<Inner>);

struct Inner {
    store: PeerPersonal,
    key: SigningKey,
    outgoing: broadcast::Sender<NeighbourMsg>,
    changed: watch::Sender<u64>,
}

impl std::fmt::Debug for PersonalSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalSync")
            .field(
                "device",
                &hex::encode(self.0.key.verifying_key().as_bytes()),
            )
            .finish_non_exhaustive()
    }
}

impl PersonalSync {
    /// Share `store`, held by the device whose signing key is `key` (and
    /// whose iroh endpoint's secret key it is).
    #[must_use]
    pub fn new(store: super::PersonalStore, key: SigningKey) -> Self {
        Self::over(PeerPersonal::new(store), key)
    }

    /// Share `store`, which other parts of the engine read as well.
    #[must_use]
    pub fn over(store: PeerPersonal, key: SigningKey) -> Self {
        Self(Arc::new(Inner {
            store,
            key,
            outgoing: broadcast::channel(OUTGOING).0,
            changed: watch::channel(0).0,
        }))
    }

    /// The store this link syncs.
    #[must_use]
    pub fn store(&self) -> PeerPersonal {
        self.0.store.clone()
    }

    /// Read the store.
    pub fn read<R>(&self, read: impl FnOnce(&super::PersonalStore) -> R) -> R {
        self.0.store.read(read)
    }

    /// Make a local write with the device's key, send the frame it returns to
    /// every open link, and return it.
    pub fn publish(
        &self,
        publish: impl FnOnce(
            &mut super::PersonalStore,
            &SigningKey,
            SystemTime,
        ) -> Result<NeighbourMsg, String>,
    ) -> Result<NeighbourMsg, String> {
        let message = self
            .0
            .store
            .write(|store| publish(store, &self.0.key, SystemTime::now()))?;
        Ok(self.published(message))
    }

    /// Admit a write this device signed itself and send it to every open
    /// link: what an app publishing its own record does.
    pub fn admit_local(
        &self,
        app: &str,
        record: &str,
        envelope: collab_types::SignedOp,
    ) -> Result<NeighbourMsg, String> {
        let message = self
            .0
            .store
            .write(|store| store.admit_local(app, record, envelope, SystemTime::now()))?;
        Ok(self.published(message))
    }

    fn published(&self, message: NeighbourMsg) -> NeighbourMsg {
        self.0.changed.send_modify(bump);
        // No open link is no error: the next one catches up from the store.
        let _ = self.0.outgoing.send(message.clone());
        message
    }

    /// Ticks whenever the store changed, by a local write or a frame a link
    /// admitted.
    #[must_use]
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.0.changed.subscribe()
    }

    /// Dial the device at `addr` from `endpoint` and sync with it until the
    /// link ends.
    pub async fn connect(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
    ) -> Result<(), String> {
        let connection = endpoint
            .connect(addr, PERSONAL_ALPN)
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
        // Subscribe before taking the catch-up, so a write published in
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

    /// Whether `device` is admitted to a link at all: one this store revoked
    /// is not. A device it has not met is admitted to the connection alone, so
    /// it can present the certificate its first frame carries.
    fn admits(&self, device: [u8; 32]) -> Result<(), String> {
        self.0
            .store
            .read(|store| store.admits_device(&device, SystemTime::now()))
    }

    /// What the other device may lack: this device's certificate, the
    /// revocations its identity signed, and a snapshot of every document it
    /// holds.
    fn catch_up(&self) -> Result<Vec<NeighbourMsg>, String> {
        self.0
            .store
            .write(|store| store.catch_up(&self.0.key, SystemTime::now()))
    }

    /// Admit every frame `remote` sends until its stream ends. A document the
    /// store refuses is that write's verdict, not the link's: the link stays
    /// up for the frames after it. A device whose identity the store does not
    /// admit loses the link.
    async fn receive_all(&self, remote: [u8; 32], connection: &Connection) -> Result<(), String> {
        let mut recv = connection
            .accept_uni()
            .await
            .map_err(|error| error.to_string())?;
        while let Some(message) = read_frame(&mut recv).await? {
            let identity = matches!(message, NeighbourMsg::IdentityHello { .. });
            let admitted = self
                .0
                .store
                .write(|store| store.receive(remote, message, SystemTime::now()));
            match admitted {
                Ok(outcome) => {
                    if outcome.changed || outcome.metadata_changed {
                        self.0.changed.send_modify(bump);
                    }
                }
                Err(reason) if identity => {
                    connection.close(REFUSED.into(), reason.as_bytes());
                    return Err(reason);
                }
                Err(_) => {}
            }
        }
        Ok(())
    }
}

impl ProtocolHandler for PersonalSync {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.run(&connection)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}
