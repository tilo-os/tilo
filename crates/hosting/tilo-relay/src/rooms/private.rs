//! Authenticated, bounded private governor door. No payload enters relay gossip,
//! room publication, action audit, tracing, or error messages.
use super::Rooms;
use crate::auth::{self, HandshakeTrust};
use iroh::protocol::{AcceptError, ProtocolHandler};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_relay_session::rooms::private::{
    MAX_REPLY_BYTES, MAX_REQUEST_BYTES, PrivateOperation, PrivateReply, PrivateRequest,
};
use tokio::sync::Semaphore;

const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 64;
const MAX_PER_PEER: usize = 4;

#[derive(Clone)]
/// Authenticated governor protocol serving owner-private commands.
pub struct PrivateProtocol {
    rooms: Rooms,
    connections: Arc<Semaphore>,
    peers: Arc<Mutex<HashMap<iroh::EndpointId, Arc<Semaphore>>>>,
}
impl std::fmt::Debug for PrivateProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateProtocol")
    }
}
impl PrivateProtocol {
    #[must_use]
    /// Serve private operations using the same governed room custody.
    pub fn new(rooms: Rooms) -> Self {
        Self {
            rooms,
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    fn authenticate(&self, request: &PrivateRequest, remote: &[u8; 32]) -> Result<(), String> {
        let host = &self.rooms.inner.host;
        let revocations = host
            .revocations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        auth::authenticate_endpoint_bound(
            &request.device_cert,
            remote,
            &host.cert.user_public_key,
            HandshakeTrust::AnyCertifiedDevice,
            &revocations,
            tilo_clock::unix_now_ns(),
        )
        .map(|_| ())
        .map_err(|_| "private caller authentication refused".into())
    }
    async fn execute(
        &self,
        request: &PrivateRequest,
        remote: &[u8; 32],
        connection: &iroh::endpoint::Connection,
    ) -> Result<PrivateReply, String> {
        let governed = self
            .rooms
            .room(&request.room)
            .await
            .map_err(|_| "private room unavailable")?;
        if let PrivateOperation::NativeTaskRead { operation, .. } = &request.operation
            && let PrivateOperation::TaskMailboxWatch { after_sequence, .. } = operation.as_ref()
        {
            return self
                .execute_task_watch(request, remote, connection, &governed, *after_sequence)
                .await;
        }
        // Subscribe before inspecting authority or the cursor, closing both wake races.
        let mut authority = crate::authority_epoch::subscribe();
        let mut governance = self.rooms.inner.changes.subscribe();
        if let PrivateOperation::Watch {
            peer,
            after_sequence,
        } = &request.operation
        {
            let mut changes = {
                let room = governed.lock().await;
                self.authenticate(request, remote)?;
                room.private_watch(&request.device_cert, peer)?
            };
            loop {
                // Expiry of either pair participant must wake the stream even if
                // nobody publishes or sends. No idle polling and no lock while waiting.
                let expires = {
                    let room = governed.lock().await;
                    // Capture time before authorization: a deadline crossed during
                    // the check must schedule an immediate wake, not be discarded.
                    let checked_at = tilo_clock::unix_now_ns();
                    self.authenticate(request, remote)?;
                    let reply =
                        room.private_command(&request.device_cert, request.operation.clone())?;
                    if !matches!(reply, PrivateReply::Changed(sequence) if sequence == *after_sequence)
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
                    _ = connection.closed() => return Err("private stream disconnected".into()),
                    result = changes.changed() => { result.map_err(|_| "private stream closed")?; },
                    _ = authority.changed() => {},
                    _ = governance.recv() => {},
                    () = expiry => {},
                }
            }
        }
        let mut room = governed.lock().await;
        self.authenticate(request, remote)?;
        match &request.operation {
            PrivateOperation::NativeTask { payload } => self
                .rooms
                .native_task_private(&mut room, &request.device_cert, payload)
                .await
                .map(|payload| PrivateReply::NativeTask { payload }),
            PrivateOperation::NativeTaskRead { .. }
            | PrivateOperation::TaskInstructions { .. }
            | PrivateOperation::TaskConfirmation { .. }
            | PrivateOperation::TaskConfirmationInvocation { .. }
            | PrivateOperation::TaskRunDigest { .. }
            | PrivateOperation::TaskModelContract { .. }
            | PrivateOperation::TaskMessages { .. }
            | PrivateOperation::TaskMailboxWatch { .. }
            | PrivateOperation::TaskAcknowledge { .. }
            | PrivateOperation::TaskSend { .. }
            | PrivateOperation::TaskOutputs { .. }
            | PrivateOperation::TaskReportTranscript { .. }
            | PrivateOperation::TaskRecordOutputs { .. } => {
                room.private_task_command(&request.device_cert, request.operation.clone())
            }
            _ => room.private_command(&request.device_cert, request.operation.clone()),
        }
    }
    async fn execute_task_watch(
        &self,
        request: &PrivateRequest,
        remote: &[u8; 32],
        connection: &iroh::endpoint::Connection,
        governed: &Arc<tokio::sync::Mutex<super::GovernedRoom>>,
        after_sequence: u64,
    ) -> Result<PrivateReply, String> {
        // Subscribe before inspecting authority or the cursor, closing both wake races.
        let mut authority = crate::authority_epoch::subscribe();
        let mut governance = self.rooms.inner.changes.subscribe();
        let mut changes = {
            let room = governed.lock().await;
            self.authenticate(request, remote)?;
            room.private_task_watch(&request.device_cert, &request.operation)?
        };
        loop {
            let expires = {
                let room = governed.lock().await;
                let checked_at = tilo_clock::unix_now_ns();
                self.authenticate(request, remote)?;
                let reply =
                    room.private_task_command(&request.device_cert, request.operation.clone())?;
                if !matches!(reply, PrivateReply::Changed(sequence) if sequence == after_sequence) {
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
                _ = connection.closed() => return Err("private task stream disconnected".into()),
                result = changes.changed() => { result.map_err(|_| "private task stream closed")?; },
                _ = authority.changed() => {},
                _ = governance.recv() => {},
                () = expiry => {},
            }
        }
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
        let reply = match self.authenticate(&request, &remote) {
            Ok(()) => self
                .execute(&request, &remote, connection)
                .await
                .unwrap_or(PrivateReply::Refused),
            Err(_) => PrivateReply::Refused,
        };
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
        let Ok(_permit) = self.connections.clone().try_acquire_owned() else {
            connection.close(1u32.into(), b"capacity");
            return Ok(());
        };
        let peer = {
            let mut peers = self
                .peers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            peers.retain(|_, semaphore| Arc::strong_count(semaphore) > 1);
            peers
                .entry(connection.remote_id())
                .or_insert_with(|| Arc::new(Semaphore::new(MAX_PER_PEER)))
                .clone()
        };
        let Ok(_peer) = peer.clone().try_acquire_owned() else {
            connection.close(1u32.into(), b"capacity");
            return Ok(());
        };
        if self.exchange(&connection).await.is_err() {
            connection.close(1u32.into(), b"private exchange failed");
        }
        Ok(())
    }
}
