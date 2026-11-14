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
//! [`NeighbourMsg`] frames. It first carries who this device is: its
//! certificate and the revocations its identity signed. Nothing of the
//! person's documents follows until the store holds a certificate that makes
//! the other device one of theirs — one it held already, or the one that
//! device shows on this link. Then it carries a snapshot of every document
//! this device holds, and every write it publishes while the link lasts and
//! the other stays one of the person's devices. Whoever holds a document can
//! hand over its whole state, so a device added later catches up from any of
//! its siblings.
//!
//! The store's own record of the person's devices travels the same way, to a
//! device the store already holds a certificate of and to no other: it is
//! published again whenever the certificates this device holds move, so a
//! device learns of the person's other devices from whichever of them it
//! meets.

// Nothing compiled for the web dials the person's own store yet: the page's
// session opens its links with its own wiring.
#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's session wires these")
)]

use super::PeerPersonal;
use crate::NeighbourMsg;
use crate::frames::{OUTGOING, bump};
#[cfg(not(target_arch = "wasm32"))]
use crate::frames::{REFUSED, from_device, read_message, send_all, write_message};
use collab_types::signer::DeviceSigner;
#[cfg(not(target_arch = "wasm32"))]
use iroh::endpoint::Connection;
#[cfg(not(target_arch = "wasm32"))]
use iroh::protocol::{AcceptError, ProtocolHandler};
#[cfg(not(target_arch = "wasm32"))]
use iroh::{Endpoint, EndpointAddr};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use web_time::SystemTime;

/// ALPN of a personal store's link between two devices of one person.
pub const PERSONAL_ALPN: &[u8] = b"tilo-personal-store/1";

/// A write a link admitted from another of the person's devices: the signed
/// op, for the record it changes.
///
/// The host hands it to the apps that have the record open, so a sibling's
/// edit arrives as any other device's does. Nothing else of the link's
/// traffic is a document: the certificates and revocations stay the store's.
#[derive(Clone, Debug)]
pub struct AdmittedFrame {
    pub app: String,
    pub record: String,
    pub envelope: collab_types::SignedOp,
}

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
    key: Arc<dyn DeviceSigner>,
    outgoing: broadcast::Sender<NeighbourMsg>,
    admitted: broadcast::Sender<AdmittedFrame>,
    changed: watch::Sender<u64>,
}

impl std::fmt::Debug for PersonalSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalSync")
            .field("device", &hex::encode(self.0.key.device_public_key()))
            .finish_non_exhaustive()
    }
}

impl PersonalSync {
    /// Share `store`, held by the device whose key `key` signs with (and
    /// whose iroh endpoint's secret key it is).
    #[must_use]
    pub fn new(store: super::PersonalStore, key: impl DeviceSigner + 'static) -> Self {
        Self::over(PeerPersonal::new(store), key)
    }

    /// Share `store`, which other parts of the engine read as well.
    #[must_use]
    pub fn over(store: PeerPersonal, key: impl DeviceSigner + 'static) -> Self {
        Self(Arc::new(Inner {
            store,
            key: Arc::new(key),
            outgoing: broadcast::channel(OUTGOING).0,
            admitted: broadcast::channel(OUTGOING).0,
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
            &dyn DeviceSigner,
            SystemTime,
        ) -> Result<NeighbourMsg, String>,
    ) -> Result<NeighbourMsg, String> {
        let now = tilo_clock::wall();
        let message = self
            .0
            .store
            .write(|store| publish(store, self.0.key.as_ref(), now));
        self.publish_refusals();
        Ok(self.published(message?))
    }

    /// Send every refusal set this device's verdicts moved into since the
    /// last message, the ones it no longer names included: a sibling that
    /// was refused a write hears of it, and hears when it stands.
    fn publish_refusals(&self) -> bool {
        let updates = self
            .0
            .store
            .write(super::PersonalStore::take_refusal_updates);
        for update in &updates {
            let _ = self.0.outgoing.send(update.clone());
        }
        !updates.is_empty()
    }

    /// Admit a write this device signed itself and send it to every open
    /// link: what an app publishing its own record does.
    pub fn admit_local(
        &self,
        app: &str,
        record: &str,
        envelope: collab_types::SignedOp,
    ) -> Result<NeighbourMsg, String> {
        let message = self.0.store.write(|store| {
            store.admit_local(
                app,
                record,
                envelope,
                self.0.key.as_ref(),
                tilo_clock::wall(),
            )
        });
        self.publish_refusals();
        Ok(self.published(message?))
    }

    /// Apply a change to the store that is not a document of it: the devices
    /// the person's identity certified, and the revocations it signed. Sends
    /// the hello that carries them to every open link, and returns whether
    /// the store changed.
    pub fn admit(
        &self,
        admit: impl FnOnce(&mut super::PersonalStore, SystemTime) -> Result<bool, String>,
    ) -> Result<bool, String> {
        let changed = self
            .0
            .store
            .write(|store| admit(store, tilo_clock::wall()))?;
        self.publish_refusals();
        if changed {
            self.published(self.0.store.read(super::PersonalStore::hello));
        }
        self.publish_devices();
        Ok(changed)
    }

    /// Send the record of the person's devices to every open link whose
    /// device is one of them, when the certificates this device holds moved
    /// since it last was: what a sibling that never met those devices learns
    /// of them from.
    fn publish_devices(&self) {
        let update = self
            .0
            .store
            .write(|store| store.take_devices_update(self.0.key.as_ref(), tilo_clock::wall()));
        match update {
            Ok(Some(message)) => {
                self.published(message);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "the record of the person's devices was not published");
            }
        }
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

    /// Every write a link admits from another of the person's devices, from
    /// now on. A receiver that falls behind loses frames and catches up on
    /// its next link.
    #[must_use]
    pub fn admitted(&self) -> broadcast::Receiver<AdmittedFrame> {
        self.0.admitted.subscribe()
    }

    /// Dial the device at `addr` from `endpoint` and sync with it until the
    /// link ends.
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    async fn run(&self, connection: &Connection) -> Result<(), String> {
        let remote = *connection.remote_id().as_bytes();
        // What this device holds of the person's devices is in their record
        // before the catch-up is taken from it.
        self.publish_devices();
        // Subscribe before anything is taken from the store, so a write
        // published in between is sent after it rather than lost.
        let live = self.0.outgoing.subscribe();
        let changes = self.0.changed.subscribe();
        let send = connection
            .open_uni()
            .await
            .map_err(|error| error.to_string())?;
        let receiving = self.receive_all(remote, connection);
        let sending = self.send_to(send, remote, live, changes);
        tokio::select! {
            result = receiving => result,
            result = sending => result,
        }
    }

    /// Say who this device is, and nothing of the person's documents until
    /// the store holds a certificate that makes `remote` one of their
    /// devices: one it held when the link started, or the one `remote` shows
    /// on it. Its catch-up follows then, on this link, and after it every
    /// write published while it stays one of them.
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_to<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        mut send: W,
        remote: [u8; 32],
        mut live: broadcast::Receiver<NeighbourMsg>,
        mut changes: watch::Receiver<u64>,
    ) -> Result<(), String> {
        let hands = |message: &NeighbourMsg| {
            self.0
                .store
                .read(|store| store.hands(message, &remote, tilo_clock::wall()))
        };
        let greeting = self.0.store.read(|store| store.greeting(&remote));
        for message in &greeting {
            write_message(&mut send, message).await?;
        }
        let documents = loop {
            if let Some(documents) = self.documents_for(remote)? {
                break documents;
            }
            tokio::select! {
                moved = changes.changed() => {
                    if moved.is_err() {
                        return Ok(());
                    }
                }
                message = live.recv() => match message {
                    // A write published before the catch-up is in the
                    // catch-up, when there is one: a device not yet one of
                    // the person's is sent neither.
                    Ok(message)
                        if matches!(message, NeighbourMsg::AppDocument { .. })
                            || from_device(&message, remote)
                            || !hands(&message) => {}
                    Ok(message) => write_message(&mut send, &message).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return Err("the link fell behind the device's changes".into());
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        };
        send_all(send, remote, documents, live, hands).await
    }

    /// A snapshot of every document this device holds, for `remote` to catch
    /// up from, once the store holds a certificate that makes it one of the
    /// person's devices now; `None` until then.
    #[cfg(not(target_arch = "wasm32"))]
    fn documents_for(&self, remote: [u8; 32]) -> Result<Option<Vec<NeighbourMsg>>, String> {
        self.0
            .store
            .write(|store| store.documents_for(self.0.key.as_ref(), &remote, tilo_clock::wall()))
    }

    /// Admit every frame `remote` sends until its stream ends. A document the
    /// store refuses is that write's verdict, not the link's: the link stays
    /// up for the frames after it. A device whose identity the store does not
    /// admit loses the link.
    #[cfg(not(target_arch = "wasm32"))]
    async fn receive_all(&self, remote: [u8; 32], connection: &Connection) -> Result<(), String> {
        let mut recv = connection
            .accept_uni()
            .await
            .map_err(|error| error.to_string())?;
        while let Some(message) = read_message(&mut recv).await? {
            let identity = matches!(message, NeighbourMsg::IdentityHello { .. });
            let frame = admitted_frame(&message);
            let admitted = self.0.store.write(|store| {
                store.receive(self.0.key.as_ref(), remote, message, tilo_clock::wall())
            });
            // Whatever the store decided, what it now refuses of this
            // device's writes is sent: the verdict reaches its author, live.
            if self.publish_refusals() {
                self.0.changed.send_modify(bump);
            }
            // A device this one just met, or learned of, is one its other
            // siblings may lack: their record is sent with it.
            self.publish_devices();
            match admitted {
                Ok(outcome) => {
                    if outcome.changed || outcome.metadata_changed {
                        self.0.changed.send_modify(bump);
                    }
                    if outcome.changed
                        && let Some(frame) = frame
                    {
                        // No local app having the record open is no error: the
                        // store has it for when one opens it.
                        let _ = self.0.admitted.send(frame);
                    }
                }
                Err(reason) if identity => {
                    connection.close(REFUSED.into(), reason.as_bytes());
                    return Err(reason);
                }
                Err(reason) => {
                    // The store's verdict on this one write, not on the
                    // link: it stays out of this device, and the store filed
                    // a refusal the author is told of.
                    tracing::warn!(remote = %hex::encode(remote), %reason, "a peer's write was refused");
                }
            }
        }
        Ok(())
    }
}

/// The document `message` carries, as local apps are handed it.
fn admitted_frame(message: &NeighbourMsg) -> Option<AdmittedFrame> {
    let NeighbourMsg::AppDocument {
        app_id,
        record_id,
        envelope,
        ..
    } = message
    else {
        return None;
    };
    // The record of the person's devices is the store's own: no app has it.
    if super::PersonalStore::is_devices_record(app_id, record_id) {
        return None;
    }
    Some(AdmittedFrame {
        app: app_id.clone(),
        record: record_id.clone(),
        envelope: envelope.as_ref().clone(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
impl ProtocolHandler for PersonalSync {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.run(&connection)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}
