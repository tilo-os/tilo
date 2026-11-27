//! The person's speakers and remote audio between their devices, carried by this device's daemon.
//!
//! The session holds no endpoint of its own, so a route it starts or answers travels here: the
//! session asks the daemon to hand one envelope to another of the person's devices, the daemon
//! writes it on the one connection it keeps to that device, and the receiving daemon hands what
//! arrives to its own session ([`tilo_session_client::speakers`]).
//!
//! Only the person's own certified devices take part: a link is admitted when the key it proves is
//! one the record of their devices lists, and every envelope is checked to be the source's and
//! this device's alone. Nothing here binds an endpoint of its own: this daemon holds the one.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use tilo_lock::MutexRecover as _;
use tilo_network_actor::fabric::{DeviceId, MAX_FABRIC_ACTION_BYTES, PersonalFabricActionEnvelope};
use tilo_session_client::speakers::{self, Frame};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::person_workspace::devices;

/// The person's speakers and audio routes, served on the device's endpoint and asked for over a
/// shell's link.
pub(crate) struct Speakers {
    endpoint: Endpoint,
    /// The person whose devices these are.
    person: [u8; 32],
    /// This device, as the others name it.
    own: [u8; 32],
    /// The record of the person's devices, where a peer is reached and who may take part.
    copies: Workspaces,
    /// The connection to each of the person's other devices, by key.
    peers: tokio::sync::Mutex<BTreeMap<[u8; 32], Arc<Peer>>>,
    /// The link this device's shell opened, while it stands: what an arriving envelope is
    /// handed to.
    link: Mutex<Option<Arc<tokio::sync::Mutex<LinkWriter>>>>,
}

/// One of the person's other devices, as this daemon reaches it.
struct Peer {
    send: tokio::sync::Mutex<SendStream>,
}

type LinkWriter = tokio::io::WriteHalf<tilo_socket::tokio::Stream>;

impl std::fmt::Debug for Speakers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Speakers")
            .field("device", &hex::encode(self.own))
            .finish_non_exhaustive()
    }
}

/// The handler this device answers the speakers protocol with.
#[derive(Debug)]
struct Handler(Arc<Speakers>);

impl iroh::protocol::ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        Arc::clone(&self.0).answer(connection).await;
        Ok(())
    }
}

impl Speakers {
    /// Serve the person's speakers on `endpoint`, for `person`'s devices, reaching each where
    /// their record says, as this device `own`.
    #[must_use]
    pub(crate) fn new(
        endpoint: Endpoint,
        own: [u8; 32],
        person: [u8; 32],
        copies: Workspaces,
    ) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            person,
            own,
            copies,
            peers: tokio::sync::Mutex::default(),
            link: Mutex::default(),
        })
    }

    /// What this device answers on its endpoint beside its other protocols.
    pub(crate) fn protocols(
        self: &Arc<Self>,
    ) -> Vec<(&'static [u8], Box<dyn iroh::protocol::DynProtocolHandler>)> {
        vec![(
            speakers::ALPN,
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(Handler(Arc::clone(self))),
        )]
    }

    /// Serve one link a shell of this device opened until it ends: what the session sends to the
    /// person's other devices.
    pub(crate) async fn serve_link(self: &Arc<Self>, stream: tilo_socket::tokio::Stream) {
        let (mut reader, writer) = tokio::io::split(stream);
        *self.link.lock_recover() = Some(Arc::new(tokio::sync::Mutex::new(writer)));
        loop {
            match speakers::read(&mut reader).await {
                Ok(Frame::Send { peer, envelope }) => {
                    if let Err(error) = Arc::clone(self).send(peer, envelope).await {
                        tracing::debug!(%error, "a speaker message did not reach the other device");
                    }
                }
                Ok(Frame::Heard { .. }) => {
                    tracing::debug!("a shell sent its own device a heard frame");
                }
                Err(error) => {
                    tracing::debug!(%error, "the shell's speakers link ended");
                    break;
                }
            }
        }
        *self.link.lock_recover() = None;
    }

    /// Take `envelope`, one device of the person's, to `peer`, on the connection this daemon
    /// keeps to it.
    async fn send(self: Arc<Self>, peer: [u8; 32], envelope: Vec<u8>) -> Result<(), String> {
        let checked = self.checked(&peer, &envelope)?;
        let envelope = postcard::to_allocvec(&checked)
            .map_err(|error| format!("the envelope does not encode: {error}"))?;
        let target = peer;
        let peer = self.peer(peer).await?;
        let mut send = peer.send.lock().await;
        speakers::write(
            &mut *send,
            &Frame::Send {
                peer: target,
                envelope,
            },
        )
        .await
        .map_err(|error| error.to_string())
    }

    /// `envelope`, read and checked to be this device's to `target`: what is written, so a shell
    /// cannot hand another device's words on.
    fn checked(
        &self,
        target: &[u8; 32],
        envelope: &[u8],
    ) -> Result<PersonalFabricActionEnvelope, String> {
        if envelope.len() > MAX_FABRIC_ACTION_BYTES {
            return Err("an envelope is larger than one link carries".to_owned());
        }
        let envelope: PersonalFabricActionEnvelope =
            postcard::from_bytes(envelope).map_err(|error| error.to_string())?;
        let own = DeviceId::new(hex::encode(self.own)).map_err(|_| "this device has no key")?;
        let target = DeviceId::new(hex::encode(target)).map_err(|_| "that is not a device")?;
        envelope
            .validate_at(&own, &target, tilo_clock::unix_now_ms())
            .map_err(|error| error.to_string())?;
        Ok(envelope)
    }

    /// The connection to `device`, dialled where its record says it answers when this device
    /// holds none.
    async fn peer(self: &Arc<Self>, device: [u8; 32]) -> Result<Arc<Peer>, String> {
        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.get(&device) {
            return Ok(Arc::clone(peer));
        }
        let id = EndpointId::from_bytes(&device).map_err(|_| "that is not a device key")?;
        let addrs = self.endpoints(device).await;
        let addr = EndpointAddr::from_parts(id, addrs.into_iter().map(TransportAddr::Ip));
        let connection = self
            .endpoint
            .connect(addr, speakers::ALPN)
            .await
            .map_err(|error| format!("the other device did not answer: {error}"))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| format!("the other device opened no stream: {error}"))?;
        let peer = Arc::new(Peer {
            send: tokio::sync::Mutex::new(send),
        });
        peers.insert(device, Arc::clone(&peer));
        drop(peers);
        let speakers = Arc::clone(self);
        let inbound = self.inbound();
        tokio::spawn(async move {
            speakers.read(device, recv, inbound).await;
            speakers.peers.lock().await.remove(&device);
        });
        Ok(peer)
    }

    /// Where the record says `device` answers, when it says.
    async fn endpoints(&self, device: [u8; 32]) -> Vec<std::net::SocketAddr> {
        let record = devices::of(&self.copies, &self.person).await;
        record
            .certified_at_with_endpoints(&self.person, tilo_clock::unix_now_ns())
            .into_iter()
            .find(|(cert, _)| cert.device_public_key == device)
            .map(|(_, addrs)| addrs)
            .unwrap_or_default()
    }

    /// Whether `device` is one of the person's certified devices now.
    async fn certified(&self, device: [u8; 32]) -> bool {
        devices::of(&self.copies, &self.person)
            .await
            .certified_at(&self.person, tilo_clock::unix_now_ns())
            .iter()
            .any(|cert| cert.device_public_key == device)
    }

    /// The link this device's shell opened, when one stands: where an arriving envelope goes.
    fn inbound(&self) -> Option<Arc<tokio::sync::Mutex<LinkWriter>>> {
        self.link.lock_recover().clone()
    }

    /// Answer `connection`, one of the person's other devices dialling this one: admit the key it
    /// proves when the record lists it, then take its stream and read what it sends until it ends.
    async fn answer(self: Arc<Self>, connection: Connection) {
        let remote = connection.remote_id();
        let device = *remote.as_bytes();
        if !self.certified(device).await {
            tracing::info!(device = %remote, "a speaker link from a device that is not the person's was refused");
            connection.close(VarInt::from_u32(1), b"not one of the person's devices");
            return;
        }
        let Ok((send, recv)) = connection.accept_bi().await else {
            return;
        };
        self.peers.lock().await.insert(
            device,
            Arc::new(Peer {
                send: tokio::sync::Mutex::new(send),
            }),
        );
        let inbound = self.inbound();
        self.read(device, recv, inbound).await;
        self.peers.lock().await.remove(&device);
    }

    /// Read `device`'s envelopes from `recv` until the link ends, handing each to this device's
    /// shell.
    async fn read(
        &self,
        device: [u8; 32],
        mut recv: RecvStream,
        inbound: Option<Arc<tokio::sync::Mutex<LinkWriter>>>,
    ) {
        loop {
            let frame = match speakers::read(&mut recv).await {
                Ok(frame) => frame,
                Err(error) => {
                    tracing::debug!(%error, "a speaker link with another device ended");
                    return;
                }
            };
            let Frame::Send { envelope, .. } = frame else {
                continue;
            };
            let received_us = tilo_network_actor::fabric::clock::fabric_monotonic_us();
            let Some(envelope) = checked_from(self.own, device, &envelope) else {
                tracing::debug!(
                    device = %hex::encode(device),
                    "an envelope another device sent was refused"
                );
                continue;
            };
            let Ok(bytes) = postcard::to_allocvec(&envelope) else {
                continue;
            };
            let Some(inbound) = inbound.as_ref() else {
                tracing::debug!("no shell of this device reads its speakers now");
                continue;
            };
            let mut link = inbound.lock().await;
            if speakers::write(
                &mut *link,
                &Frame::Heard {
                    peer: device,
                    envelope: bytes,
                    received_us,
                },
            )
            .await
            .is_err()
            {
                tracing::debug!("the shell's speakers link is gone");
                return;
            }
        }
    }
}

/// `envelope`, read as the action `source` sent this device `own`: `None` when it is not.
fn checked_from(
    own: [u8; 32],
    source: [u8; 32],
    envelope: &[u8],
) -> Option<PersonalFabricActionEnvelope> {
    let own = DeviceId::new(hex::encode(own)).ok()?;
    let source = DeviceId::new(hex::encode(source)).ok()?;
    let envelope: PersonalFabricActionEnvelope = postcard::from_bytes(envelope).ok()?;
    envelope
        .validate_at(&source, &own, tilo_clock::unix_now_ms())
        .ok()?;
    Some(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The protocol this serves is the one both a session and another device speak.
    #[test]
    fn the_link_is_the_speakers_protocol() {
        assert_eq!(speakers::ALPN, b"tilo-speakers/1");
    }
}
