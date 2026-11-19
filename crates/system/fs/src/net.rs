//! Where the files daemon reaches the network: an endpoint of its own, or the session's.
//!
//! A device has one endpoint id: its device key, which the session's endpoint binds. A relay keeps
//! one connection per endpoint id, so a second endpoint under that key in the daemon would keep
//! replacing the session's there, and each would lose its relay path in turn. Beside a session the
//! daemon therefore binds no network endpoint. It binds a loopback-only endpoint under a throwaway
//! key and links to the session through it ([`SessionLink`]):
//!
//! - the daemon writes that endpoint's address to its link file; the session watches the file and
//!   says hello, with where the daemon reaches it and the device's own address;
//! - a peer that reaches the device on a files protocol reaches the session, which opens a
//!   connection to the daemon naming the peer and carries every stream between the two;
//! - the daemon dials a peer by asking the session to: the session dials from the device's
//!   endpoint and carries the streams the same way.
//!
//! Only the device key opens the session's side: the daemon admits link connections from the
//! device's id alone. The session admits a dial only from the daemon endpoint it said hello to,
//! whose address only the daemon can write.
//!
//! A daemon with no session (a home peer, a test) binds its own endpoint under the device key
//! ([`Net::Direct`]): nothing else on that machine holds the key.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use collab_types::WorkspaceId;
use futures_util::future::BoxFuture;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler, Router};
use iroh_base::{EndpointAddr, EndpointId};
use key_group::ContentKeys;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tokio::io::AsyncReadExt as _;

/// The link between the session and its files daemon, both ways.
pub const LINK_ALPN: &[u8] = b"tilo-fs/session-link/1";

/// Local-only request for one workspace's Files keys, authenticated like the daemon link.
const FILE_KEYS_ALPN: &[u8] = b"tilo-fs/file-keys/1";
const MAX_FILE_KEY_BYTES: usize = 64 * 1024 * 1024;

/// The link file's name, beside the daemon's document-media socket.
pub const LINK_FILE: &str = "fs-session-link";

/// The protocols a peer reaches the daemon on through the session.
pub fn daemon_alpns() -> [&'static [u8]; 2] {
    [
        iroh_blobs::ALPN,
        crate::workspace_authority::TILO_FS_AUTHORITY_ALPN,
    ]
}

/// The largest opening frame either side reads.
const MAX_FRAME: u32 = 16 * 1024;

/// How a connection between the session and the daemon begins.
#[derive(Debug, Serialize, Deserialize)]
enum Opening {
    /// The session is here: the daemon dials out through `session` while this connection lives,
    /// and peers reach the device at `device`.
    Hello {
        session: EndpointAddr,
        device: EndpointAddr,
    },
    /// `remote` reached the device on `alpn`; every later stream of this connection is theirs.
    Inbound { remote: EndpointId, alpn: Vec<u8> },
}

/// The daemon asks the session to dial `target` on `alpn`; every later stream of this connection
/// is that connection's.
#[derive(Debug, Serialize, Deserialize)]
struct Dial {
    target: EndpointAddr,
    alpn: Vec<u8>,
}

/// The daemon's network: an endpoint of its own under the device key, or the session's.
#[derive(Clone, Debug)]
pub enum Net {
    Direct {
        endpoint: Endpoint,
        lookup: MemoryLookup,
    },
    Session(SessionLink),
}

impl From<Endpoint> for Net {
    fn from(endpoint: Endpoint) -> Self {
        Self::Direct {
            endpoint,
            lookup: MemoryLookup::new(),
        }
    }
}

impl Net {
    /// The id peers know this device by.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        match self {
            Self::Direct { endpoint, .. } => endpoint.id(),
            Self::Session(link) => link.device,
        }
    }

    /// Where peers reach this device: the session's address once it said hello, else the id alone.
    #[must_use]
    pub fn addr(&self) -> EndpointAddr {
        match self {
            Self::Direct { endpoint, .. } => endpoint.addr(),
            Self::Session(link) => link
                .hello()
                .map_or_else(|| EndpointAddr::new(link.device), |hello| hello.device),
        }
    }

    /// Remember where `addr` is reached, for a later dial by id.
    pub fn learn(&self, addr: &EndpointAddr) {
        if let Self::Direct { lookup, .. } = self {
            lookup.add_endpoint_info(addr.clone());
        }
    }

    /// Open a connection to `target` on `alpn`.
    ///
    /// # Errors
    ///
    /// When the peer, or the session the daemon dials through, cannot be reached.
    pub async fn connect(&self, target: EndpointAddr, alpn: &[u8]) -> Result<Connection> {
        match self {
            Self::Direct { endpoint, .. } => Ok(endpoint.connect(target, alpn).await?),
            Self::Session(link) => link.connect(target, alpn).await,
        }
    }

    /// Serve `answer`'s protocols to the peers that reach this device.
    pub(crate) fn serve(&self, answer: Arc<dyn Answer>) -> Router {
        match self {
            Self::Direct { endpoint, .. } => {
                let mut router = Router::builder(endpoint.clone());
                for alpn in answer.alpns() {
                    router = router.accept(
                        alpn,
                        Direct {
                            answer: Arc::clone(&answer),
                        },
                    );
                }
                router.spawn()
            }
            Self::Session(link) => Router::builder(link.endpoint.clone())
                .accept(
                    LINK_ALPN,
                    Linked {
                        link: link.clone(),
                        answer,
                    },
                )
                .spawn(),
        }
    }
}

/// What the daemon answers a peer on each of its protocols, given who the peer is.
pub(crate) trait Answer: Send + Sync + 'static {
    fn alpns(&self) -> Vec<&'static [u8]>;
    fn answer(
        &self,
        alpn: &[u8],
        remote: EndpointId,
        connection: Connection,
    ) -> BoxFuture<'static, Result<(), AcceptError>>;
}

#[derive(Clone)]
struct Direct {
    answer: Arc<dyn Answer>,
}

impl std::fmt::Debug for Direct {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Direct")
    }
}

impl ProtocolHandler for Direct {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let alpn = connection.alpn().to_vec();
        let remote = connection.remote_id();
        self.answer.answer(&alpn, remote, connection).await
    }
}

/// What the session said in its hello.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// Where the daemon dials the session.
    pub session: EndpointAddr,
    /// Where peers reach this device.
    pub device: EndpointAddr,
}

/// The daemon's side of its link to the session.
#[derive(Clone)]
pub struct SessionLink {
    endpoint: Endpoint,
    device: EndpointId,
    hello: Arc<tokio::sync::watch::Sender<Option<Hello>>>,
}

impl std::fmt::Debug for SessionLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionLink")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl SessionLink {
    /// Bind the daemon's loopback endpoint for the device `device`, and write where it listens to
    /// `file` for the session.
    ///
    /// # Errors
    ///
    /// When the endpoint cannot bind or the file cannot be written.
    pub async fn bind(device: EndpointId, file: &Path) -> Result<Self> {
        // Its keep-alives hold the session's hello open while nothing moves.
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .transport_config(crate::sync::transport())
            .bind_addr((Ipv4Addr::LOCALHOST, 0))?
            .bind()
            .await
            .context("bind the session link's loopback endpoint")?;
        let here = loopback(&endpoint)?;
        // The address is no secret: the daemon admits only the device key on it.
        tilo_files::native::atomic::write_with_mode(file, &postcard::to_allocvec(&here)?, 0o644)
            .with_context(|| format!("write the session link {}", file.display()))?;
        Ok(Self {
            endpoint,
            device,
            hello: Arc::new(tokio::sync::watch::channel(None).0),
        })
    }

    /// What the session said last, while it is there.
    #[must_use]
    pub fn hello(&self) -> Option<Hello> {
        self.hello.borrow().clone()
    }

    /// Follow the session coming and going.
    #[must_use]
    pub fn hellos(&self) -> tokio::sync::watch::Receiver<Option<Hello>> {
        self.hello.subscribe()
    }

    pub(crate) async fn file_keys(&self, workspace: WorkspaceId) -> Result<ContentKeys> {
        let hello = self
            .hello()
            .context("no login session is connected to Files")?;
        let connection = self.endpoint.connect(hello.session, FILE_KEYS_ALPN).await?;
        ensure!(
            connection.remote_id() == self.device,
            "the session answered as another device"
        );
        let (mut send, mut recv) = connection.open_bi().await?;
        write_frame(&mut send, &workspace).await?;
        send.finish()?;
        let response: Result<(), String> = read_frame(&mut recv).await?;
        response.map_err(|reason| anyhow!(reason))?;
        let bytes = zeroize::Zeroizing::new(recv.read_to_end(MAX_FILE_KEY_BYTES).await?);
        Ok(ContentKeys::from_bytes(&bytes)?)
    }

    async fn connect(&self, target: EndpointAddr, alpn: &[u8]) -> Result<Connection> {
        let Some(hello) = self.hello() else {
            bail!("no session runs to reach {} through", target.id);
        };
        let connection = self.endpoint.connect(hello.session, LINK_ALPN).await?;
        ensure!(
            connection.remote_id() == self.device,
            "the session link answered as another device"
        );
        let (mut send, mut recv) = connection.open_bi().await?;
        write_frame(
            &mut send,
            &Dial {
                target,
                alpn: alpn.to_vec(),
            },
        )
        .await?;
        send.finish()?;
        let reached: Result<(), String> = read_frame(&mut recv).await?;
        reached.map_err(|error| anyhow!(error))?;
        Ok(connection)
    }
}

/// The daemon's answer to the session on its link endpoint.
#[derive(Clone)]
struct Linked {
    link: SessionLink,
    answer: Arc<dyn Answer>,
}

impl std::fmt::Debug for Linked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Linked")
    }
}

impl ProtocolHandler for Linked {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if connection.remote_id() != self.link.device {
            return Err(refused(
                "only this device's session links to its files daemon",
            ));
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        let opening: Opening = read_frame(&mut recv).await.map_err(refused)?;
        let _ = send.finish();
        match opening {
            Opening::Hello { session, device } => {
                let hello = Hello { session, device };
                self.link.hello.send_replace(Some(hello.clone()));
                connection.closed().await;
                // A later hello may have replaced this one already.
                self.link.hello.send_if_modified(|held| {
                    let ours = held.as_ref() == Some(&hello);
                    if ours {
                        *held = None;
                    }
                    ours
                });
                Ok(())
            }
            Opening::Inbound { remote, alpn } => {
                if !self.answer.alpns().contains(&alpn.as_slice()) {
                    return Err(refused("the daemon does not answer that protocol"));
                }
                self.answer.answer(&alpn, remote, connection).await
            }
        }
    }
}

/// The session's side of the link: it answers the daemon's protocols for it on the device's
/// endpoint, and dials for it.
pub struct DaemonLink {
    endpoint: Endpoint,
    daemon: Arc<Mutex<Option<EndpointAddr>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl DaemonLink {
    /// The protocols the session serves on `endpoint` for the daemon whose link file is `file`.
    /// `wake` is called when the daemon dials a peer, which may then dial this device back.
    ///
    /// Spawns the hello on the current runtime: it waits for the file and greets each daemon
    /// that writes it.
    #[must_use]
    pub fn serve(
        endpoint: Endpoint,
        file: PathBuf,
        wake: Arc<dyn Fn() + Send + Sync>,
        identity_socket: Option<PathBuf>,
    ) -> Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)> {
        let link = Arc::new(Self {
            endpoint,
            daemon: Arc::new(Mutex::new(None)),
            wake,
        });
        tokio::spawn(greet(Arc::clone(&link), file));
        let mut served: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)> = vec![(
            LINK_ALPN.to_vec(),
            Box::new(Outward {
                link: Arc::clone(&link),
            }),
        )];
        if let Some(identity_socket) = identity_socket {
            served.push((
                FILE_KEYS_ALPN.to_vec(),
                Box::new(FilesKeyRequest {
                    link: Arc::clone(&link),
                    identity_socket,
                }),
            ));
        }
        for alpn in daemon_alpns() {
            served.push((
                alpn.to_vec(),
                Box::new(Inward {
                    link: Arc::clone(&link),
                    alpn,
                }),
            ));
        }
        served
    }

    fn daemon(&self) -> Option<EndpointAddr> {
        self.daemon.lock_recover().clone()
    }
}

/// Ask the signed-in person's identity service for the Files document keys of one workspace.
#[derive(Clone)]
struct FilesKeyRequest {
    link: Arc<DaemonLink>,
    identity_socket: PathBuf,
}

impl std::fmt::Debug for FilesKeyRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FilesKeyRequest")
    }
}

impl ProtocolHandler for FilesKeyRequest {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if self.link.daemon().map(|daemon| daemon.id) != Some(connection.remote_id()) {
            return Err(refused(
                "only this device's Files daemon can request local keys",
            ));
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        let workspace: WorkspaceId = read_frame(&mut recv).await.map_err(refused)?;
        let result = crate::hydration::FileKeys::new(workspace, self.identity_socket.clone())
            .access()
            .await
            .and_then(|keys| keys.to_bytes().map_err(anyhow::Error::from))
            .and_then(|bytes| {
                ensure!(
                    bytes.len() <= MAX_FILE_KEY_BYTES,
                    "the Files key history is too large"
                );
                Ok(bytes)
            });
        write_frame(
            &mut send,
            &result.as_ref().map(|_| ()).map_err(ToString::to_string),
        )
        .await
        .map_err(refused)?;
        if let Ok(bytes) = result {
            send.write_all(&bytes).await.map_err(refused)?;
        }
        send.finish()?;
        // Keep the authenticated connection alive until the requester has received the response.
        let _ = send.stopped().await.map_err(refused)?;
        Ok(())
    }
}

/// Greet every daemon that writes `file`, for as long as it runs.
async fn greet(link: Arc<DaemonLink>, file: PathBuf) {
    let mut changes = match crate::file_watch::FileChanges::for_file(&file) {
        Ok(changes) => changes,
        Err(error) => {
            tracing::debug!(%error, file = %file.display(), "no files daemon links to this session");
            return;
        }
    };
    loop {
        changes.next().await;
        let Some(daemon) = std::fs::read(&file)
            .ok()
            .and_then(|bytes| postcard::from_bytes::<EndpointAddr>(&bytes).ok())
        else {
            continue;
        };
        if link.daemon().as_ref() == Some(&daemon) {
            continue;
        }
        let connection = match hello(&link.endpoint, daemon.clone()).await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::debug!(%error, "the files daemon in the link file does not answer");
                continue;
            }
        };
        *link.daemon.lock_recover() = Some(daemon.clone());
        tracing::info!(daemon = %daemon.id, "the files daemon reaches the network through this session");
        let link = Arc::clone(&link);
        tokio::spawn(async move {
            connection.closed().await;
            let mut held = link.daemon.lock_recover();
            if held.as_ref() == Some(&daemon) {
                *held = None;
            }
        });
    }
}

async fn hello(endpoint: &Endpoint, daemon: EndpointAddr) -> Result<Connection> {
    let connection = endpoint.connect(daemon, LINK_ALPN).await?;
    let (mut send, _recv) = connection.open_bi().await?;
    write_frame(
        &mut send,
        &Opening::Hello {
            session: loopback(endpoint)?,
            device: endpoint.addr(),
        },
    )
    .await?;
    send.finish()?;
    Ok(connection)
}

/// A peer reached the device on one of the daemon's protocols: hand it to the daemon.
#[derive(Clone)]
struct Inward {
    link: Arc<DaemonLink>,
    alpn: &'static [u8],
}

impl std::fmt::Debug for Inward {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Inward")
    }
}

impl ProtocolHandler for Inward {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Some(daemon) = self.link.daemon() else {
            return Err(refused("no files daemon runs on this device"));
        };
        let opened = async {
            let local = self.link.endpoint.connect(daemon, LINK_ALPN).await?;
            let (mut send, _recv) = local.open_bi().await?;
            write_frame(
                &mut send,
                &Opening::Inbound {
                    remote: connection.remote_id(),
                    alpn: self.alpn.to_vec(),
                },
            )
            .await?;
            send.finish()?;
            anyhow::Ok(local)
        };
        let local = opened.await.map_err(refused)?;
        splice(connection, local).await;
        Ok(())
    }
}

/// The daemon dials a peer through the session.
#[derive(Clone)]
struct Outward {
    link: Arc<DaemonLink>,
}

impl std::fmt::Debug for Outward {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Outward")
    }
}

impl ProtocolHandler for Outward {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if self.link.daemon().map(|daemon| daemon.id) != Some(connection.remote_id()) {
            return Err(refused(
                "only the files daemon this session greeted dials through it",
            ));
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        let dial: Dial = read_frame(&mut recv).await.map_err(refused)?;
        if !daemon_alpns().contains(&dial.alpn.as_slice()) {
            let _ = write_frame(&mut send, &Err::<(), _>("not a files protocol".to_owned())).await;
            return Err(refused("the daemon dials only its own protocols"));
        }
        (self.link.wake)();
        let reached = self
            .link
            .endpoint
            .connect(dial.target, &dial.alpn)
            .await
            .map_err(|error| error.to_string());
        write_frame(
            &mut send,
            &reached.as_ref().map(|_| ()).map_err(Clone::clone),
        )
        .await
        .map_err(refused)?;
        let _ = send.finish();
        if let Ok(remote) = reached {
            splice(connection, remote).await;
        }
        Ok(())
    }
}

/// Where `endpoint` listens on this machine's loopback.
fn loopback(endpoint: &Endpoint) -> Result<EndpointAddr> {
    let port = endpoint
        .bound_sockets()
        .into_iter()
        .find(SocketAddr::is_ipv4)
        .context("the endpoint listens on no IPv4 socket")?
        .port();
    Ok(
        EndpointAddr::new(endpoint.id())
            .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
    )
}

/// Carry every stream between `a` and `b` until either ends: a stream one opens is opened on the
/// other, and its bytes copied both ways. Then both close.
pub async fn splice(a: Connection, b: Connection) {
    let mut pipes = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            opened = a.accept_bi() => match opened {
                Ok(streams) => { pipes.spawn(carry_bi(streams, b.clone())); }
                Err(_) => break,
            },
            opened = b.accept_bi() => match opened {
                Ok(streams) => { pipes.spawn(carry_bi(streams, a.clone())); }
                Err(_) => break,
            },
            opened = a.accept_uni() => match opened {
                Ok(recv) => { pipes.spawn(carry_uni(recv, b.clone())); }
                Err(_) => break,
            },
            opened = b.accept_uni() => match opened {
                Ok(recv) => { pipes.spawn(carry_uni(recv, a.clone())); }
                Err(_) => break,
            },
        }
    }
    // What either side sent before it ended still reaches the other.
    while pipes.join_next().await.is_some() {}
    a.close(0u32.into(), b"");
    b.close(0u32.into(), b"");
}

async fn carry_bi((send, recv): (SendStream, RecvStream), to: Connection) {
    let Ok((there_send, there_recv)) = to.open_bi().await else {
        return;
    };
    tokio::join!(pipe(recv, there_send), pipe(there_recv, send));
}

async fn carry_uni(recv: RecvStream, to: Connection) {
    if let Ok(send) = to.open_uni().await {
        pipe(recv, send).await;
    }
}

async fn pipe(mut from: RecvStream, mut to: SendStream) {
    if tokio::io::copy(&mut from, &mut to).await.is_ok() {
        let _ = to.finish();
    } else {
        let _ = to.reset(0u32.into());
        let _ = from.stop(0u32.into());
    }
}

async fn write_frame<T: Serialize + Sync>(send: &mut SendStream, value: &T) -> Result<()> {
    let bytes = postcard::to_allocvec(value)?;
    let len = u32::try_from(bytes.len()).context("a frame too long")?;
    ensure!(len <= MAX_FRAME, "a frame too long");
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read_frame<T: for<'de> Deserialize<'de>>(recv: &mut RecvStream) -> Result<T> {
    let len = recv.read_u32().await?;
    ensure!(len <= MAX_FRAME, "a frame too long");
    let mut bytes = vec![0; usize::try_from(len)?];
    recv.read_exact(&mut bytes).await?;
    Ok(postcard::from_bytes(&bytes)?)
}

fn refused(error: impl std::fmt::Display) -> AcceptError {
    AcceptError::from_err(std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::hydration::Hydrator;
    use crate::store::ContentStore;

    /// Answers blobs from `store`, and remembers who it was told it answered.
    struct Blobs {
        store: Arc<ContentStore>,
        answered: Arc<Mutex<Vec<EndpointId>>>,
    }

    impl Answer for Blobs {
        fn alpns(&self) -> Vec<&'static [u8]> {
            vec![iroh_blobs::ALPN]
        }

        fn answer(
            &self,
            _alpn: &[u8],
            remote: EndpointId,
            connection: Connection,
        ) -> BoxFuture<'static, Result<(), AcceptError>> {
            use futures_util::FutureExt as _;
            self.answered.lock_recover().push(remote);
            let blobs = self.store.blobs_protocol(None);
            async move { ProtocolHandler::accept(&blobs, connection).await }.boxed()
        }
    }

    async fn on_relay(secret: iroh::SecretKey, relay: &tilo_test_env::relay::Relay) -> Endpoint {
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(relay.relays()))
            .secret_key(secret)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback bind address")
            .bind()
            .await
            .expect("an endpoint on the test relay");
        tilo_test_env::within("the endpoint to take the relay as home", endpoint.online()).await;
        endpoint
    }

    /// The relay counts each client that a later one with the same endpoint id displaced.
    fn displaced(relay: &tilo_test_env::relay::Relay) -> u64 {
        relay.server().metrics().server.clients_inactive_added.get()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_second_endpoint_under_the_device_key_displaces_the_first_on_the_relay() {
        let relay = tilo_test_env::relay::relay().await;
        let device = iroh::SecretKey::generate();
        let _session = on_relay(device.clone(), &relay).await;
        let _daemon = on_relay(device, &relay).await;
        tilo_test_env::within("the relay to displace the first endpoint", async {
            while displaced(&relay) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
    }

    fn answer_files_identity(
        listener: std::os::unix::net::UnixListener,
        workspace: WorkspaceId,
        keys: &ContentKeys,
    ) {
        use tilo_identity_client::wire::{
            FRAMING, HANDSHAKE_FRAMING, KeyAccess, PresentationHandshake, Request, Response,
        };
        let (mut stream, _) = listener.accept().unwrap();
        let hello: PresentationHandshake =
            serde_json::from_slice(&HANDSHAKE_FRAMING.read(&mut stream).unwrap().unwrap()).unwrap();
        assert_eq!(hello.handshake.workspace_id, workspace.to_string());
        assert_eq!(hello.handshake.app_id, fs_protocol::tree::FILES_APP);
        assert!(matches!(
            postcard::from_bytes::<Request>(&FRAMING.read(&mut stream).unwrap().unwrap()).unwrap(),
            Request::Metadata
        ));
        let metadata = Response::Metadata {
            display_name: "Files test".into(),
            avatar_color: [0; 3],
            group_public_key: [0; 32],
            signing_public_key: [0; 32],
            device_certificate: None,
            threshold: 1,
            total_shares: 1,
            share_index: 1,
        };
        FRAMING
            .write(&mut stream, &postcard::to_allocvec(&metadata).unwrap())
            .unwrap();
        let request =
            postcard::from_bytes::<Request>(&FRAMING.read(&mut stream).unwrap().unwrap()).unwrap();
        assert!(
            matches!(request, Request::GroupKeys { workspace_id, app_id, document_id, known_generation: 0 }
                if workspace_id == workspace.to_string()
                    && app_id == fs_protocol::tree::FILES_APP
                    && document_id == fs_protocol::tree::document_id())
        );
        let answer = Response::GroupKeys {
            access: KeyAccess::Ready,
            generation: 1,
            keys: keys.to_bytes().unwrap().to_vec(),
        };
        FRAMING
            .write(&mut stream, &postcard::to_allocvec(&answer).unwrap())
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn system_files_requests_keys_over_its_authenticated_session_link() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(LINK_FILE);
        let socket = dir.path().join("identity.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let workspace = WorkspaceId::new();
        let keys = key_group::KeyGroup::create(
            workspace,
            key_group::MemberKey::generate("member").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap();
        let kept = keys.clone();
        let broker = std::thread::spawn(move || answer_files_identity(listener, workspace, &kept));

        let session = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let link = SessionLink::bind(session.id(), &file).await.unwrap();
        let _daemon = Net::Session(link.clone()).serve(Arc::new(Blobs {
            store: Arc::new(ContentStore::memory()),
            answered: Arc::new(Mutex::new(Vec::new())),
        }));
        let mut router = Router::builder(session.clone());
        for (alpn, handler) in
            DaemonLink::serve(session.clone(), file, Arc::new(|| {}), Some(socket))
        {
            router = router.accept(alpn, handler);
        }
        let _session = router.spawn();
        let mut hellos = link.hellos();
        tilo_test_env::within(
            "the system Files daemon answers the session's hello",
            hellos.wait_for(Option::is_some),
        )
        .await
        .unwrap();
        let obtained = tilo_test_env::within(
            "the Files daemon receives its workspace keys",
            link.file_keys(workspace),
        )
        .await
        .unwrap();
        assert_eq!(&*obtained.to_bytes().unwrap(), &*keys.to_bytes().unwrap());
        broker.join().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn files_reach_and_leave_the_device_through_the_sessions_endpoint() {
        let relay = tilo_test_env::relay::relay().await;
        let device = iroh::SecretKey::generate();
        let dir = tempfile::tempdir().expect("a runtime directory");
        let file = dir.path().join(LINK_FILE);

        // The daemon's side first, as a daemon that started before the login.
        let link = SessionLink::bind(device.public(), &file)
            .await
            .expect("the daemon's link endpoint");
        let net = Net::Session(link.clone());
        let held = Arc::new(ContentStore::memory());
        let here = held
            .ingest(b"written on this device".to_vec())
            .await
            .expect("content kept here");
        let answered = Arc::new(Mutex::new(Vec::new()));
        let _router = net.serve(Arc::new(Blobs {
            store: Arc::clone(&held),
            answered: Arc::clone(&answered),
        }));

        // The session binds the device's one endpoint and greets the daemon.
        let session = on_relay(device.clone(), &relay).await;
        let mut router = Router::builder(session.clone());
        for (alpn, handler) in DaemonLink::serve(session.clone(), file, Arc::new(|| {}), None) {
            router = router.accept(alpn, handler);
        }
        let _session_router = router.spawn();
        let mut hellos = link.hellos();
        tilo_test_env::within(
            "the session to greet the daemon",
            hellos.wait_for(Option::is_some),
        )
        .await
        .expect("the daemon hears the session");
        assert_eq!(net.id(), device.public());
        assert_eq!(net.addr().id, device.public());

        // A peer elsewhere reaches the device on the relay alone, and is answered by the daemon,
        // which is told who it is.
        let peer_key = iroh::SecretKey::generate();
        let peer = on_relay(peer_key.clone(), &relay).await;
        let device_on_relay =
            EndpointAddr::new(device.public()).with_relay_url(relay.url().clone());
        let fetched = Hydrator::new(peer.clone(), Arc::new(ContentStore::memory()))
            .fetch_from(here, device_on_relay)
            .await
            .expect("the device's content reaches the peer");
        assert_eq!(&fetched[..], b"written on this device");
        assert_eq!(*answered.lock_recover(), vec![peer_key.public()]);

        // The daemon fetches what the peer keeps, through the session.
        let theirs = Arc::new(ContentStore::memory());
        let there = theirs
            .ingest(b"written elsewhere".to_vec())
            .await
            .expect("content kept there");
        let _peer_router = Router::builder(peer.clone())
            .accept(iroh_blobs::ALPN, theirs.blobs_protocol(None))
            .spawn();
        let peer_on_relay =
            EndpointAddr::new(peer_key.public()).with_relay_url(relay.url().clone());
        let fetched = Hydrator::new(net.clone(), Arc::new(ContentStore::memory()))
            .fetch_from(there, peer_on_relay)
            .await
            .expect("the peer's content reaches the device");
        assert_eq!(&fetched[..], b"written elsewhere");

        // Throughout, the device had one endpoint on the relay.
        assert_eq!(displaced(&relay), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_daemon_answers_only_its_own_devices_session() {
        let dir = tempfile::tempdir().expect("a runtime directory");
        let file = dir.path().join(LINK_FILE);
        let link = SessionLink::bind(iroh::SecretKey::generate().public(), &file)
            .await
            .expect("the daemon's link endpoint");
        let _router = Net::Session(link.clone()).serve(Arc::new(Blobs {
            store: Arc::new(ContentStore::memory()),
            answered: Arc::new(Mutex::new(Vec::new())),
        }));
        let stranger = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .expect("an endpoint of another device");
        let daemon: EndpointAddr =
            postcard::from_bytes(&std::fs::read(&file).expect("the link file"))
                .expect("an address");
        // The daemon refuses the link of a device that is not its own, and iroh reports that where
        // it lands: the handshake, or the connection the stranger already opened. Either way the
        // stranger holds no link and the daemon takes no hello.
        if let Ok(connection) = hello(&stranger, daemon).await {
            let _ = tilo_test_env::within(
                "the daemon to close another device's link",
                connection.closed(),
            )
            .await;
        }
        assert!(link.hello().is_none(), "another device's hello is refused");
    }
}
