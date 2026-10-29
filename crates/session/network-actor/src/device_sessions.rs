//! The session between two of the person's devices.
//!
//! Two of the person's devices keep one connection between them
//! ([`SIBLING_ALPN`]), whichever way it was dialled: the device with the
//! lower endpoint id dials it, the other only when it is not dialled within
//! [`UNDIALLED`] (the first may not know of it yet). Of two dialled at once,
//! both devices keep the one the lower endpoint id dialled. An idle
//! connection keeps both devices waking for its keep-alives, so a second one
//! would cost as much again.
//!
//! On the connection's first bidirectional stream each side first says who
//! it is ([`Hello`]): its certificates (its person's, and those of the
//! workspace identities it acts for) and the revocations that come with
//! them. The endpoint id proves the device key, so a certificate counts only
//! for the device at the other end, and only when it is signed by a root
//! this device acts for too. From then on the stream carries what is for the
//! person's own devices alone: codec adverts, the person's workspace list,
//! and a renewed hello when a certificate or a revocation changes. Every
//! further bidirectional stream carries one personal fabric frame
//! ([`fabric_link`]); nearby transfers stream on the same connection.
//!
//! The other device is a sibling while the session lasts: what it sends,
//! and what is sent to it, is bound to the membership epoch the session
//! started ([`NetEvent::Sibling`]), keyed by its root and device, never by a
//! workspace.
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_relay_protocol::transport::SIBLING_ALPN;
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};
use tokio::sync::mpsc;

use super::{
    ActorState, DEFAULT_WS_TOPIC, InternalEvent, MembershipEpoch, NetEvent, fabric_link,
    message_fingerprint,
};

/// How long dialling one of the person's devices may take.
const DEVICE_DIAL: Duration = Duration::from_secs(10);
/// How long the other device may take to say who it is.
const HELLO_WAIT: Duration = Duration::from_secs(10);
/// The largest frame of the session: a workspace list with room to spare.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Frames waiting to be written to one device. Beyond it the device is not
/// keeping up; what it missed it gets again with the next change.
const FRAMES: usize = 64;
/// How long the device with the higher endpoint id waits to be dialled
/// before it dials itself.
const UNDIALLED: Duration = Duration::from_secs(5);

/// Who a device is, as it says first on the session, and again whenever
/// its certificates or revocations change.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    /// The sender's [`neighbour_wire::PEER_PROTOCOL_VERSION`]: devices of
    /// different versions keep no session.
    pub protocol: u32,
    /// Its person's certificate first, then one per workspace identity.
    pub certs: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
}

/// One frame of the session's stream.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
enum Frame {
    Hello(Hello),
    Msg(NeighbourMsg),
}

/// What the other device sent on the session's stream.
#[derive(Debug)]
pub(super) enum Heard {
    Hello(Hello),
    Msg(NeighbourMsg),
}

/// A session whose hellos were exchanged, for the actor to take.
pub(super) struct Opened {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    hello: Hello,
}

/// One of the person's devices with its session up.
pub(super) struct Sibling {
    connection: Connection,
    pub(super) epoch: MembershipEpoch,
    /// The certificates it proved, each of a root this device acts for.
    pub(super) certs: Vec<DeviceCert>,
    pub(super) revocations: Vec<RevocationEntry>,
    frames: mpsc::Sender<Vec<u8>>,
    pub(super) fabric: fabric_link::FabricLink,
    /// Fingerprint of the last codec advert taken from it: a repeated
    /// advert changes nothing.
    caps_advert: Option<[u8; 32]>,
    tasks: [tokio::task::JoinHandle<()>; 2],
}

impl Sibling {
    /// Whether it proved a certificate of `root`.
    pub(super) fn proved(&self, root: &[u8; 32]) -> bool {
        self.certs.iter().any(|cert| &cert.user_public_key == root)
    }

    fn send(&self, frame: &Frame) {
        let Ok(bytes) = postcard::to_allocvec(frame) else {
            return;
        };
        if bytes.len() > MAX_FRAME_BYTES {
            tracing::warn!(
                bytes = bytes.len(),
                "a frame for a device of the person is too large"
            );
            return;
        }
        if self.frames.try_send(bytes).is_err() {
            tracing::debug!(peer = %self.connection.remote_id(), "frames to a device of the person are backed up; one is dropped");
        }
    }

    fn event(&self) -> NetEvent {
        NetEvent::Sibling {
            peer: self.connection.remote_id(),
            membership_epoch: self.epoch,
            certs: self.certs.clone(),
            revocations: self.revocations.clone(),
        }
    }
}

impl Drop for Sibling {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        self.connection.close(0_u32.into(), b"session ended");
    }
}

/// The [`SIBLING_ALPN`] handler: a connection one of the person's devices
/// dialled, handed to the actor, which runs its session.
#[derive(Clone, Debug)]
pub(super) struct SiblingProtocol(pub(super) mpsc::Sender<InternalEvent>);

impl ProtocolHandler for SiblingProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let _ = self
            .0
            .send(InternalEvent::SiblingConnected(connection))
            .await;
        Ok(())
    }
}

impl ActorState {
    /// What this device says it is to the person's other devices: its
    /// person's certificate, then one per workspace identity of another
    /// root, with every revocation that comes with them.
    pub(super) fn own_hello(&self) -> Option<Hello> {
        let identity = self.host_identity.as_ref()?;
        let (cert, mut revocations) = identity.material_for(DEFAULT_WS_TOPIC);
        let mut certs = vec![cert];
        let mut workspaces: Vec<_> = identity.workspace_device_certs.keys().copied().collect();
        workspaces.sort_unstable();
        for ws in workspaces {
            let (cert, entries) = identity.material_for(ws);
            if !certs
                .iter()
                .any(|known| known.user_public_key == cert.user_public_key)
            {
                certs.push(cert);
            }
            for entry in entries {
                if !revocations.contains(&entry) {
                    revocations.push(entry);
                }
            }
        }
        Some(Hello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            certs,
            revocations,
        })
    }

    /// The certificates of `hello` that prove the device at `peer` is one
    /// of a root this device acts for (`own`), unrevoked; `None` when none
    /// does, or the device runs another peer protocol version.
    fn proven(peer: EndpointId, hello: &Hello, own: &Hello) -> Option<Vec<DeviceCert>> {
        if hello.protocol != neighbour_wire::PEER_PROTOCOL_VERSION {
            tracing::info!(peer = %peer, protocol = hello.protocol, "a device of the person runs another peer protocol version");
            return None;
        }
        let mut revocations = RevocationList::new();
        for entry in &hello.revocations {
            if revocations.insert(entry.clone()).is_err() {
                tracing::warn!(peer = %peer, "a device of the person sent an invalid revocation");
                return None;
            }
        }
        let now = tilo_clock::unix_now_ns();
        let revoked = |cert: &DeviceCert| {
            hello
                .revocations
                .iter()
                .chain(&own.revocations)
                .any(|entry| {
                    // The person's key revoked this very device.
                    (entry.user_public_key, entry.revoked_device_public_key)
                        == (cert.user_public_key, cert.device_public_key)
                        && entry.revoked_at_ns <= now
                })
        };
        let certs: Vec<DeviceCert> = hello
            .certs
            .iter()
            .filter(|cert| {
                cert.device_public_key == *peer.as_bytes()
                    && own
                        .certs
                        .iter()
                        .any(|mine| mine.user_public_key == cert.user_public_key)
                    && tilo_user_identity::verify_device_cert_at(cert, now).is_ok()
                    && !revoked(cert)
            })
            .cloned()
            .collect();
        (!certs.is_empty()).then_some(certs)
    }

    /// Dial each of the person's devices without a session that this device
    /// dials ([`dials`]), or that did not dial it within [`UNDIALLED`].
    pub(super) fn dial_siblings(&mut self) {
        let own = self.endpoint.id();
        let devices: Vec<EndpointAddr> = self.own_devices.values().cloned().collect();
        for addr in devices {
            let device = addr.id;
            if self.siblings.contains_key(&device) || self.sibling_dialling.contains(&device) {
                continue;
            }
            if !dials(own, device) {
                let since = *self
                    .sibling_waiting
                    .entry(device)
                    .or_insert_with(Instant::now);
                if since.elapsed() < UNDIALLED {
                    continue;
                }
            }
            let Some(hello) = self.own_hello() else {
                return;
            };
            self.sibling_dialling.insert(device);
            let (endpoint, tx) = (self.endpoint.clone(), self.internal_tx.clone());
            tokio::spawn(async move {
                match tokio::time::timeout(DEVICE_DIAL, endpoint.connect(addr, SIBLING_ALPN)).await
                {
                    Ok(Ok(connection)) => open(connection, hello, tx).await,
                    Ok(Err(error)) => {
                        tracing::debug!(peer = %device, %error, "a device of the person did not answer");
                        let _ = tx.send(InternalEvent::SiblingFailed(device)).await;
                    }
                    Err(_) => {
                        tracing::debug!(peer = %device, "a device of the person did not answer in time");
                        let _ = tx.send(InternalEvent::SiblingFailed(device)).await;
                    }
                }
            });
        }
    }

    /// One of the person's devices dialled this one: run the session on
    /// `connection`, once each side said who it is.
    pub(super) fn sibling_connected(&self, connection: Connection) {
        let Some(hello) = self.own_hello() else {
            connection.close(0_u32.into(), b"no identity");
            return;
        };
        tokio::spawn(open(connection, hello, self.internal_tx.clone()));
    }

    /// The session on `opened` exchanged hellos: the other device is a
    /// sibling from now on when it proved itself one of the person's, and
    /// this connection is the one the pair keeps.
    pub(super) async fn sibling_opened(&mut self, opened: Opened) {
        let Opened {
            connection,
            send,
            recv,
            hello,
        } = opened;
        let peer = connection.remote_id();
        let own_id = self.endpoint.id();
        if connection.side() == iroh::endpoint::Side::Client {
            self.sibling_dialling.remove(&peer);
        }
        let Some(own) = self.own_hello() else {
            connection.close(0_u32.into(), b"no identity");
            return;
        };
        let Some(certs) = Self::proven(peer, &hello, &own) else {
            tracing::warn!(peer = %peer, "a device proved no certificate of the person");
            connection.close(0_u32.into(), b"not one of the person's devices");
            return;
        };
        if let Some(current) = self.siblings.get(&peer)
            && current.connection.close_reason().is_none()
            && current.connection.stable_id() != connection.stable_id()
            && current.connection.side() != connection.side()
            && !dialled_by_the_lower(&connection, own_id)
        {
            connection.close(0_u32.into(), b"the lower endpoint id's connection is kept");
            return;
        }
        self.sibling_waiting.remove(&peer);
        if let Some(previous) = self.siblings.remove(&peer) {
            self.sibling_gone(peer, previous).await;
        }
        self.next_membership_epoch = self.next_membership_epoch.saturating_add(1).max(1);
        let epoch = MembershipEpoch(self.next_membership_epoch);
        let (frames, queued) = mpsc::channel(FRAMES);
        let tx = self.internal_tx.clone();
        let tasks = [
            tokio::spawn(write_frames(send, queued)),
            tokio::spawn(read_frames(peer, epoch, recv, tx.clone())),
        ];
        crate::read_personal_fabric(connection.clone(), tx, epoch);
        let sibling = Sibling {
            fabric: fabric_link::FabricLink::new(connection.clone()),
            connection,
            epoch,
            certs,
            revocations: hello.revocations,
            frames,
            caps_advert: None,
            tasks,
        };
        tracing::info!(peer = %peer, "session with a device of the person started");
        let advert = self.media_caps_message();
        if let Ok(bytes) = postcard::to_allocvec(&advert) {
            self.caps_sent = Some(message_fingerprint(&bytes));
        }
        sibling.send(&Frame::Msg(advert));
        let event = sibling.event();
        self.sibling_hello_sent = Some(hello_fingerprint(&own));
        self.siblings.insert(peer, sibling);
        self.tell_siblings(event).await;
        for (ws, target) in self.pending_remote_actions.targets() {
            if target == peer {
                self.flush_pending_remote_actions(ws, peer);
            }
        }
    }

    /// `peer` sent `heard` on the session of membership `epoch`.
    pub(super) async fn sibling_heard(
        &mut self,
        peer: EndpointId,
        epoch: MembershipEpoch,
        heard: Heard,
    ) {
        if self
            .siblings
            .get(&peer)
            .is_none_or(|sibling| sibling.epoch != epoch)
        {
            return;
        }
        match heard {
            Heard::Hello(hello) => {
                let proven = self
                    .own_hello()
                    .and_then(|own| Self::proven(peer, &hello, &own));
                let Some(certs) = proven else {
                    tracing::warn!(peer = %peer, "a device of the person no longer proves itself one");
                    self.sibling_closed(peer, epoch).await;
                    return;
                };
                let Some(sibling) = self.siblings.get_mut(&peer) else {
                    return;
                };
                sibling.certs = certs;
                sibling.revocations = hello.revocations;
                let event = sibling.event();
                self.tell_siblings(event).await;
            }
            Heard::Msg(msg) => {
                let news = match &msg {
                    NeighbourMsg::MediaCapabilities { .. } => {
                        let Ok(bytes) = postcard::to_allocvec(&msg) else {
                            return;
                        };
                        let fingerprint = message_fingerprint(&bytes);
                        self.siblings.get_mut(&peer).is_some_and(|sibling| {
                            sibling.caps_advert.replace(fingerprint) != Some(fingerprint)
                        })
                    }
                    NeighbourMsg::PersonalWorkspaces { .. } => true,
                    _ => {
                        tracing::warn!(peer = %peer, kind = msg.kind(), "a device of the person sent what its session does not carry");
                        false
                    }
                };
                self.tally.note(msg.kind());
                if news {
                    self.tell_siblings(NetEvent::SiblingMsg {
                        peer,
                        membership_epoch: epoch,
                        msg,
                    })
                    .await;
                }
            }
        }
    }

    /// The session of membership `epoch` with `peer` ended.
    pub(super) async fn sibling_closed(&mut self, peer: EndpointId, epoch: MembershipEpoch) {
        if self
            .siblings
            .get(&peer)
            .is_none_or(|sibling| sibling.epoch != epoch)
        {
            return;
        }
        if let Some(sibling) = self.siblings.remove(&peer) {
            tracing::info!(peer = %peer, "session with a device of the person ended; the next heartbeat dials it again");
            self.sibling_gone(peer, sibling).await;
        }
    }

    /// `sibling`, once `peer`'s, is a sibling no longer: what streamed to it
    /// stops, and the shell hears its membership ended.
    async fn sibling_gone(&mut self, peer: EndpointId, sibling: Sibling) {
        let epoch = sibling.epoch;
        drop(sibling);
        self.cancel_personal_transfer_route(peer);
        self.tell_siblings(NetEvent::SiblingLeft {
            peer,
            membership_epoch: epoch,
        })
        .await;
    }

    /// Send `msg` to every sibling.
    pub(super) fn publish_to_siblings(&self, msg: NeighbourMsg) {
        let frame = Frame::Msg(msg);
        for sibling in self.siblings.values() {
            sibling.send(&frame);
        }
    }

    /// Say who this device is again to every sibling, when it changed since
    /// it was last said: a renewed certificate or a new revocation reaches
    /// them as soon as it is on disk.
    pub(super) fn hello_siblings_if_changed(&mut self) {
        let Some(hello) = self.own_hello() else {
            return;
        };
        let fingerprint = hello_fingerprint(&hello);
        if self.sibling_hello_sent.replace(fingerprint) == Some(fingerprint) {
            return;
        }
        let frame = Frame::Hello(hello);
        for sibling in self.siblings.values() {
            sibling.send(&frame);
        }
    }

    /// Hear the person's devices from now on at `on_event`, starting with
    /// those whose sessions are up.
    pub(super) async fn watch_siblings(&mut self, on_event: mpsc::Sender<NetEvent>) {
        for sibling in self.siblings.values() {
            let _ = on_event.send(sibling.event()).await;
        }
        self.sibling_events = Some(on_event);
    }

    async fn tell_siblings(&self, event: NetEvent) {
        if let Some(tx) = &self.sibling_events {
            let _ = tx.send(event).await;
        }
    }
}

/// Exchange hellos on `connection`: this device's `own` first, then the
/// other's, and hand the session to the actor; close it when the other
/// device says nothing, or something else first.
async fn open(connection: Connection, own: Hello, tx: mpsc::Sender<InternalEvent>) {
    let peer = connection.remote_id();
    let dialled = connection.side() == iroh::endpoint::Side::Client;
    let exchanged = tokio::time::timeout(HELLO_WAIT, async {
        let (mut send, mut recv) = if dialled {
            connection.open_bi().await
        } else {
            connection.accept_bi().await
        }
        .map_err(|error| error.to_string())?;
        let bytes = postcard::to_allocvec(&Frame::Hello(own)).map_err(|error| error.to_string())?;
        write_frame(&mut send, &bytes).await?;
        match read_frame(&mut recv).await? {
            Frame::Hello(hello) => Ok((send, recv, hello)),
            Frame::Msg(_) => Err("the device said something before who it is".to_string()),
        }
    })
    .await
    .map_err(|_| "the device did not say who it is in time".to_string())
    .and_then(|result| result);
    match exchanged {
        Ok((send, recv, hello)) => {
            let _ = tx
                .send(InternalEvent::SiblingOpened(Box::new(Opened {
                    connection,
                    send,
                    recv,
                    hello,
                })))
                .await;
        }
        Err(error) => {
            tracing::debug!(peer = %peer, %error, "no session with a device");
            connection.close(0_u32.into(), b"no session");
            if dialled {
                let _ = tx.send(InternalEvent::SiblingFailed(peer)).await;
            }
        }
    }
}

/// Write each queued frame to `send`, in order, until the session ends.
async fn write_frames(mut send: SendStream, mut queued: mpsc::Receiver<Vec<u8>>) {
    while let Some(bytes) = queued.recv().await {
        if write_frame(&mut send, &bytes).await.is_err() {
            return;
        }
    }
}

/// Read the frames `peer` sends on the session of membership `epoch` into
/// the actor, until the stream ends.
async fn read_frames(
    peer: EndpointId,
    epoch: MembershipEpoch,
    mut recv: RecvStream,
    tx: mpsc::Sender<InternalEvent>,
) {
    loop {
        let heard = match read_frame(&mut recv).await {
            Ok(Frame::Hello(hello)) => Heard::Hello(hello),
            Ok(Frame::Msg(msg)) => Heard::Msg(msg),
            Err(error) => {
                tracing::debug!(peer = %peer, %error, "session with a device of the person closed");
                let _ = tx.send(InternalEvent::SiblingClosed { peer, epoch }).await;
                return;
            }
        };
        if tx
            .send(InternalEvent::SiblingHeard { peer, epoch, heard })
            .await
            .is_err()
        {
            return;
        }
    }
}

async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<(), String> {
    let len = u32::try_from(bytes.len()).map_err(|_| "frame too large".to_string())?;
    send.write_all(&len.to_le_bytes())
        .await
        .map_err(|error| error.to_string())?;
    send.write_all(bytes)
        .await
        .map_err(|error| error.to_string())
}

async fn read_frame(recv: &mut RecvStream) -> Result<Frame, String> {
    let mut len = [0_u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|error| error.to_string())?;
    let len =
        usize::try_from(u32::from_le_bytes(len)).map_err(|_| "frame too large".to_string())?;
    if len > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    let mut bytes = vec![0_u8; len];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    postcard::from_bytes(&bytes).map_err(|error| error.to_string())
}

fn hello_fingerprint(hello: &Hello) -> [u8; 32] {
    postcard::to_allocvec(hello).map_or([0; 32], |bytes| message_fingerprint(&bytes))
}

/// Whether this device, `own`, dials the connection to the person's device
/// `other` at once: the one with the lower endpoint id does, so the two do
/// not bring up a connection each.
fn dials(own: EndpointId, other: EndpointId) -> bool {
    own < other
}

/// Whether `connection` is the one the device with the lower endpoint id
/// dialled, of the two a pair may bring up at once. `own` is this device.
fn dialled_by_the_lower(connection: &Connection, own: EndpointId) -> bool {
    let dialled_here = connection.side() == iroh::endpoint::Side::Client;
    dialled_here == dials(own, connection.remote_id())
}
