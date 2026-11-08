//! Direct sync of one workspace replica between two of its devices over an
//! iroh connection, with no workspace host in between.
//!
//! The connection's endpoint id is the remote device key, so the QUIC
//! handshake already proves who sent every frame: a device that is not a
//! certified, unrevoked device of the replica's roster is refused before a
//! frame is read, and everything it sends is admitted as its own live
//! traffic through [`StatePeer::receive_from`], the same admission a workspace
//! host's relayed delta takes.
//!
//! A link is three streams. Each side opens one unidirectional stream of
//! length-prefixed postcard [`NeighbourMsg`] frames: what the other may lack
//! first, then every message it publishes while the link lasts. A
//! bidirectional control stream carries the dialling device's ask for the
//! other device's retained history ([`DirectSync::with_store`]) and the frames
//! its store holds, so a device that carries a workspace's history serves a
//! member the same way a host does.
//!
//! A host of several workspaces answers them all on [`DIRECT_ALPN`] through
//! one [`Serving`], which dispatches each connection to the workspace its
//! first frame names.

use crate::actions::now_ns;
use crate::frames::LinkFrame;
pub use crate::replica::Replica;
use crate::store::{self, Budget, Cursor, Frame, Store};
use crate::{NeighbourMsg, StatePeer, TransportOrigin};
use ed25519_dalek::SigningKey;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use neighbour_wire::peer_handle;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tilo_lock::RwLockRecover as _;
use tokio::sync::{broadcast, watch};
use web_time::SystemTime;

/// ALPN of a direct replica link between two devices of one workspace.
pub const DIRECT_ALPN: &[u8] = b"tilo-peer-state-direct/1";

/// The largest frame: a document checkpoint with space to spare.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Messages published while a link is writing; a link further behind than
/// this ends, and the next connection catches up from the checkpoint.
pub(crate) const OUTGOING: usize = 1024;

/// Application close code of a refused connection.
pub(crate) const REFUSED: u32 = 1;

/// A device's replica, shared by the device's engine and its direct links.
///
/// Clone it to hand it to an [`iroh::protocol::Router`] under [`DIRECT_ALPN`]
/// (it is the accepting [`ProtocolHandler`]) and to dial with
/// [`Self::connect`]; every clone is the same replica.
#[derive(Clone)]
pub struct DirectSync(Arc<Inner>);

#[derive(Clone)]
struct Admission {
    owner: crate::owner::NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
}

struct Inner {
    replica: Replica,
    /// Where this device's engine owns the workspace: frames are admitted into
    /// its live replica and published as they land.
    admission: Option<Admission>,
    key: SigningKey,
    /// The sealed history this device keeps for the workspace, when it is a
    /// store device.
    store: Option<Arc<Store>>,
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
        Self::over(Replica::new(replica), key)
    }

    /// Sync `replica`, which other parts of the engine read as well: the
    /// action plane authorizes against the state this link admits into.
    #[must_use]
    pub fn over(replica: Replica, key: SigningKey) -> Self {
        Self(Arc::new(Inner {
            replica,
            admission: None,
            key,
            store: None,
            outgoing: broadcast::channel(OUTGOING).0,
            changed: watch::channel(0).0,
        }))
    }

    /// Keep this workspace's sealed history at `store` and hand it to the
    /// devices that ask: what a store device does with every link. Call it
    /// before cloning the link to its handlers.
    #[must_use]
    pub fn with_store(self, store: Arc<Store>) -> Self {
        Self(Arc::new(Inner {
            replica: self.0.replica.clone(),
            admission: self.0.admission.clone(),
            key: self.0.key.clone(),
            store: Some(store),
            outgoing: self.0.outgoing.clone(),
            changed: self.0.changed.clone(),
        }))
    }

    /// Admit every frame this link brings into `owner`'s live replica of
    /// `workspace` and publish it, instead of into this device's copy alone:
    /// what a link brings is durable as it lands, and the device's own engine
    /// decides it. The link then syncs that replica.
    pub fn owning(
        self,
        owner: crate::owner::NativeWorkspaceOwner,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> Result<Self, String> {
        let replica = owner.replica(workspace)?;
        Ok(Self(Arc::new(Inner {
            replica,
            admission: Some(Admission { owner, workspace }),
            key: self.0.key.clone(),
            store: self.0.store.clone(),
            outgoing: self.0.outgoing.clone(),
            changed: self.0.changed.clone(),
        })))
    }

    /// The replica this link syncs.
    #[must_use]
    pub fn replica(&self) -> Replica {
        self.0.replica.clone()
    }

    /// The store this device keeps the workspace's history at, when it is a
    /// store device.
    #[must_use]
    pub fn store(&self) -> Option<&Arc<Store>> {
        self.0.store.as_ref()
    }

    /// Read the replica.
    pub fn read<R>(&self, read: impl FnOnce(&StatePeer) -> R) -> R {
        self.0.replica.read(read)
    }

    /// Make a local change with the device's key, e.g.
    /// `publication_mut().publish_incremental`, keep the message it returns at
    /// this device's store, send it to every open link, and return it.
    pub fn publish(
        &self,
        write: impl FnOnce(&mut StatePeer, &SigningKey, SystemTime) -> Result<NeighbourMsg, String>,
    ) -> Result<NeighbourMsg, String> {
        let message = write(&mut self.0.replica.lock(), &self.0.key, SystemTime::now())?;
        self.0.changed.send_modify(bump);
        self.keep(&message);
        // No open link is no error: the next one catches up from the replica.
        let _ = self.0.outgoing.send(message.clone());
        Ok(message)
    }

    /// Admit a frame a store replayed, as the retained history of its
    /// author: the same admission a live frame from that device takes.
    pub fn admit_retained(&self, author: [u8; 32], message: NeighbourMsg) -> Result<(), String> {
        let outcome = self.0.replica.lock().receive_from(
            TransportOrigin::RetainedRelayDelta(author),
            message,
            SystemTime::now(),
        )?;
        if outcome.changed || outcome.metadata_changed {
            self.0.changed.send_modify(bump);
        }
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
    pub async fn connect(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
    ) -> Result<(), String> {
        let connection = endpoint
            .connect(addr, DIRECT_ALPN)
            .await
            .map_err(|error| error.to_string())?;
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| error.to_string())?;
        let workspace = self.workspace_number()?;
        crate::frames::write_link(&mut send, &LinkFrame::Workspace(workspace)).await?;
        self.link(&connection, true, Control { send, recv }).await
    }

    /// Serve the connection a device dialled this one: its first frame names
    /// the workspace.
    ///
    /// # Errors
    ///
    /// The device named no workspace, or one this replica is not.
    pub async fn serve(&self, connection: Connection) -> Result<(), String> {
        let (named, send, recv) = dialed_workspace(&connection).await?;
        if named != self.workspace_number()? {
            return Err(refuse(&connection, "this device holds no such workspace"));
        }
        self.link(&connection, false, Control { send, recv }).await
    }

    /// Sync with the device on the other end of `connection` until the link
    /// ends, whichever side dialled.
    async fn link(
        &self,
        connection: &Connection,
        dialing: bool,
        control: Control,
    ) -> Result<(), String> {
        let remote = *connection.remote_id().as_bytes();
        if let Err(reason) = self.admits(remote) {
            return Err(refuse(connection, &reason));
        }
        let certificate = self.certificate()?;
        // Subscribe before taking the catch-up, so a change published in
        // between is sent after it rather than lost.
        let live = self.0.outgoing.subscribe();
        let catch_up = self.catch_up()?;
        let send = connection
            .open_uni()
            .await
            .map_err(|error| error.to_string())?;
        let sending = send_all(send, remote, catch_up, live);
        let receiving = self.receive_all(remote, &certificate, connection);
        let history = async {
            if dialing {
                self.ask_store(control).await
            } else {
                self.answer_store(control).await
            }
        };
        tokio::select! {
            result = receiving => result,
            result = sending => result,
            result = history => result,
        }
    }

    /// Ask the other device's store for the frames this device lacks, round
    /// by round while it says more are left.
    async fn ask_store(&self, mut control: Control) -> Result<(), String> {
        let mut cursors = self.cursors();
        crate::frames::write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone())).await?;
        while let Some(frame) = crate::frames::read_link(&mut control.recv, MAX_FRAME_BYTES).await?
        {
            let LinkFrame::Frames { frames, more } = frame else {
                return Err("the device answered something else than retained frames".into());
            };
            for message in &frames {
                let author = store::author_of(message)
                    .ok_or("the store replayed a message that is not a document frame")?;
                self.admit_retained(author, message.clone())?;
            }
            store::advance(&mut cursors, &frames);
            if more {
                crate::frames::write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone()))
                    .await?;
            }
        }
        Ok(())
    }

    /// Answer the dialling device's cursors from this device's store.
    async fn answer_store(&self, mut control: Control) -> Result<(), String> {
        while let Some(frame) = crate::frames::read_link(&mut control.recv, MAX_FRAME_BYTES).await?
        {
            let LinkFrame::Cursors(cursors) = frame else {
                return Err("the device asked for something else than retained frames".into());
            };
            let replay = match &self.0.store {
                Some(store) => store
                    .replay(self.workspace_number()?, &cursors, Budget::default())
                    .map_err(|error| error.to_string())?,
                None => store::Replay::default(),
            };
            crate::frames::write_link(
                &mut control.send,
                &LinkFrame::Frames {
                    frames: replay.frames,
                    more: replay.more,
                },
            )
            .await?;
        }
        Ok(())
    }

    /// What this device's replica holds, as the cursors it asks a store to
    /// continue from.
    #[must_use]
    pub fn cursors(&self) -> Vec<Cursor> {
        self.read(store::cursors)
    }

    /// Whether `device` may sync with this replica: a certified device of
    /// its roster, unrevoked and unexpired.
    fn admits(&self, device: [u8; 32]) -> Result<(), String> {
        let replica = self.0.replica.lock();
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
        let mut replica = self.0.replica.lock();
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
    /// stays up for the frames after it, and the device files what it refused
    /// as its own verdict in the workspace document, so the write's author
    /// reads why it is not here.
    async fn receive_all(
        &self,
        remote: [u8; 32],
        certificate: &tilo_user_identity::DeviceCert,
        connection: &Connection,
    ) -> Result<(), String> {
        let mut recv = connection
            .accept_uni()
            .await
            .map_err(|error| error.to_string())?;
        while let Some(message) = read_frame(&mut recv).await? {
            if let Some(admission) = &self.0.admission {
                let report = admission.owner.receive(
                    admission.workspace,
                    TransportOrigin::RelayDelta(remote),
                    message.clone(),
                    Some(&self.0.key),
                    SystemTime::now(),
                );
                match report {
                    Ok(report) => {
                        let landed = report.changed || report.metadata_changed;
                        self.keep(&message);
                        if landed {
                            self.0.changed.send_modify(bump);
                            let _ = self.0.outgoing.send(message);
                        }
                    }
                    Err(reason) => {
                        tracing::warn!(%reason, "a direct link dropped a frame the workspace refused");
                    }
                }
                continue;
            }
            let (landed, filed, accepted) = {
                let mut replica = self.0.replica.lock();
                let now = SystemTime::now();
                match replica.receive_from(
                    TransportOrigin::RelayDelta(remote),
                    message.clone(),
                    now,
                ) {
                    Ok(outcome) => {
                        let landed = outcome.changed || outcome.metadata_changed;
                        let cleared = landed
                            && replica.workspace_mut().clear_landed_write(
                                certificate,
                                &message,
                                &self.0.key,
                                now,
                            )?;
                        // What the workspace took in, even without the key
                        // that opens it, is what a store device keeps.
                        (landed || cleared, false, true)
                    }
                    Err(reason) => {
                        // A document write the workspace refuses is its
                        // author's verdict, filed where it reads why its write
                        // is not here. Any other frame is the link's alone to
                        // drop.
                        let filed = if matches!(message, NeighbourMsg::AppDocument { .. }) {
                            replica.workspace_mut().refuse_write(
                                certificate,
                                &message,
                                &reason,
                                &self.0.key,
                                now,
                            )?
                        } else {
                            tracing::warn!(%reason, "a direct link dropped a frame the workspace refused");
                            false
                        };
                        (false, filed, false)
                    }
                }
            };
            if landed || filed {
                self.0.changed.send_modify(bump);
            }
            if accepted {
                self.keep(&message);
            }
            if landed {
                // What a link admits, the other links see: the device
                // carrying a workspace is its members' one path.
                let _ = self.0.outgoing.send(message);
            }
        }
        Ok(())
    }

    /// Keep `message` at this device's store when it is a sealed document
    /// frame, the only history a store holds.
    fn keep(&self, message: &NeighbourMsg) {
        let Some(store) = &self.0.store else {
            return;
        };
        let Ok(workspace) = self.0.replica.number() else {
            return;
        };
        let Ok(frame) = Frame::of(message) else {
            return;
        };
        if let Err(error) = store.retain(workspace, &frame) {
            tracing::warn!(%error, "a frame was not kept at the store");
        }
    }

    fn workspace_number(&self) -> Result<u64, String> {
        self.0.replica.number()
    }

    /// This device's certificate in the replica's roster: the one its key signs
    /// for.
    fn certificate(&self) -> Result<tilo_user_identity::DeviceCert, String> {
        let device = self.0.key.verifying_key().to_bytes();
        self.0
            .replica
            .lock()
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device)
            .ok_or_else(|| "this device is not in the workspace roster".into())
    }
}

/// The control stream of one link: what a device asks the other for, and what
/// it answers with.
struct Control {
    send: SendStream,
    recv: RecvStream,
}

/// Read the workspace a dialling device names first, off its control stream.
async fn dialed_workspace(
    connection: &Connection,
) -> Result<(u64, SendStream, RecvStream), String> {
    let (send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| error.to_string())?;
    let Some(LinkFrame::Workspace(named)) =
        crate::frames::read_link(&mut recv, MAX_FRAME_BYTES).await?
    else {
        return Err(refuse(connection, "the device did not name its workspace"));
    };
    Ok((named, send, recv))
}

fn refuse(connection: &Connection, reason: &str) -> String {
    connection.close(REFUSED.into(), reason.as_bytes());
    reason.to_owned()
}

impl ProtocolHandler for DirectSync {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.serve(connection)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

/// A host's direct links, one per workspace it serves, answering
/// [`DIRECT_ALPN`] on one router.
///
/// The dialling device names its workspace in the link's first frame; a
/// workspace this host holds and does not serve yet is refused, one it does
/// not hold at all is refused the same way.
#[derive(Clone, Default)]
pub struct Serving {
    links: Arc<RwLock<BTreeMap<u64, DirectSync>>>,
}

impl std::fmt::Debug for Serving {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Serving")
            .field("workspaces", &self.links.read_recover().len())
            .finish_non_exhaustive()
    }
}

impl Serving {
    /// A host serving no workspace yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve `link`'s workspace, replacing a link this host served before.
    ///
    /// # Errors
    ///
    /// The link's replica names no workspace id.
    pub fn serve(&self, link: DirectSync) -> Result<(), String> {
        let workspace = link.workspace_number()?;
        self.links.write_recover().insert(workspace, link);
        Ok(())
    }

    /// Stop serving `workspace`: a workspace this host released.
    pub fn release(&self, workspace: u64) {
        self.links.write_recover().remove(&workspace);
    }

    /// The link serving `workspace`, when this host serves it.
    #[must_use]
    pub fn link(&self, workspace: u64) -> Option<DirectSync> {
        self.links.read_recover().get(&workspace).cloned()
    }
}

impl ProtocolHandler for Serving {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let serve = async {
            let (named, send, recv) = dialed_workspace(&connection).await?;
            let link = self
                .link(named)
                .ok_or_else(|| refuse(&connection, "this host holds no such workspace"))?;
            link.link(&connection, false, Control { send, recv }).await
        };
        serve
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

/// A device answering [`DIRECT_ALPN`] for the workspaces its engine holds.
///
/// A device of a workspace that dials this one syncs with the replica held
/// here and catches up from the history this device kept, whether the
/// workspace was founded here, joined, or restored at start: a workspace is
/// served the first time one of its devices asks, so nothing is registered as
/// workspaces arrive. One this device no longer holds is refused, and its link
/// let go.
#[derive(Clone)]
pub struct Owning {
    owner: crate::owner::NativeWorkspaceOwner,
    key: SigningKey,
    store: Arc<Store>,
    serving: Serving,
}

impl std::fmt::Debug for Owning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Owning")
            .field("device", &hex::encode(self.key.verifying_key().as_bytes()))
            .field("serving", &self.serving)
            .finish_non_exhaustive()
    }
}

impl Owning {
    /// Answer for `owner`'s workspaces as the device whose signing key is
    /// `key`, keeping the frames of each at `store`.
    #[must_use]
    pub fn new(
        owner: crate::owner::NativeWorkspaceOwner,
        key: SigningKey,
        store: Arc<Store>,
    ) -> Self {
        Self {
            owner,
            key,
            store,
            serving: Serving::new(),
        }
    }

    /// The link serving `workspace`: this device's replica of it, kept at its
    /// store, made now when this device holds it and nothing serves it yet.
    fn link(&self, workspace: u64) -> Result<DirectSync, String> {
        // The link names the workspace by the transport number its first frame
        // carries: this device answers for the workspace whose own number that
        // is.
        let held = self.owner.workspaces().into_iter().find(|held| {
            crate::join::room_workspace_id(&held.to_string())
                .is_ok_and(|number| number == workspace)
        });
        let Some(held) = held else {
            self.serving.release(workspace);
            return Err("this device holds no such workspace".into());
        };
        if let Some(link) = self.serving.link(workspace) {
            return Ok(link);
        }
        let link = DirectSync::over(self.owner.replica(held)?, self.key.clone())
            .owning(self.owner.clone(), held)?
            .with_store(Arc::clone(&self.store));
        self.serving.serve(link.clone())?;
        Ok(link)
    }
}

impl ProtocolHandler for Owning {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let serve = async {
            let (named, send, recv) = dialed_workspace(&connection).await?;
            let link = match self.link(named) {
                Ok(link) => link,
                Err(reason) => return Err(refuse(&connection, &reason)),
            };
            link.link(&connection, false, Control { send, recv }).await
        };
        serve
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

pub(crate) fn bump(count: &mut u64) {
    *count = count.wrapping_add(1);
}

/// Whether `message` is one `device` sent: what a link does not send back to
/// the device it came from.
pub(crate) fn from_device(message: &NeighbourMsg, device: [u8; 32]) -> bool {
    match message {
        NeighbourMsg::AppDocument { peer, envelope, .. } => {
            envelope.author.as_bytes() == &device || *peer == peer_handle(&device)
        }
        NeighbourMsg::IdentityHello { cert, .. } => cert.device_public_key == device,
        _ => false,
    }
}

/// Write `catch_up`, then every message published live that `remote` did not
/// send, to `send`.
pub(crate) async fn send_all(
    mut send: SendStream,
    remote: [u8; 32],
    catch_up: Vec<NeighbourMsg>,
    mut live: broadcast::Receiver<NeighbourMsg>,
) -> Result<(), String> {
    for message in &catch_up {
        write_frame(&mut send, message).await?;
    }
    loop {
        match live.recv().await {
            Ok(message) if from_device(&message, remote) => {}
            Ok(message) => write_frame(&mut send, &message).await?,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("the link fell behind the device's changes".into());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

pub(crate) async fn write_frame(
    send: &mut SendStream,
    message: &NeighbourMsg,
) -> Result<(), String> {
    let bytes = postcard::to_allocvec(message).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    crate::frames::write_frame(send, &bytes).await
}

/// The next frame, or `None` once the other side finished its stream.
pub(crate) async fn read_frame(recv: &mut RecvStream) -> Result<Option<NeighbourMsg>, String> {
    match crate::frames::read_frame(recv, MAX_FRAME_BYTES).await? {
        Some(bytes) => postcard::from_bytes(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}
