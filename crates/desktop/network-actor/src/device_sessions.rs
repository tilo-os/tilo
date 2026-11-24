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
//! what each says of a workspace's key group (its frames, access requests
//! and asks for frames it lacks), and a renewed hello when a certificate or a revocation changes. Every
//! further bidirectional stream carries one personal fabric frame
//! ([`fabric_link`]).
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
use tilo_server_protocol::transport::SIBLING_ALPN;
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
const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(MAX_FRAME_BYTES);
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
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Frame {
    Hello(Hello),
    Msg(NeighbourMsg),
}

impl Frame {
    /// Postcard variant index of [`Frame::Msg`].
    const MSG_TAG: u8 = 1;

    /// The frame `bytes` holds, its message read as [`NeighbourMsg::decode`] reads one, so a
    /// device one peer version behind is heard too.
    fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        match bytes.strip_prefix(&[Self::MSG_TAG]) {
            Some(message) => NeighbourMsg::decode(message).map(Self::Msg),
            None => postcard::from_bytes(bytes),
        }
    }
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
        if bytes.len() > FRAMING.max_len() {
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
        let (cert, revocations) = identity.material_for(DEFAULT_WS_TOPIC);
        Some(Hello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            certs: vec![cert],
            revocations,
        })
    }

    /// The certificates of `hello` that prove the device at `peer` is one
    /// of a root this device acts for (`own`), unrevoked; `None` when none
    /// does, or the device runs another peer protocol version.
    fn proven(peer: EndpointId, hello: &Hello, own: &Hello) -> Option<Vec<DeviceCert>> {
        if !neighbour_wire::works_with_peer_version(hello.protocol) {
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
            tasks,
        };
        tracing::info!(peer = %peer, "session with a device of the person started");
        let event = sibling.event();
        self.sibling_hello_sent = Some(hello_fingerprint(&own));
        self.siblings.insert(peer, sibling);
        self.tell_siblings(event).await;
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
                    // A workspace's key group as one of the person's devices says it: its
                    // frames, an access request, and an ask for frames it lacks. The key
                    // service judges those as it judges a host's.
                    NeighbourMsg::MlsGovernance { .. }
                    | NeighbourMsg::MlsAccessRequest { .. }
                    | NeighbourMsg::MlsGovernanceWanted { .. } => true,
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

async fn write_frame(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    bytes: &[u8],
) -> Result<(), String> {
    FRAMING
        .write_async(send, bytes)
        .await
        .map_err(|error| error.to_string())
}

async fn read_frame(recv: &mut (impl tokio::io::AsyncRead + Unpin)) -> Result<Frame, String> {
    let bytes = FRAMING
        .read_async(recv)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| wire_frame::FrameError::Closed.to_string())?;
    Frame::decode(&bytes).map_err(|error| error.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use tilo_user_identity::{
        DeviceCapabilities, RevocationReason, revocation_signing_digest, sign_device_cert,
    };

    #[derive(Clone, Debug)]
    struct AcceptOne(
        std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Connection>>>>,
    );

    impl ProtocolHandler for AcceptOne {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            let tx = self.0.lock().unwrap().take();
            if let Some(tx) = tx {
                let _ = tx.send(connection);
            }
            Ok(())
        }
    }

    async fn connected_pair() -> (
        iroh::Endpoint,
        iroh::Endpoint,
        iroh::protocol::Router,
        Connection,
        Connection,
    ) {
        let (a, _) = crate::loopback_endpoint(None).await;
        let (b, _) = crate::loopback_endpoint(None).await;
        let alpn: &[u8] = b"tilo-test-device-sessions-frame";
        let (accept_tx, accept_rx) = tokio::sync::oneshot::channel();
        let router = iroh::protocol::Router::builder(b.clone())
            .accept(
                alpn,
                AcceptOne(std::sync::Arc::new(std::sync::Mutex::new(Some(accept_tx)))),
            )
            .spawn();
        let client_conn = a.connect(b.addr(), alpn).await.unwrap();
        let server_conn = accept_rx.await.unwrap();
        (a, b, router, client_conn, server_conn)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wire_bytes_match_frozen_fixtures() {
        // Literal bytes captured from main before migrating device_sessions.rs to wire-frame.
        const HELLO_HEX: &str = concat!(
            "1d010000003f018a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3",
            "748801b40f6f5c8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25d",
            "f60f5b8fc9b3940b776f726b73746174696f6e010100006401c801401142b6a8",
            "d77956a62c28f72a92b225c604043f459eec5336736fe4790e17e2bfb903194c",
            "8ae79c04fa46af83e7f71c3a90b0ae88fe03fe13f6910dc2ba40f907018a88e3",
            "dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5ced4928",
            "c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1960100",
            "4015e925ade27c013ecd1da027d828811e10d4c1187676d3af813cd8934e9484",
            "6c1a1c3a68d50e4757848b0d655e60b07abf2957c892f95087ea89adab1b69e3",
            "0b",
        );
        const WANTED_MSG_HEX: &str = "28000000011a2430303030303030302d303030302d343030302d383030302d30303030303030303030303107";
        const MEDIA_CAPABILITIES_HEX: &str =
            "1a0000000110cdd702010361763101046832363401036176310103767039";

        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let revoked_device = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "workstation".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 100,
            expires_at_ns: Some(200),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let mut revocation = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: revoked_device.verifying_key().to_bytes(),
            revoked_at_ns: 150,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        revocation.signature = root
            .sign(&revocation_signing_digest(&revocation).unwrap())
            .to_bytes();

        let cases = [
            (
                Frame::Hello(Hello {
                    protocol: 63,
                    certs: vec![cert],
                    revocations: vec![revocation],
                }),
                HELLO_HEX,
            ),
            (
                Frame::Msg(NeighbourMsg::MlsGovernanceWanted {
                    workspace: "00000000-0000-4000-8000-000000000001".into(),
                    from_epoch: 7,
                }),
                WANTED_MSG_HEX,
            ),
            (
                Frame::Msg(NeighbourMsg::MediaCapabilities {
                    peer: 0xABCD,
                    hardware_decode: vec!["av1".into()],
                    software_decode: vec!["h264".into()],
                    hardware_encode: vec!["av1".into()],
                    software_encode: vec!["vp9".into()],
                }),
                MEDIA_CAPABILITIES_HEX,
            ),
        ];

        let (a, _b, router, client_conn, server_conn) = connected_pair().await;
        for (frame, expected_hex) in cases {
            let expected = hex::decode(expected_hex).unwrap();
            let body = postcard::to_allocvec(&frame).unwrap();

            // Write over a real QUIC SendStream and compare the literal wire bytes.
            let reader_conn = server_conn.clone();
            let reader = tokio::spawn(async move {
                let (_, mut recv) = reader_conn.accept_bi().await.unwrap();
                recv.read_to_end(usize::MAX).await.unwrap()
            });
            let (mut send, _) = client_conn.open_bi().await.unwrap();
            write_frame(&mut send, &body).await.unwrap();
            send.finish().unwrap();
            assert_eq!(reader.await.unwrap(), expected);

            // Decode directly from the frozen bytes via read_frame (raw read + Frame::decode).
            let mut slice = expected.as_slice();
            let decoded = read_frame(&mut slice).await.unwrap();
            assert_eq!(decoded, frame);
            assert!(slice.is_empty());
        }
        a.close().await;
        let _ = router.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn framing_enforces_max_size_on_every_send_including_hello_and_uses_frame_decode() {
        // Direct write_frame refuses payloads over MAX_FRAME_BYTES without writing anything.
        let mut wire = Vec::new();
        let oversized = vec![0_u8; MAX_FRAME_BYTES + 1];
        assert!(write_frame(&mut wire, &oversized).await.is_err());
        assert!(wire.is_empty());

        // `open()` sends its Hello through `write_frame`, so an oversized Hello is
        // refused before anything hits the QUIC stream.
        let (a, _b, router, client_conn, server_conn) = connected_pair().await;
        let oversized_hello = Hello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            certs: vec![DeviceCert {
                user_public_key: [1; 32],
                device_public_key: [2; 32],
                device_label: "x".repeat(MAX_FRAME_BYTES),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            }],
            revocations: Vec::new(),
        };
        let (tx, mut rx) = mpsc::channel(4);
        let peer_id = client_conn.remote_id();
        // The server reads whatever the client sends while `open` runs, and counts it. The
        // stream the client opened and ended empty may reach it ahead of the close, so that the
        // stream arrives says nothing: only its bytes do.
        let server_read = tokio::spawn(async move {
            let mut total = 0_usize;
            if let Ok((_send, mut recv)) = server_conn.accept_bi().await {
                let mut chunk = [0_u8; 4096];
                while let Ok(read) = tokio::io::AsyncReadExt::read(&mut recv, &mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    total = total.saturating_add(read);
                }
            }
            total
        });
        open(client_conn, oversized_hello, tx).await;
        assert!(matches!(
            rx.recv().await,
            Some(InternalEvent::SiblingFailed(peer)) if peer == peer_id
        ));
        // No byte of the refused hello reached the server.
        assert_eq!(server_read.await.unwrap(), 0);
        a.close().await;
        let _ = router.shutdown().await;

        // Incoming oversized length prefix is refused before allocating the payload.
        let too_big = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_le_bytes();
        let mut slice = too_big.as_slice();
        assert!(read_frame(&mut slice).await.is_err());

        // Clean EOF and truncation are errors.
        let mut empty: &[u8] = &[];
        assert!(read_frame(&mut empty).await.is_err());
        let mut cut: &[u8] = &[4, 0, 0, 0, 1, 2];
        assert!(read_frame(&mut cut).await.is_err());

        // Trailing bytes on Frame::Msg are rejected, confirming Frame::decode
        // routes through NeighbourMsg::decode rather than plain postcard::from_bytes.
        let mut msg_body = postcard::to_allocvec(&Frame::Msg(NeighbourMsg::MlsGovernanceWanted {
            workspace: "00000000-0000-4000-8000-000000000001".into(),
            from_epoch: 1,
        }))
        .unwrap();
        msg_body.push(0xFF);
        let mut framed = Vec::new();
        write_frame(&mut framed, &msg_body).await.unwrap();
        let mut slice = framed.as_slice();
        assert!(read_frame(&mut slice).await.is_err());
    }
}
