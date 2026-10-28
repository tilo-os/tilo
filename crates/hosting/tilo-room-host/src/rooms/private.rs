//! Authenticated, bounded private governor door. No payload enters relay gossip,
//! room publication, action audit, tracing, or error messages.
//!
//! One door, two carriers: a native device's iroh stream on the private ALPN
//! and a page's HTTPS session (`tilo_relay_protocol::rooms::private::PRIVATE_PATH`).
//! Each hands [`PrivateProtocol::serve`] the device certificate its transport
//! proved and the device key that proof bound; the checks and the governor
//! code are the same.
use super::Rooms;
use crate::auth::{self, HandshakeTrust};
use iroh::protocol::{AcceptError, ProtocolHandler};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_relay_protocol::rooms::private::{
    MAX_REPLY_BYTES, MAX_REQUEST_BYTES, PrivateOperation, PrivateReply,
};
use tilo_relay_session::rooms::private::PrivateRequest;
use tilo_user_identity::DeviceCert;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 64;
const MAX_PER_PEER: usize = 4;

#[derive(Clone)]
/// Authenticated governor protocol serving owner-private commands.
pub struct PrivateProtocol {
    rooms: Rooms,
}

/// How many exchanges the door holds at once, in all and per device: one
/// set for the rooms, whichever carrier an exchange came by.
pub(super) struct PrivateBounds {
    connections: Arc<Semaphore>,
    peers: Mutex<HashMap<[u8; 32], Arc<Semaphore>>>,
}

impl Default for PrivateBounds {
    fn default() -> Self {
        Self {
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            peers: Mutex::default(),
        }
    }
}
impl std::fmt::Debug for PrivateProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateProtocol")
    }
}

/// The capacity one exchange holds: of the door, and of its device.
pub struct Admitted {
    _door: OwnedSemaphorePermit,
    _device: OwnedSemaphorePermit,
}

impl PrivateProtocol {
    #[must_use]
    /// Serve private operations using the same governed room custody.
    pub const fn new(rooms: Rooms) -> Self {
        Self { rooms }
    }

    /// Room for one more exchange of `device`, within the door's and the
    /// device's bounds; `None` when either is full.
    pub fn admit(&self, device: [u8; 32]) -> Option<Admitted> {
        let bounds = &self.rooms.inner.private;
        let door = bounds.connections.clone().try_acquire_owned().ok()?;
        let peer = {
            let mut peers = bounds
                .peers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            peers.retain(|_, semaphore| Arc::strong_count(semaphore) > 1);
            peers
                .entry(device)
                .or_insert_with(|| Arc::new(Semaphore::new(MAX_PER_PEER)))
                .clone()
        };
        let device = peer.try_acquire_owned().ok()?;
        Some(Admitted {
            _door: door,
            _device: device,
        })
    }

    fn authenticate(&self, cert: &DeviceCert, device: &[u8; 32]) -> Result<(), String> {
        let host = &self.rooms.inner.host;
        let revocations = host
            .revocations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        auth::authenticate_endpoint_bound(
            cert,
            device,
            &host.cert.user_public_key,
            HandshakeTrust::AnyCertifiedDevice,
            &revocations,
            tilo_clock::unix_now_ns(),
        )
        .map(|_| ())
        .map_err(|_| "private caller authentication refused".into())
    }

    /// Answer `operation` in `room` for the device `device` whose
    /// certificate `cert` its transport proved. A watch waits for the
    /// conversation to change; `until` ends the wait early: with an error
    /// when the caller went away (nothing is answered), with `Ok` when a
    /// poll's time is up (the current, freshly authorized answer is given,
    /// the unchanged cursor if nothing changed). Anything refused answers
    /// [`PrivateReply::Refused`], never why.
    pub async fn serve(
        &self,
        cert: &DeviceCert,
        device: &[u8; 32],
        room: &str,
        operation: PrivateOperation,
        until: impl Future<Output = Result<(), String>>,
    ) -> PrivateReply {
        if self.authenticate(cert, device).is_err() {
            return PrivateReply::Refused;
        }
        self.watched(cert, device, room, operation, until)
            .await
            .unwrap_or(PrivateReply::Refused)
    }

    /// [`Self::execute`] for people only. The room's spectator person, the
    /// one every page that only watches is certified as, is a member of
    /// the room but nobody to talk with: it holds no conversations and is
    /// nobody's contact.
    async fn watched(
        &self,
        cert: &DeviceCert,
        device: &[u8; 32],
        room: &str,
        operation: PrivateOperation,
        until: impl Future<Output = Result<(), String>>,
    ) -> Result<PrivateReply, String> {
        let workspace_id = self
            .rooms
            .room(room)
            .await
            .map_err(|_| "private room unavailable")?
            .lock()
            .await
            .workspace_id();
        let Some(spectator) = self.rooms.spectator_person(workspace_id).await else {
            return self.execute(cert, device, room, operation, until).await;
        };
        let spectator_key = hex::encode(spectator);
        let peer = match &operation {
            PrivateOperation::Contacts => None,
            PrivateOperation::Open { peer }
            | PrivateOperation::Read { peer, .. }
            | PrivateOperation::Send { peer, .. }
            | PrivateOperation::Watch { peer, .. } => Some(peer),
        };
        if cert.user_public_key == spectator || peer == Some(&spectator_key) {
            return Err("the room's spectator holds no private conversations".into());
        }
        Ok(
            match self.execute(cert, device, room, operation, until).await? {
                PrivateReply::Contacts(contacts) => PrivateReply::Contacts(
                    contacts
                        .into_iter()
                        .filter(|contact| contact.public_key != spectator_key)
                        .collect(),
                ),
                reply => reply,
            },
        )
    }

    async fn execute(
        &self,
        cert: &DeviceCert,
        device: &[u8; 32],
        room: &str,
        operation: PrivateOperation,
        until: impl Future<Output = Result<(), String>>,
    ) -> Result<PrivateReply, String> {
        let governed = self
            .rooms
            .room(room)
            .await
            .map_err(|_| "private room unavailable")?;
        // Subscribe before inspecting authority or the cursor, closing both wake races.
        let mut authority = crate::authority_epoch::subscribe();
        let mut governance = self.rooms.inner.changes.subscribe();
        if let PrivateOperation::Watch {
            peer,
            after_sequence,
        } = &operation
        {
            let mut changes = {
                let room = governed.lock().await;
                self.authenticate(cert, device)?;
                room.private_watch(cert, peer)?
            };
            let mut until = std::pin::pin!(until);
            let mut ended = false;
            loop {
                // Expiry of either pair participant must wake the stream even if
                // nobody publishes or sends. No idle polling and no lock while waiting.
                let expires = {
                    let room = governed.lock().await;
                    // Capture time before authorization: a deadline crossed during
                    // the check must schedule an immediate wake, not be discarded.
                    let checked_at = tilo_clock::unix_now_ns();
                    self.authenticate(cert, device)?;
                    let reply = room.private_command(cert, operation.clone())?;
                    if ended
                        || !matches!(reply, PrivateReply::Changed(sequence) if sequence == *after_sequence)
                    {
                        return Ok(reply);
                    }
                    room.peer
                        .certificates()
                        .iter()
                        .filter_map(|cert| cert.expires_at_ns)
                        .filter(|deadline| *deadline > checked_at)
                        .min()
                };
                let expiry = async {
                    match expires {
                        Some(ns) => {
                            tokio::time::sleep(Duration::from_nanos(
                                ns.saturating_sub(tilo_clock::unix_now_ns()),
                            ))
                            .await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    result = &mut until => { result?; ended = true; },
                    result = changes.changed() => { result.map_err(|_| "private stream closed")?; },
                    _ = authority.changed() => {},
                    _ = governance.recv() => {},
                    () = expiry => {},
                }
            }
        }
        let room = governed.lock().await;
        self.authenticate(cert, device)?;
        room.private_command(cert, operation)
    }

    async fn exchange(&self, connection: &iroh::endpoint::Connection) -> Result<(), ()> {
        let (mut send, mut recv) = tokio::time::timeout(HEADER_TIMEOUT, connection.accept_bi())
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
        let bytes = tokio::time::timeout(
            HEADER_TIMEOUT,
            crate::frame::read_frame(&mut recv, MAX_REQUEST_BYTES),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        let request: PrivateRequest = postcard::from_bytes(&bytes).map_err(|_| ())?;
        let remote = *connection.remote_id().as_bytes();
        let disconnected = async {
            connection.closed().await;
            Err("private stream disconnected".to_owned())
        };
        let reply = self
            .serve(
                &request.device_cert,
                &remote,
                &request.room,
                request.operation,
                disconnected,
            )
            .await;
        let bytes = postcard::to_stdvec(&reply).map_err(|_| ())?;
        if bytes.len() > MAX_REPLY_BYTES {
            return Err(());
        }
        tokio::time::timeout(HEADER_TIMEOUT, crate::frame::write_frame(&mut send, &bytes))
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
        send.finish().map_err(|_| ())?;
        let _ = tokio::time::timeout(HEADER_TIMEOUT, connection.closed()).await;
        Ok(())
    }
}
impl ProtocolHandler for PrivateProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let Some(_admitted) = self.admit(*connection.remote_id().as_bytes()) else {
            connection.close(1u32.into(), b"capacity");
            return Ok(());
        };
        if self.exchange(&connection).await.is_err() {
            connection.close(1u32.into(), b"private exchange failed");
        }
        Ok(())
    }
}
