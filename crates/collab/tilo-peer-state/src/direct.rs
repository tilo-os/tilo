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
//! A frame can come ahead of what it builds on, or what it builds on can be
//! lost: a workspace's changes reach a member both as the host forwards
//! another member's frame and as the host publishes its own governance. A
//! device whose replica then holds a change it cannot apply asks the device
//! that sent it, over the same link, for the state that device holds
//! ([`NeighbourMsg::SyncReq`]), and that device answers on that link alone
//! with what a link starts with ([`DirectSync::catch_up`]): its certificate
//! and, from a device of the workspace's owner, the workspace's governance and
//! a checkpoint of every record. What the waiting change builds on comes with
//! it, wherever the change came from.
//!
//! A host of several workspaces answers them all on [`DIRECT_ALPN`] through
//! one [`Serving`], which dispatches each connection to the workspace its
//! first frame names.

use crate::actions::now_ns;
use crate::frames::{MAX_FRAME_BYTES, OUTGOING, REFUSED, bump, from_device};
use crate::join::WorkspaceId;
pub use crate::replica::Replica;
use crate::store::{self, Budget, Cursor, Frame, Store};
use crate::{NeighbourMsg, StatePeer, TransportOrigin};
use collab_types::signer::DeviceSigner;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tokio::sync::{broadcast, watch};
use web_time::SystemTime;

/// ALPN of a direct replica link between two devices of one workspace.
pub const DIRECT_ALPN: &[u8] = b"tilo-peer-state-direct/2";

/// One frame of a direct link's control stream.
///
/// A dialling device names its workspace first, then asks the other device's
/// retained history to continue from its own cursors; the other device
/// answers with the frames its store holds. The workspace's live messages
/// travel on their own streams, one per direction.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum LinkFrame {
    /// The workspace the dialling device wants, named before anything else.
    Workspace(WorkspaceId),
    /// The retained history the dialling device asks for, from its cursors.
    Cursors(Vec<Cursor>),
    /// Retained history after the asker's cursors, and whether more is left.
    Frames {
        /// The frames the other device holds after the asker's cursors, each
        /// with the device that authored it: a round opens with the other
        /// device's own state, which is history too, and a frame is judged
        /// under the device that wrote it, as the live stream's are.
        frames: Vec<([u8; 32], NeighbourMsg)>,
        /// Whether a further ask carries more.
        more: bool,
    },
}

/// Ask the other device's store for the frames after `cursors`, round by round
/// while it says more are left, and answer them, each with its author, once
/// it says no more are.
async fn fetch_store(
    control: &mut Control,
    mut cursors: Vec<Cursor>,
) -> Result<Vec<([u8; 32], NeighbourMsg)>, String> {
    let mut fetched = Vec::new();
    write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone())).await?;
    while let Some(frame) = read_link(&mut control.recv, MAX_FRAME_BYTES).await? {
        let LinkFrame::Frames { frames, more } = frame else {
            return Err("the device answered something else than retained frames".into());
        };
        let messages = frames
            .iter()
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        store::advance(&mut cursors, &messages);
        fetched.extend(frames);
        if !more {
            return Ok(fetched);
        }
        write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone())).await?;
    }
    Err("the device's store ended the history before the last frame".into())
}

/// Write `frame` as the control stream's next record.
async fn write_link<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &LinkFrame,
) -> Result<(), String> {
    let bytes = postcard::to_allocvec(frame).map_err(|error| error.to_string())?;
    crate::frames::write_frame(writer, &bytes).await
}

/// The next control frame, or `None` once the stream ended cleanly before one started.
async fn read_link<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<LinkFrame>, String> {
    match crate::frames::read_frame(reader, max).await? {
        Some(bytes) => postcard::from_bytes(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}

/// A device's replica, shared by the device's engine and its direct links.
///
/// Clone it to hand it to an [`iroh::protocol::Router`] under [`DIRECT_ALPN`]
/// (it is the accepting [`ProtocolHandler`]) and to dial with
/// [`Self::connect`]; every clone is the same replica.
#[derive(Clone)]
pub struct DirectSync(Arc<Inner>);

/// A frame a link brought into this device's engine, as its engine sees it.
#[derive(Clone)]
pub struct Heard {
    /// The device that authored it: what its signed content names, else the
    /// device whose frame the host forwarded.
    pub author: [u8; 32],
    pub message: NeighbourMsg,
    /// Whether the workspace took it in.
    pub landed: bool,
    /// Whether it came from the other device's retained history rather than
    /// live traffic.
    pub retained: bool,
    /// What the workspace's admission decided: the frame is already in, and
    /// this says how.
    pub report: Result<crate::AdmissionReport, String>,
    /// Whether this device had not taken the frame in before, on any carrier:
    /// a sealed frame its store already held is one it carried on already.
    pub fresh: bool,
}

/// A reader of what a device's links bring its engine ([`DirectSync::reader`]).
///
/// The links hand each frame to a channel that keeps the newest ones and drops
/// the oldest for a reader that falls behind. A dropped frame the workspace
/// retains is still in its store, so this reader takes those it missed from
/// there, after where it stands, before it reads on: each once, judged by the
/// workspace again, never one this device wrote itself, and a frame the
/// channel still held after the catch-up handed it is not read twice. What
/// the store never keeps — presence, asks for content and their answers,
/// channel governance — is asked again by whoever wants it.
pub struct Reader {
    link: DirectSync,
    heard: broadcast::Receiver<Heard>,
    /// Where this reader stands in the workspace's retained history.
    cursors: Vec<Cursor>,
    /// The frames the last catch-up handed, whose copy still in the channel
    /// is skipped.
    caught: std::collections::BTreeSet<(crate::store::Lineage, u64)>,
    pending: std::collections::VecDeque<Heard>,
    /// Whether this reader fell behind and has not caught up yet.
    behind: bool,
}

impl Reader {
    /// The next frame the link brought, or `None` once the link is gone.
    /// Cancel-safe: a catch-up cut short is taken again on the next call.
    pub async fn recv(&mut self) -> Option<Heard> {
        loop {
            if self.behind {
                self.catch_up().await;
            }
            if let Some(heard) = self.pending.pop_front() {
                return Some(heard);
            }
            match self.heard.recv().await {
                Ok(heard) => {
                    if let Ok(frame) = Frame::of(&heard.message) {
                        if self.caught.remove(&(frame.lineage, frame.seq)) {
                            continue;
                        }
                        store::advance(&mut self.cursors, std::slice::from_ref(&heard.message));
                    }
                    return Some(heard);
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::info!(
                        missed,
                        "a reader fell behind its link; it takes what it missed from the workspace's history"
                    );
                    self.behind = true;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Take the retained frames after this reader's cursors in, judged by the
    /// workspace as the link judged them when they came.
    async fn catch_up(&mut self) {
        let missed = match self.link.missed(&self.cursors).await {
            Ok(missed) => missed,
            Err(error) => {
                tracing::warn!(%error, "a reader that fell behind its link could not catch up");
                self.behind = false;
                return;
            }
        };
        self.behind = false;
        self.caught.clear();
        for message in missed {
            let Some(author) = store::author_of(&message) else {
                continue;
            };
            let identity = Frame::of(&message).map(|frame| (frame.lineage, frame.seq));
            store::advance(&mut self.cursors, std::slice::from_ref(&message));
            if let Ok(identity) = identity {
                self.caught.insert(identity);
            }
            self.pending
                .push_back(self.link.judge(author, message, true).await);
        }
    }
}

/// A frame on its way to this device's links, with the device that authored
/// it: a workspace's host forwards a member's frame under the member's name,
/// so every member knows whose it is.
#[derive(Clone)]
struct Authored {
    from: [u8; 32],
    message: NeighbourMsg,
    /// Whether this device published the frame itself just now, rather than
    /// carrying it from its replica or forwarding another device's: a device
    /// hosting its own workspace shares its key with the member it serves, so
    /// whose name a frame bears does not say whether the other end has it.
    published: bool,
}

/// What one link carries besides what this device sends every link: its own
/// ask for the other device's state, and its answer to the other's. Several
/// waiting at once go out as one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnlyHere {
    /// A frame here waits for history: ask the other device for its state.
    Ask,
    /// The other device asked for this device's state.
    Answer,
}

/// The streams of a dialled link after the other device admitted this one:
/// what runs it, once its dialer has answered.
pub struct Dialed {
    connection: Connection,
    /// The device dialled: whose the frames are that the other end's history names no author
    /// for.
    device: [u8; 32],
    control: Control,
    /// Subscribed when the dial was answered: what this device sends from
    /// then on is the link's to carry, never lost between the dial and the
    /// link running.
    live: broadcast::Receiver<Authored>,
}

impl Dialed {
    /// Sync with the device on the other end until the link ends.
    ///
    /// # Errors
    ///
    /// As [`DirectSync::connect`]; a link the other device refused ends with
    /// the reason it gave ([`DirectSync::end`]).
    pub async fn run(self, link: &DirectSync) -> Result<(), String> {
        let ran = link
            .link(&self.connection, Some(self.device), self.control, self.live)
            .await;
        refusal(&self.connection).map_or(ran, Err)
    }
}

/// Why the device on the other end of `connection` refused it, when it did.
fn refusal(connection: &Connection) -> Option<String> {
    match connection.close_reason()? {
        iroh::endpoint::ConnectionError::ApplicationClosed(close)
            if close.error_code == REFUSED.into() =>
        {
            Some(String::from_utf8_lossy(&close.reason).into_owned())
        }
        _ => None,
    }
}

#[derive(Clone)]
struct Admission {
    owner: crate::owner::NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
}

/// The workspace's channel governance as a host retains it, each frame with
/// its author.
pub type ChannelHistory = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Vec<([u8; 32], NeighbourMsg)>> + Send>,
        > + Send
        + Sync,
>;

struct Inner {
    replica: Replica,
    /// Where this device's engine owns the workspace: frames are admitted into
    /// its live replica and published as they land.
    admission: Option<Admission>,
    /// This device's custody of its key, which signs what the link writes.
    signer: Arc<dyn DeviceSigner>,
    /// The sealed history this device keeps for the workspace, when it is a
    /// store device.
    store: Option<Arc<Store>>,
    outgoing: broadcast::Sender<Authored>,
    heard: broadcast::Sender<Heard>,
    changed: watch::Sender<u64>,
    /// Set once the other device's retained history is in hand.
    caught_up: watch::Sender<bool>,
    /// Set once this device has written what the other one lacks — its
    /// certificate, the revocations and the workspace's governance — to the
    /// link. The other one decides the retained history it asks for next
    /// against that governance, so the history waits for it.
    catch_up_written: watch::Sender<bool>,
    /// Why the workspace ended for good here, once it did ([`DirectSync::end`]).
    ended: watch::Sender<Option<String>>,
    /// Asks the link this device dialled to read the other device's retained
    /// history again ([`DirectSync::ask_replay`]).
    replay: Arc<tokio::sync::Notify>,
    /// This device is the workspace's host on its links: what they bring in,
    /// it sends on to its other links, so a member's frame reaches every
    /// member through one path.
    fan_out: std::sync::atomic::AtomicBool,
    /// Who else takes what the links bring, when they are an [`Owning`]
    /// device's members' links: the device's session, which hands it to its
    /// apps and channels and on to the workspace's own host.
    hearing: Option<Hearing>,
    /// Handed with the catch-up ([`DirectSync::with_channel_history`]).
    channels: Option<ChannelHistory>,
    /// The geneses of the workspace's channels this host carries.
    geneses: Geneses,
    /// The other device's store, for a device that keeps none: where a reader
    /// that fell behind takes the frames it missed ([`Reader`]).
    other_store: Arc<std::sync::Mutex<Option<OtherStore>>>,
}

/// The geneses of a workspace's channels its host carries, each with its
/// author, by channel: what it answers a member's access request with
/// ([`DirectSync::answer_request`]). Taken from the channel history as a link
/// starts, and from every frame the host sends its links.
type Geneses =
    Arc<std::sync::Mutex<std::collections::HashMap<collab_types::ChannelId, Vec<Authored>>>>;

/// The store on the other end of a device's link.
#[derive(Clone)]
enum OtherStore {
    /// The device it pairs with in this process ([`DirectSync::pair`]).
    Paired(DirectSync),
    /// The device it dialled, asked over the link's control stream.
    Dialled(tokio::sync::mpsc::UnboundedSender<HistoryAsk>),
}

/// An ask for the frames a store holds after some cursors, and where the
/// answer goes.
type HistoryAsk = (
    Vec<Cursor>,
    tokio::sync::oneshot::Sender<Result<Vec<NeighbourMsg>, String>>,
);

/// Where a device takes what its members' links bring it, in the workspace
/// they are of ([`Owning::hear`]): each frame as its link took it in.
pub type MemberSink = Arc<dyn Fn(tilo_desktop_model::WorkspaceId, &Heard) + Send + Sync>;

/// The sinks a link of one workspace hands what it brings to.
#[derive(Clone)]
struct Hearing {
    workspace: tilo_desktop_model::WorkspaceId,
    sinks: Arc<RwLock<Vec<MemberSink>>>,
}

impl std::fmt::Debug for DirectSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectSync")
            .field("device", &hex::encode(self.0.signer.device_public_key()))
            .finish_non_exhaustive()
    }
}

impl DirectSync {
    /// Share `replica`, the replica of the device `signer` is the custody of
    /// (and whose key its iroh endpoint's secret key is).
    #[must_use]
    pub fn new(replica: StatePeer, signer: impl DeviceSigner + 'static) -> Self {
        Self::over(Replica::new(replica), signer)
    }

    /// Sync `replica`, which other parts of the engine read as well: the
    /// action plane authorizes against the state this link admits into.
    #[must_use]
    pub fn over(replica: Replica, signer: impl DeviceSigner + 'static) -> Self {
        Self(Arc::new(Inner {
            replica,
            admission: None,
            signer: Arc::new(signer),
            store: None,
            outgoing: broadcast::channel(OUTGOING).0,
            heard: broadcast::channel(OUTGOING).0,
            changed: watch::channel(0).0,
            caught_up: watch::channel(false).0,
            catch_up_written: watch::channel(false).0,
            ended: watch::channel(None).0,
            replay: Arc::default(),
            fan_out: std::sync::atomic::AtomicBool::new(false),
            hearing: None,
            channels: None,
            geneses: Geneses::default(),
            other_store: Arc::default(),
        }))
    }

    /// This link's channels, device and state, for a link built on it.
    fn copied(&self) -> Inner {
        Inner {
            replica: self.0.replica.clone(),
            admission: self.0.admission.clone(),
            signer: Arc::clone(&self.0.signer),
            store: self.0.store.clone(),
            outgoing: self.0.outgoing.clone(),
            heard: self.0.heard.clone(),
            changed: self.0.changed.clone(),
            caught_up: self.0.caught_up.clone(),
            catch_up_written: self.0.catch_up_written.clone(),
            ended: self.0.ended.clone(),
            replay: Arc::clone(&self.0.replay),
            fan_out: std::sync::atomic::AtomicBool::new(self.fans_out()),
            hearing: self.0.hearing.clone(),
            channels: self.0.channels.clone(),
            geneses: Arc::clone(&self.0.geneses),
            other_store: Arc::clone(&self.0.other_store),
        }
    }

    /// The same link, handing what it brings to `hearing`'s sinks too.
    fn hearing(&self, hearing: Hearing) -> Self {
        Self(Arc::new(Inner {
            hearing: Some(hearing),
            ..self.copied()
        }))
    }

    /// Hand every device that links the channel governance `channels` reads,
    /// with this device's own: the history a link replays keeps sealed records
    /// only. Call it before cloning the link to its handlers.
    #[must_use]
    pub fn with_channel_history(&self, channels: ChannelHistory) -> Self {
        Self(Arc::new(Inner {
            channels: Some(channels),
            ..self.copied()
        }))
    }

    /// Keep this workspace's sealed history at `store` and hand it to the
    /// devices that ask: what a store device does with every link. Call it
    /// before cloning the link to its handlers.
    #[must_use]
    pub fn with_store(&self, store: Arc<Store>) -> Self {
        Self(Arc::new(Inner {
            store: Some(store),
            ..self.copied()
        }))
    }

    /// The same link onto the same replica and store, with room for `capacity`
    /// frames heard and not read yet: what a test that makes a reader fall
    /// behind the link builds, where the link otherwise keeps
    /// [`OUTGOING`] frames.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_heard_capacity(&self, capacity: usize) -> Self {
        Self(Arc::new(Inner {
            heard: broadcast::channel(capacity).0,
            ..self.copied()
        }))
    }

    /// Admit every frame this link brings into `owner`'s live replica of
    /// `workspace` and publish it, instead of into this device's copy alone:
    /// what a link brings is durable as it lands, and the device's own engine
    /// decides it. The link then syncs that replica.
    pub fn owning(
        &self,
        owner: crate::owner::NativeWorkspaceOwner,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> Result<Self, String> {
        let replica = owner.replica(workspace)?;
        Ok(Self(Arc::new(Inner {
            replica,
            admission: Some(Admission { owner, workspace }),
            ..self.copied()
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

    /// Make a local change the caller signed, e.g.
    /// `publication_mut().publish_incremental`, keep the message it returns at
    /// this device's store, send it to every open link, and return it.
    ///
    /// # Errors
    /// `write` refused.
    pub fn publish(
        &self,
        write: impl FnOnce(&mut StatePeer) -> Result<NeighbourMsg, String>,
    ) -> Result<NeighbourMsg, String> {
        let message = write(&mut self.0.replica.lock())?;
        self.0.changed.send_modify(bump);
        self.keep(&message, false);
        self.outgoing(Authored {
            from: self.device(),
            message: message.clone(),
            published: true,
        });
        Ok(message)
    }

    /// Make `change` to the replica in one signed turn of the device's signer:
    /// each run changes a copy of the replica as it stands, and only a run that
    /// asked for no signature the signer had not given yet replaces it. The
    /// signer signs between runs, with the replica unlocked.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the replica stays held until the change replaces the state it was made from"
    )]
    async fn signed_change<T>(
        &self,
        mut change: impl FnMut(&mut StatePeer, &dyn DeviceSigner, SystemTime) -> Result<T, String>,
    ) -> Result<T, String> {
        let now = tilo_clock::wall();
        crate::owner::write::signed_turn(&*self.0.signer, |signed| {
            let mut replica = self.0.replica.lock();
            let mut candidate = replica.clone();
            let changed = change(&mut candidate, signed, now)?;
            signed.settled()?;
            *replica = candidate;
            Ok(changed)
        })
        .await
    }

    /// Ticks whenever the replica changed, by a local change or a frame a
    /// link admitted.
    #[must_use]
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.0.changed.subscribe()
    }

    /// Every frame a link brings this device's engine, in the order the
    /// workspace took them in. A receiver that falls behind loses frames;
    /// [`Self::reader`] does not.
    #[must_use]
    pub fn heard(&self) -> broadcast::Receiver<Heard> {
        self.0.heard.subscribe()
    }

    /// A reader of every frame a link brings this device's engine, from now
    /// on, that misses none of the workspace's retained frames: one that fell
    /// behind takes those it missed from the workspace's store
    /// ([`Reader::recv`]).
    #[must_use]
    pub fn reader(&self) -> Reader {
        Reader {
            heard: self.heard(),
            cursors: self.cursors(),
            link: self.clone(),
            caught: std::collections::BTreeSet::new(),
            pending: std::collections::VecDeque::new(),
            behind: false,
        }
    }

    /// The retained frames after `cursors` a reader of this device's links
    /// missed, each with whether it is the first copy this device took in:
    /// from this device's store, the frames its links brought it; from the
    /// other device's, every frame not this device's own, as the other device
    /// sent them all here.
    async fn missed(&self, cursors: &[Cursor]) -> Result<Vec<NeighbourMsg>, String> {
        if let Some(store) = &self.0.store {
            let workspace = self.workspace()?;
            let mut cursors = cursors.to_vec();
            let mut missed = Vec::new();
            loop {
                let replay = store
                    .replay(workspace, &cursors, Budget::default())
                    .map_err(|error| error.to_string())?;
                store::advance(&mut cursors, &replay.frames);
                missed.extend(replay.frames.into_iter().filter(|message| {
                    Frame::of(message).is_ok_and(|frame| store.taken_from_link(workspace, &frame))
                }));
                if !replay.more {
                    return Ok(missed);
                }
            }
        }
        let other = self.0.other_store.lock_recover().clone();
        let frames = match other {
            None => return Err("neither end of the link keeps the workspace's history".into()),
            Some(OtherStore::Paired(host)) => {
                let mut cursors = cursors.to_vec();
                let mut frames = Vec::new();
                loop {
                    let (round, more) = host.replay(&cursors)?;
                    store::advance(&mut cursors, &round);
                    frames.extend(round);
                    if !more {
                        break frames;
                    }
                }
            }
            Some(OtherStore::Dialled(asks)) => {
                let (answer, answered) = tokio::sync::oneshot::channel();
                asks.send((cursors.to_vec(), answer))
                    .map_err(|_| "the link ended".to_owned())?;
                answered.await.map_err(|_| "the link ended".to_owned())??
            }
        };
        let own = self.device();
        Ok(frames
            .into_iter()
            .filter(|message| store::author_of(message).is_some_and(|author| author != own))
            .collect())
    }

    /// Read the other device's retained history again, from where this
    /// device's replica stands, on the link this device dialled: what a write
    /// let go here unapplied waited for has landed
    /// ([`crate::ReceiveOutcome::replay_wanted`]), and the other device
    /// still keeps the write. Asked while the link reads the history
    /// already, it reads it once more after.
    pub fn ask_replay(&self) {
        self.0.replay.notify_one();
    }

    /// Whether the other device's retained history is in hand yet: true once
    /// this device has asked its store (or, in process, replayed it).
    #[must_use]
    pub fn caught_up(&self) -> watch::Receiver<bool> {
        self.0.caught_up.subscribe()
    }

    /// Send `message` to every open link, as this device's live traffic: what
    /// the workspace keeps of it does not come from here.
    ///
    /// # Errors
    ///
    /// `message` is not this device's to send in the workspace (see
    /// [`Self::publish`]).
    pub fn send(&self, message: NeighbourMsg) -> Result<(), String> {
        self.keep(&message, false);
        self.outgoing(Authored {
            from: self.device(),
            message,
            published: true,
        });
        Ok(())
    }

    /// Carry `message`, `from`'s frame the workspace admitted on another
    /// carrier (a relay session), to every device on this link, once: kept at
    /// this device's store, the history a device that joins later replays, and
    /// sent live to the devices linked now. A sealed frame the store already
    /// holds went out when it first arrived, and is not sent again. Returns
    /// whether it went out.
    pub fn carry(&self, from: [u8; 32], message: &NeighbourMsg) -> bool {
        if !self.keep(message, false) {
            return false;
        }
        self.outgoing(Authored {
            from,
            message: message.clone(),
            published: false,
        });
        true
    }

    /// Hand `authored` to every open link; no open link is no error, as the
    /// next one catches up from the replica.
    fn outgoing(&self, authored: Authored) {
        if self.fans_out() {
            self.note_genesis(&authored);
        }
        let _ = self.0.outgoing.send(authored);
    }

    /// Note `authored` when it is a channel's genesis this host carries.
    fn note_genesis(&self, authored: &Authored) {
        let NeighbourMsg::MlsGovernance { frame, .. } = &authored.message else {
            return;
        };
        if !matches!(frame.action, collab_types::MlsGovernanceAction::Genesis) {
            return;
        }
        let mut geneses = self.0.geneses.lock_recover();
        let known = geneses.entry(frame.channel).or_default();
        if !known.iter().any(|kept| kept.message == authored.message) {
            known.push(authored.clone());
        }
        drop(geneses);
    }

    /// Answer `message` when it is a member's access request: with the
    /// geneses of its channel this host carries, so the device asks to join
    /// the group rather than create one, or with word that it carries none
    /// ([`NeighbourMsg::MlsChannelUnfounded`]), so the device creates the
    /// group at once instead of waiting to hear of one. A genesis made beside
    /// the one the answer names is resolved by the channel's members.
    fn answer_request(&self, message: &NeighbourMsg) {
        let (NeighbourMsg::MlsAccessRequest {
            workspace, request, ..
        }
        | NeighbourMsg::MlsRoutedAccessRequest {
            room: workspace,
            request,
            ..
        }) = message
        else {
            return;
        };
        let known = self.0.geneses.lock_recover().get(&request.channel).cloned();
        let answers = known.unwrap_or_else(|| {
            vec![Authored {
                from: self.device(),
                message: NeighbourMsg::MlsChannelUnfounded {
                    workspace: workspace.clone(),
                    channel: request.channel,
                    device: request.device,
                },
                published: true,
            }]
        });
        for answer in answers {
            let _ = self.0.outgoing.send(answer);
        }
    }

    /// Whether this device is the workspace's host on its links
    /// ([`Serving::serve`]).
    fn fans_out(&self) -> bool {
        self.0.fan_out.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Dial `device` at `addr` from `endpoint` and answer its admission,
    /// leaving the link ready to run ([`Dialed::run`]). `addr` is the device's
    /// own key, or the service address of a relay its signed host hint binds
    /// to it: a relay answers on a key of its own, and what its history names
    /// no author for is still its device's.
    ///
    /// # Errors
    ///
    /// The connection fails, the other device refuses this one, or this
    /// device is not in the workspace's roster.
    pub async fn dial(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
        device: [u8; 32],
    ) -> Result<Dialed, String> {
        let connection = endpoint
            .connect(addr, DIRECT_ALPN)
            .await
            .map_err(|error| error.to_string())?;
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| error.to_string())?;
        let workspace = self.workspace()?;
        write_link(&mut send, &LinkFrame::Workspace(workspace)).await?;
        // Who the dialled device is, this one learns from what it sends: its
        // certificate is in the frames it opens with, and a device that has
        // not synced yet holds no roster to refuse by.
        self.certificate()?;
        // Subscribed before the dial returns: the caller may send at once.
        let live = self.0.outgoing.subscribe();
        Ok(Dialed {
            connection,
            device,
            control: Control { send, recv },
            live,
        })
    }

    /// Sync with the device at `addr` until the link ends.
    ///
    /// # Errors
    ///
    /// As [`Self::dial`].
    pub async fn connect(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
    ) -> Result<(), String> {
        let addr = addr.into();
        let device = *addr.id.as_bytes();
        let dialed = self.dial(endpoint, addr, device).await?;
        dialed.run(self).await
    }

    /// Sync `member` with `host`, two devices of one workspace in one process:
    /// what a link carries, with no network in between — each side's
    /// certificate and checkpoint, then the retained history `member` lacks
    /// from `host`'s store, then every frame either publishes while both
    /// live. Runs until the process ends.
    ///
    /// # Errors
    ///
    /// Either device is not in the workspace's roster.
    pub async fn pair(member: &Self, host: &Self) -> Result<(), String> {
        let (own, theirs) = (member.device(), host.device());
        host.admits(own)?;
        member.admits(theirs)?;
        let live_mine = member.0.outgoing.subscribe();
        let live_theirs = host.0.outgoing.subscribe();
        // Each side's catch-up is history, however old what it carries: it is
        // judged as retained, never as a late live frame.
        for authored in host.catch_up().await? {
            member.admit(authored.from, authored.message, true).await;
        }
        for authored in member.catch_up().await? {
            host.admit(authored.from, authored.message, true).await;
        }
        member.replay_from(host).await?;
        if member.0.store.is_none() {
            *member.0.other_store.lock_recover() = Some(OtherStore::Paired(host.clone()));
        }
        member.0.caught_up.send_replace(true);
        host.0.caught_up.send_replace(true);
        let to_host = forward(live_mine, member, host, false);
        let to_member = forward(live_theirs, host, member, false);
        tokio::try_join!(to_host, to_member)?;
        Ok(())
    }

    /// Take in every frame `host` retains that this device's cursors do not
    /// cover, round by round.
    async fn replay_from(&self, host: &Self) -> Result<(), String> {
        let mut cursors = self.cursors();
        loop {
            let (frames, more) = host.replay(&cursors)?;
            for message in &frames {
                let author = store::author_of(message)
                    .ok_or("the store replayed a message that is not a document frame")?;
                if self.admit(author, message.clone(), true).await {
                    for state in host.answer_state_request().await {
                        self.admit(state.from, state.message, true).await;
                    }
                }
            }
            store::advance(&mut cursors, &frames);
            if !more {
                return Ok(());
            }
        }
    }

    /// The frames `cursors` do not cover at this device's store, and whether
    /// more are left.
    fn replay(&self, cursors: &[Cursor]) -> Result<(Vec<NeighbourMsg>, bool), String> {
        let Some(store) = &self.0.store else {
            return Ok((Vec::new(), false));
        };
        let replay = store
            .replay(self.workspace()?, cursors, Budget::default())
            .map_err(|error| error.to_string())?;
        Ok((replay.frames, replay.more))
    }

    /// The device key this link signs for.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.signer.device_public_key()
    }

    /// Serve the connection a device dialled this one: its first frame names
    /// the workspace.
    ///
    /// # Errors
    ///
    /// The device named no workspace, or one this replica is not.
    pub async fn serve(&self, connection: Connection) -> Result<(), String> {
        let (named, send, recv) = dialed_workspace(&connection).await?;
        if named != self.workspace()? {
            return Err(refuse(&connection, "this device holds no such workspace"));
        }
        let live = self.0.outgoing.subscribe();
        self.link(&connection, None, Control { send, recv }, live)
            .await
    }

    /// Sync with the device on the other end of `connection` until the link
    /// ends, whichever side dialled: `dialled` names the device this side
    /// dialled, and the side that accepted reads it off the connection.
    async fn link(
        &self,
        connection: &Connection,
        dialled: Option<[u8; 32]>,
        control: Control,
        live: broadcast::Receiver<Authored>,
    ) -> Result<(), String> {
        let dialing = dialled.is_some();
        let remote = dialled.unwrap_or_else(|| *connection.remote_id().as_bytes());
        // The device that accepted a dial refuses one of the workspace's
        // roster it does not know before a frame is read; the one that
        // dialled cannot judge, and takes only what its roster verifies.
        if !dialing && let Err(reason) = self.admits(remote) {
            return Err(refuse(connection, &reason));
        }
        // Taken after the catch-up, which the caller subscribed before: a
        // change published in between reaches the link after the history
        // rather than not at all.
        // The device that accepted the dial hands its replica's catch-up with
        // the history the other one asks for ([`Self::answer_store`]), where it
        // is judged as history: on the live stream, what it carries that was
        // written more than the live window ago was refused as a late frame.
        let mut catch_up = if dialing {
            self.catch_up().await?
        } else {
            Vec::new()
        };
        if let Some(channels) = &self.0.channels {
            let history: Vec<Authored> = channels()
                .await
                .into_iter()
                .map(|(from, message)| Authored {
                    from,
                    message,
                    published: false,
                })
                .collect();
            for authored in &history {
                self.note_genesis(authored);
            }
            catch_up.extend(history);
        }
        let send = connection
            .open_uni()
            .await
            .map_err(|error| error.to_string())?;
        // What this link alone carries, besides what every link does: this
        // device's asks for the other's state, and its answers to the other's.
        let (own, only_here) = tokio::sync::mpsc::unbounded_channel();
        // The accepting device's live frames follow the first history it
        // answers, which carries the governance they are judged against.
        let (answered, first_answer) = watch::channel(dialing);
        let sending = send_authored(
            self,
            send,
            Sending {
                remote,
                dialing,
                catch_up,
                first_answer,
            },
            live,
            only_here,
        );
        let receiving = self.receive_all(connection, remote, &own);
        let history = async {
            let mut control = control;
            if !dialing {
                // The other device asks for the history as the link starts,
                // and again whenever it wants what it let go.
                self.answer_store(&mut control, &answered).await?;
                self.0.caught_up.send_replace(true);
                return std::future::pending().await;
            }
            self.ask_store(&mut control).await?;
            // The history is in hand: the frames from here on are the live
            // ones the link brings. A write this device let go unapplied
            // waits in the other device's history; once what it waited for
            // lands here, the history is read again from where the replica
            // stands. A reader of this link that fell behind it asks the same
            // history for the frames it missed.
            self.0.caught_up.send_replace(true);
            let (asks, mut asked) = tokio::sync::mpsc::unbounded_channel();
            *self.0.other_store.lock_recover() = Some(OtherStore::Dialled(asks));
            loop {
                tokio::select! {
                    () = self.0.replay.notified() => self.ask_store(&mut control).await?,
                    Some((cursors, answer)) = asked.recv() => {
                        let fetched = Self::fetch_history(&mut control, cursors).await;
                        let failed = fetched.as_ref().err().cloned();
                        let _ = answer.send(fetched);
                        if let Some(error) = failed {
                            return Err(error);
                        }
                    }
                }
            }
        };
        let ended = async {
            let mut ended = self.0.ended.subscribe();
            ended.wait_for(Option::is_some).await.ok()?.clone()
        };
        tokio::select! {
            result = receiving => result,
            result = sending => result,
            result = history => result,
            Some(reason) = ended => Err(refuse(connection, &reason)),
        }
    }

    /// The workspace ended for good on this device, for `reason`: every link of
    /// it closes, refused with that reason, which the device on the other end
    /// reads ([`Dialed::run`]).
    pub fn end(&self, reason: &str) {
        self.0.ended.send_replace(Some(reason.to_owned()));
    }

    /// Ask the other device for the history this device lacks, round by round
    /// while it says more are left, and return once it says no more are: the
    /// history is in hand. Each ask's first round opens with the other
    /// device's state, its catch-up ([`Self::catch_up`]), and the frames its
    /// store keeps follow; each frame is judged under the device the round
    /// names as its author. A frame still waiting for history after that waits
    /// for what the other device lacks too: asking again would bring nothing.
    async fn ask_store(&self, control: &mut Control) -> Result<(), String> {
        for (author, message) in fetch_store(control, self.cursors()).await? {
            self.admit(author, message, true).await;
        }
        Ok(())
    }

    /// The frames the other device's store holds after `cursors`, read over
    /// the link's control stream for a reader of this device's links that
    /// fell behind them.
    async fn fetch_history(
        control: &mut Control,
        cursors: Vec<Cursor>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let fetched = fetch_store(control, cursors).await?;
        Ok(fetched.into_iter().map(|(_, message)| message).collect())
    }

    /// Answer the dialling device's cursors from this device's store, for as
    /// long as it asks: the history is in its hands once the first answer
    /// with no more left is sent, and it asks again for what it let go.
    ///
    /// Each ask's first round opens with this device's state, its catch-up
    /// ([`Self::catch_up`]): history, which the other device judges as such
    /// however old what it carries. `answered` is set once the first round is
    /// written.
    async fn answer_store(
        &self,
        control: &mut Control,
        answered: &watch::Sender<bool>,
    ) -> Result<(), String> {
        // The history this answers is decided by the other device against the
        // governance it has not been sent yet: it arrives with the catch-up,
        // and the frames asked for before it are the ones it cannot admit.
        // The answer waits for the catch-up to be written, so a device that
        // joins takes the workspace's governance in before its records.
        let mut written = self.0.catch_up_written.subscribe();
        while !*written.borrow_and_update() {
            if written.changed().await.is_err() {
                return Ok(());
            }
        }
        let mut opens_an_ask = true;
        while let Some(frame) = read_link(&mut control.recv, MAX_FRAME_BYTES).await? {
            let LinkFrame::Cursors(cursors) = frame else {
                return Err("the device asked for something else than retained frames".into());
            };
            let (kept, more) = self.replay(&cursors)?;
            let mut frames = Vec::new();
            if opens_an_ask {
                frames.extend(
                    self.answer_state_request()
                        .await
                        .into_iter()
                        .map(|state| (state.from, state.message)),
                );
            }
            // A frame the store keeps names its own author; one that does not
            // is this device's.
            let own = self.device();
            frames.extend(
                kept.into_iter()
                    .map(|message| (store::author_of(&message).unwrap_or(own), message)),
            );
            write_link(&mut control.send, &LinkFrame::Frames { frames, more }).await?;
            answered.send_replace(true);
            opens_an_ask = !more;
            if !more {
                self.0.caught_up.send_replace(true);
            }
        }
        Ok(())
    }

    /// What this device's replica holds, as the cursors it asks a store to
    /// continue from.
    #[must_use]
    pub fn cursors(&self) -> Vec<Cursor> {
        self.read(store::cursors)
    }

    /// Whether the workspace removed `device`'s person: it reads nothing the
    /// workspace says from then on, and what it sends is no longer taken.
    fn removed(&self, device: [u8; 32]) -> bool {
        self.0.replica.lock().governance().removed_device(device)
    }

    /// Whether `device` may sync with this replica: a certified device of
    /// its roster, unrevoked and unexpired, of a person the workspace has
    /// not removed.
    fn admits(&self, device: [u8; 32]) -> Result<(), String> {
        if self.removed(device) {
            return Err(REMOVED.into());
        }
        let replica = self.0.replica.lock();
        let certificate = replica
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device);
        // A revocation reaches the keys its own root certified: another
        // person's entry naming this key refuses nothing of its device's.
        let revoked = certificate.as_ref().is_some_and(|certificate| {
            replica.revocations().iter().any(|entry| {
                (entry.user_public_key, entry.revoked_device_public_key)
                    == (certificate.user_public_key, device)
            })
        });
        drop(replica);
        let certificate = certificate.ok_or("the device is not in the workspace roster")?;
        if revoked {
            return Err("the device is revoked".into());
        }
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
    async fn catch_up(&self) -> Result<Vec<Authored>, String> {
        let device = self.device();
        let (hello, bootstraps) = {
            let replica = self.0.replica.lock();
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
            let hello = NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(cert),
                revocations,
            };
            (
                hello,
                replica.has_governance() && replica.signed_by_owner(&device),
            )
        };
        let mut messages = vec![hello];
        // The snapshots take numbers of this device's own sequence: where its engine owns the
        // workspace they are its commit, made through the owner, so its next write numbers after
        // them.
        match &self.0.admission {
            _ if !bootstraps => {}
            Some(admission) => messages.extend(
                admission
                    .owner
                    .commit_bootstrap(admission.workspace, &*self.0.signer, tilo_clock::wall())
                    .await?,
            ),
            None => messages.extend(
                self.signed_change(|peer, signer, now| {
                    peer.publication_mut().bootstrap_messages(signer, now)
                })
                .await?,
            ),
        }
        Ok(messages
            .into_iter()
            .map(|message| Authored {
                from: device,
                message,
                published: false,
            })
            .collect())
    }

    /// Admit every frame the other device sends until its stream ends. A
    /// frame the replica refuses is that message's verdict, not the link's: the link
    /// stays up for the frames after it, and the device files what it refused
    /// as its own verdict in the workspace document, so the write's author
    /// reads why it is not here.
    ///
    /// A frame that leaves something waiting for history asks `remote` for
    /// its state, and `remote`'s ask for this device's is answered, each on
    /// `link`, this link alone: no other device sent the frame, or asked.
    async fn receive_all(
        &self,
        connection: &Connection,
        remote: [u8; 32],
        link: &tokio::sync::mpsc::UnboundedSender<OnlyHere>,
    ) -> Result<(), String> {
        let mut recv = connection
            .accept_uni()
            .await
            .map_err(|error| error.to_string())?;
        while let Some(authored) = read_authored(&mut recv).await? {
            // An ask another device made reached this one only because a
            // host forwarded it, and is not this device's to answer.
            if matches!(authored.message, NeighbourMsg::SyncReq { .. }) {
                if authored.from == remote {
                    let _ = link.send(OnlyHere::Answer);
                }
                continue;
            }
            if self.admit(authored.from, authored.message, false).await {
                let _ = link.send(OnlyHere::Ask);
            }
        }
        Ok(())
    }

    /// Take `message` in as `claimed`'s frame, admitted into this device's
    /// workspace, kept at its store and handed to its engine. What the frame
    /// itself signs names its author over what the link says.
    ///
    /// Returns whether the frame left something waiting for history this
    /// device does not hold ([`crate::ReceiveOutcome::lacks_history`]): the
    /// device that sent it is asked for its state. What the workspace
    /// refuses of a frame is that frame's verdict, never the link's.
    async fn admit(&self, claimed: [u8; 32], message: NeighbourMsg, retained: bool) -> bool {
        let author = store::author_of(&message).unwrap_or(claimed);
        if channel_governance(&message) {
            if self.fans_out() && !retained {
                self.answer_request(&message);
            }
            self.carry_governance(author, message, retained);
            return false;
        }
        let mut heard = self.judge(author, message, retained).await;
        let lacks_history = heard
            .report
            .as_ref()
            .is_ok_and(|report| report.lacks_history);
        heard.fresh = self.keep(&heard.message, true);
        if heard.landed {
            self.0.changed.send_modify(bump);
        }
        // What a write let go here waited for came with this frame: the
        // write is still in the other device's history, read again for it.
        if heard
            .report
            .as_ref()
            .is_ok_and(|report| report.replay_wanted)
        {
            self.ask_replay();
        }
        // The workspace's host is its members' one path: what one member's
        // link brings reaches every other, under the member's own name, once:
        // a frame its store already held went out when it first arrived.
        if self.fans_out() && heard.report.is_ok() && heard.fresh {
            self.outgoing(Authored {
                from: heard.author,
                message: heard.message.clone(),
                published: false,
            });
        }
        self.tell(heard);
        lacks_history
    }

    /// What this device sends the other end of a link to ask for the state
    /// it holds: a frame here waits for history this device lacks.
    fn state_request(&self) -> Authored {
        Authored {
            from: self.device(),
            message: NeighbourMsg::SyncReq {
                since: Vec::new(),
                room: self.0.replica.workspace(),
                invite_id: None,
                nonce: now_ns(),
            },
            published: true,
        }
    }

    /// Answer the other end of a link's ask for the state this device holds:
    /// what the link started with, again ([`Self::catch_up`]).
    async fn answer_state_request(&self) -> Vec<Authored> {
        self.catch_up().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "a device's ask for this device's state went unanswered");
            Vec::new()
        })
    }

    /// Admit `message`, `author`'s frame, into this device's workspace: what
    /// the workspace decided of it, as its engine hears it.
    async fn judge(&self, author: [u8; 32], message: NeighbourMsg, retained: bool) -> Heard {
        match &self.0.admission {
            Some(admission) => self.admit_owned(admission, author, message, retained).await,
            None => self.admit_local(author, message, retained).await,
        }
    }

    /// Hand `heard` to this device's engine, and to the sinks an [`Owning`]
    /// device's session hears its members by.
    fn tell(&self, heard: Heard) {
        if let Some(hearing) = &self.0.hearing {
            for sink in hearing.sinks.read_recover().iter() {
                sink(hearing.workspace, &heard);
            }
        }
        let _ = self.0.heard.send(heard);
    }

    /// Carry `message`, `author`'s channel governance, without admitting it:
    /// a channel's group is settled between its members, never in the
    /// workspace's state, so no admission decides it. The workspace's host
    /// sends it on to its other links, as it does every member's frame, and
    /// this device's own channel service takes it from [`Self::heard`]. A host
    /// that dropped it left each member founding the channel's group alone,
    /// sealing what it wrote under keys no other member held.
    fn carry_governance(&self, author: [u8; 32], message: NeighbourMsg, retained: bool) {
        if self.fans_out() {
            self.outgoing(Authored {
                from: author,
                message: message.clone(),
                published: false,
            });
        }
        self.tell(Heard {
            author,
            message,
            landed: false,
            retained,
            report: Err(NOT_WORKSPACE_STATE.into()),
            fresh: true,
        });
    }

    /// Take `message` into the workspace's own live replica, which the
    /// device's engine decides on: durable as it lands.
    async fn admit_owned(
        &self,
        admission: &Admission,
        author: [u8; 32],
        message: NeighbourMsg,
        retained: bool,
    ) -> Heard {
        let origin = if retained {
            TransportOrigin::RetainedRelayDelta(author)
        } else {
            TransportOrigin::RelayDelta(author)
        };
        let now = tilo_clock::wall();
        let report = admission
            .owner
            .receive(admission.workspace, origin, message.clone(), now);
        // A refused write reaches its author: this device keeps its verdict on it in its own
        // slice of the workspace, which the next sync carries back.
        if let Err(error) = admission
            .owner
            .keep_verdicts(
                admission.workspace,
                &[(&message, &report)],
                &*self.0.signer,
                now,
            )
            .await
        {
            tracing::warn!(%error, "a document write's verdict was not kept");
        }
        let (landed, report) = match report {
            Ok(report) => (report.changed || report.metadata_changed, Ok(report)),
            Err(reason) => {
                tracing::warn!(%reason, "a direct link carried a frame the workspace refused");
                (false, Err(reason))
            }
        };
        Heard {
            author,
            message,
            landed,
            retained,
            report,
            fresh: true,
        }
    }

    /// Take `message` into this device's own replica: whether it is the
    /// workspace's governor decides there too, and what it refuses is filed
    /// where the write's author reads why.
    async fn admit_local(&self, author: [u8; 32], message: NeighbourMsg, retained: bool) -> Heard {
        let origin = if retained {
            TransportOrigin::RetainedRelayDelta(author)
        } else {
            TransportOrigin::RelayDelta(author)
        };
        // The certificate the verdicts below are filed under is this device's
        // own, read before the replica is locked: a replica locked already is
        // not to be read again. A device not in the roster files none.
        let own = matches!(message, NeighbourMsg::AppDocument { .. })
            .then(|| self.certificate().ok())
            .flatten();
        let received = self
            .0
            .replica
            .lock()
            .receive_from_report(origin, message.clone(), tilo_clock::wall())
            .map_err(|reason| reason.to_string());
        let mut landed = received
            .as_ref()
            .is_ok_and(|report| report.changed || report.metadata_changed);
        // The verdict is this device's own, filed under its own certificate:
        // a document write the workspace refuses is filed where its author
        // reads why it is not here, and a landed one clears the verdicts it
        // lands on. The carrier's certificate says who sent the frame, not
        // whose verdicts these are. Any other frame is the link's alone to
        // drop.
        if let Some(own) = own.filter(|_| received.is_err() || landed) {
            let filed = self
                .signed_change(|peer, signer, now| match &received {
                    Ok(_) => peer
                        .workspace_mut()
                        .clear_landed_write(&own, &message, signer, now),
                    Err(reason) => peer
                        .workspace_mut()
                        .refuse_write(&own, &message, reason, signer, now),
                })
                .await;
            match filed {
                Ok(filed) => landed |= filed,
                Err(error) => tracing::warn!(%error, "a write's verdict was not kept"),
            }
        }
        Heard {
            author,
            message,
            landed,
            retained,
            report: received,
            fresh: true,
        }
    }

    /// Keep `message` at this device's store when it is a sealed document
    /// frame, the only history a store holds, as one a link brought when
    /// `from_link`. Returns `false` only for a frame
    /// the store already held: one this device took in before, on any carrier.
    fn keep(&self, message: &NeighbourMsg, from_link: bool) -> bool {
        let Some(store) = &self.0.store else {
            return true;
        };
        let Ok(workspace) = self.0.replica.id() else {
            return true;
        };
        let Ok(frame) = Frame::of(message) else {
            return true;
        };
        let retained = if from_link {
            store.retain_from_link(workspace, &frame)
        } else {
            store.retain(workspace, &frame)
        };
        match retained {
            Ok(retained) => retained != crate::store::Retained::Duplicate,
            Err(error) => {
                tracing::warn!(%error, "a frame was not kept at the store");
                true
            }
        }
    }

    fn workspace(&self) -> Result<WorkspaceId, String> {
        self.0.replica.id()
    }

    /// This device's certificate in the replica's roster: the one its key signs
    /// for.
    fn certificate(&self) -> Result<tilo_user_identity::DeviceCert, String> {
        let device = self.device();
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
) -> Result<(WorkspaceId, SendStream, RecvStream), String> {
    let (send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| error.to_string())?;
    let Some(LinkFrame::Workspace(named)) = read_link(&mut recv, MAX_FRAME_BYTES).await? else {
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
        // A connection this device accepts is a member's, so this device is
        // the workspace's host on it.
        self.0
            .fan_out
            .store(true, std::sync::atomic::Ordering::Release);
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
/// not hold at all is refused the same way, and one its owner deleted is
/// refused as deleted.
#[derive(Clone, Default)]
pub struct Serving {
    links: Arc<RwLock<BTreeMap<WorkspaceId, DirectSync>>>,
    /// The workspaces deleted here, refused as deleted to whoever names one
    /// ([`Self::delete`]).
    deleted: Arc<RwLock<std::collections::BTreeSet<WorkspaceId>>>,
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
        let workspace = link.workspace()?;
        link.0
            .fan_out
            .store(true, std::sync::atomic::Ordering::Release);
        self.links.write_recover().insert(workspace, link);
        Ok(())
    }

    /// Stop serving `workspace`: a workspace this host released.
    pub fn release(&self, workspace: WorkspaceId) {
        self.links.write_recover().remove(&workspace);
    }

    /// `workspace` was deleted here, for good: every link of it ends and a
    /// device that names it from now on is refused, each told
    /// [`WORKSPACE_DELETED`](tilo_relay_protocol::refusal::WORKSPACE_DELETED).
    /// A device that knows the workspace's random id learns nothing more than
    /// that it is gone.
    pub fn delete(&self, workspace: WorkspaceId) {
        self.deleted.write_recover().insert(workspace);
        let link = self.links.write_recover().remove(&workspace);
        if let Some(link) = link {
            link.end(tilo_relay_protocol::refusal::WORKSPACE_DELETED);
        }
    }

    /// Whether `workspace` was deleted here ([`Self::delete`]).
    #[must_use]
    pub fn deleted(&self, workspace: WorkspaceId) -> bool {
        self.deleted.read_recover().contains(&workspace)
    }

    /// The link serving `workspace`, when this host serves it.
    #[must_use]
    pub fn link(&self, workspace: WorkspaceId) -> Option<DirectSync> {
        self.links.read_recover().get(&workspace).cloned()
    }
}

impl ProtocolHandler for Serving {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let serve = async {
            let (named, send, recv) = dialed_workspace(&connection).await?;
            if self.deleted(named) {
                return Err(refuse(
                    &connection,
                    tilo_relay_protocol::refusal::WORKSPACE_DELETED,
                ));
            }
            let link = self
                .link(named)
                .ok_or_else(|| refuse(&connection, "this host holds no such workspace"))?;
            let live = link.0.outgoing.subscribe();
            link.link(&connection, None, Control { send, recv }, live)
                .await
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
    signer: Arc<dyn DeviceSigner>,
    store: Arc<Store>,
    serving: Serving,
    /// Who takes what the members' links bring ([`Self::hear`]).
    sinks: Arc<RwLock<Vec<MemberSink>>>,
    /// Hands what this device commits to a workspace it hosts itself to the
    /// members linked here, for as long as this is kept.
    _kept_here: Arc<crate::owner::NativeKeptHereSubscription>,
}

impl std::fmt::Debug for Owning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Owning")
            .field("device", &hex::encode(self.signer.device_public_key()))
            .field("serving", &self.serving)
            .finish_non_exhaustive()
    }
}

impl Owning {
    /// Answer for `owner`'s workspaces as the device `signer` is the custody
    /// of, keeping the frames of each at `store`.
    #[must_use]
    pub fn new(
        owner: crate::owner::NativeWorkspaceOwner,
        signer: impl DeviceSigner + 'static,
        store: Arc<Store>,
    ) -> Self {
        let signer: Arc<dyn DeviceSigner> = Arc::new(signer);
        let serving = Serving::new();
        // What this device commits to a workspace it hosts itself reaches the
        // members linked to it now, as this device's frame; the workspace keeps
        // no custody for it and holds no link to a host. Nothing is kept for a
        // member linked later: the workspace's own state is what its link's
        // catch-up hands it.
        let device = signer.device_public_key();
        let linked = serving.clone();
        let kept_here = owner.observe_kept_here(Arc::new(
            move |workspace: tilo_desktop_model::WorkspaceId, message: &NeighbourMsg| {
                send_to_members(&linked, workspace, device, message.clone());
            },
        ));
        Self {
            owner,
            signer,
            store,
            serving,
            sinks: Arc::default(),
            _kept_here: Arc::new(kept_here),
        }
    }

    /// Hand `sink` every frame the members linked to this device bring it, in
    /// any workspace it serves, as their link took it in: the device's own
    /// apps and channels read the workspace as the members write it, and the
    /// devices that take part through the workspace's own host hear it only
    /// when this device sends it on there. The links already send it on to
    /// the device's other members.
    pub fn hear(&self, sink: MemberSink) {
        self.sinks.write_recover().push(sink);
    }

    /// The owner whose workspaces this answers for.
    #[must_use]
    pub fn owner(&self) -> &crate::owner::NativeWorkspaceOwner {
        &self.owner
    }

    /// Send `message`, `from`'s frame of `workspace`, to the members linked to
    /// this device there: what this device writes, and what the workspace's
    /// other devices wrote that reached it another way. A member linked here
    /// has no other path to the rest of the workspace. A sealed write is kept
    /// at this device's store as well, so a member that links later catches
    /// up on it; nothing else goes out for a workspace no member reached here.
    pub fn carry(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        from: [u8; 32],
        message: NeighbourMsg,
    ) {
        carry_to(&self.store, &self.serving, workspace, from, message);
    }

    /// Send `message`, channel governance of a workspace this device serves,
    /// to the members linked to it there: what this device's channel service
    /// says, and what the workspace's other devices said that reached it
    /// another way. A member hears no other path to the rest of the
    /// workspace. Nothing goes out for a workspace no member reached here.
    pub fn carry_governance(&self, message: &NeighbourMsg) {
        if let Some(workspace) = message
            .channel_workspace()
            .and_then(tilo_desktop_model::WorkspaceId::parse)
        {
            self.carry(workspace, self.signer.device_public_key(), message.clone());
        }
    }

    /// The link serving `workspace`: this device's replica of it, kept at its
    /// store, made now when this device holds it and nothing serves it yet.
    fn link(&self, workspace: WorkspaceId) -> Result<DirectSync, String> {
        let held = self
            .owner
            .workspaces()
            .into_iter()
            .find(|held| *held == workspace);
        let Some(held) = held else {
            self.serving.release(workspace);
            return Err("this device holds no such workspace".into());
        };
        if let Some(link) = self.serving.link(workspace) {
            return Ok(link);
        }
        let link = DirectSync::over(self.owner.replica(held)?, Arc::clone(&self.signer))
            .hearing(Hearing {
                workspace: held,
                sinks: Arc::clone(&self.sinks),
            })
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
            let live = link.0.outgoing.subscribe();
            link.link(&connection, None, Control { send, recv }, live)
                .await
        };
        serve
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

/// Take every frame `live`, the links of `sender`, publishes into
/// `device`'s workspace, in its author's name: what `device` wrote itself
/// does not come back to it. A frame that leaves `device` waiting for
/// history takes in what `sender` holds, as a link asks for it.
async fn forward(
    mut live: broadcast::Receiver<Authored>,
    sender: &DirectSync,
    device: &DirectSync,
    retained: bool,
) -> Result<(), String> {
    loop {
        match live.recv().await {
            Ok(authored) if already_theirs(&authored, device.device(), sender.device()) => {}
            Ok(authored) => {
                if device
                    .admit(authored.from, authored.message, retained)
                    .await
                {
                    for state in sender.answer_state_request().await {
                        // What still waits after the answer waits for history
                        // the sender lacks too: asking again brings nothing.
                        device.admit(state.from, state.message, true).await;
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("the link fell behind the device's changes".into());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

/// Why a link to a device ends, or is refused: the workspace removed its
/// person.
const REMOVED: &str = "the workspace removed the device's person";

/// Why a channel's governance is never admitted into the workspace: what a
/// [`Heard`] carrying it reports.
const NOT_WORKSPACE_STATE: &str =
    "channel governance is settled by the channel's members, not admitted into the workspace";

/// Whether `message` is a channel's governance: a frame of its group, or a
/// device asking to join it.
fn channel_governance(message: &NeighbourMsg) -> bool {
    message.channel_workspace().is_some()
}

/// Whether `authored` is the other end's already: the device `remote` sent it
/// itself, and is another device than the one `own` this link signs for. A
/// device hosting a workspace it placed itself serves the member it placed
/// under the same signing key, and what that member published is what only
/// that member's replica lacks, so whose name a frame bears says nothing there.
fn already_theirs(authored: &Authored, remote: [u8; 32], own: [u8; 32]) -> bool {
    !authored.published && remote != own && from_device(&authored.message, remote)
}

/// [`Owning::carry`]: keep `message`, `from`'s frame of `workspace`, at
/// `store` when it is a sealed write, and send it to the members `serving`
/// links there.
fn carry_to(
    store: &Store,
    serving: &Serving,
    workspace: tilo_desktop_model::WorkspaceId,
    from: [u8; 32],
    message: NeighbourMsg,
) {
    if let Ok(frame) = Frame::of(&message)
        && let Err(error) = store.retain(workspace, &frame)
    {
        tracing::warn!(%error, "a frame was not kept for the workspace's members");
    }
    send_to_members(serving, workspace, from, message);
}

/// Send `message`, `from`'s frame of `workspace`, to the members `serving`
/// links there now; nothing when none is linked.
fn send_to_members(
    serving: &Serving,
    workspace: tilo_desktop_model::WorkspaceId,
    from: [u8; 32],
    message: NeighbourMsg,
) {
    let Some(link) = serving.link(workspace) else {
        return;
    };
    link.outgoing(Authored {
        from,
        message,
        published: false,
    });
}

/// What one link's live stream sends besides the frames published live.
struct Sending {
    /// The device on the other end.
    remote: [u8; 32],
    /// Whether this device dialled: the device that did asks the other one
    /// for its state over the link's control stream, with its history.
    dialing: bool,
    /// What this stream opens with.
    catch_up: Vec<Authored>,
    /// Set once the first history this device answers is written: its live
    /// frames follow it.
    first_answer: watch::Receiver<bool>,
}

/// Write `sending`'s catch-up, then every frame published live, to `send`,
/// under the name of the device that authored each, until the workspace
/// removes the device on the other end: the governance that removes it is
/// the last frame it is sent, and the link ends there.
async fn send_authored(
    link: &DirectSync,
    mut send: SendStream,
    sending: Sending,
    mut live: broadcast::Receiver<Authored>,
    mut only_here: tokio::sync::mpsc::UnboundedReceiver<OnlyHere>,
) -> Result<(), String> {
    let Sending {
        remote,
        dialing,
        catch_up,
        mut first_answer,
    } = sending;
    let own = link.device();
    for authored in &catch_up {
        write_authored(&mut send, authored).await?;
    }
    link.0.catch_up_written.send_replace(true);
    while !*first_answer.borrow_and_update() {
        if first_answer.changed().await.is_err() {
            return Ok(());
        }
    }
    loop {
        let next = tokio::select! {
            Some(first) = only_here.recv() => {
                if link.removed(remote) {
                    return Err(REMOVED.into());
                }
                let mut waiting = vec![first];
                while let Ok(more) = only_here.try_recv() {
                    waiting.push(more);
                }
                // The device that dialled reads the other one's state with its
                // history, judged as history; the one that accepted asks on
                // this stream.
                if waiting.contains(&OnlyHere::Ask) {
                    if dialing {
                        link.ask_replay();
                    } else {
                        write_authored(&mut send, &link.state_request()).await?;
                    }
                }
                if waiting.contains(&OnlyHere::Answer) {
                    for state in link.answer_state_request().await {
                        write_authored(&mut send, &state).await?;
                    }
                }
                continue;
            }
            next = live.recv() => next,
        };
        match next {
            Ok(authored) if already_theirs(&authored, remote, own) => {}
            Ok(authored) => {
                // The governance that removes the device is the last frame it
                // is sent, whichever path told this device of the removal.
                let governance = matches!(authored.message, NeighbourMsg::SyncResp { .. });
                if !governance && link.removed(remote) {
                    return Err(REMOVED.into());
                }
                write_authored(&mut send, &authored).await?;
                if governance && link.removed(remote) {
                    return Err(REMOVED.into());
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("the link fell behind the device's changes".into());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

/// Write `authored` as this link's next record: the device that authored it,
/// then its frame.
async fn write_authored(send: &mut SendStream, authored: &Authored) -> Result<(), String> {
    let bytes = postcard::to_allocvec(&(authored.from, &authored.message))
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    crate::frames::write_frame(send, &bytes).await
}

/// The next frame this link brought, with its author, or `None` once the
/// other side finished its stream.
async fn read_authored(recv: &mut RecvStream) -> Result<Option<Authored>, String> {
    match crate::frames::read_frame(recv, MAX_FRAME_BYTES).await? {
        Some(bytes) => postcard::from_bytes::<([u8; 32], NeighbourMsg)>(&bytes)
            .map(|(from, message)| {
                Some(Authored {
                    from,
                    message,
                    published: false,
                })
            })
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
        sign_device_cert, sign_revocation_entry,
    };

    const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    /// A member device `n` certified by `root`.
    fn device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    fn revocation(root: &SigningKey, device: [u8; 32]) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    /// A link over a replica of `roster`'s workspace, its first certificate
    /// the authority's, holding `revocations`.
    fn link(roster: &[(SigningKey, DeviceCert)], revocations: RevocationList) -> DirectSync {
        let (key, authority) = roster.first().unwrap();
        let now = tilo_clock::wall();
        let mut peer = StatePeer::new(
            WORKSPACE,
            authority.clone(),
            roster
                .iter()
                .map(|(_, certificate)| certificate.clone())
                .collect(),
            revocations,
            now,
        )
        .unwrap();
        peer.select_admission_v5().unwrap();
        let roster = roster
            .iter()
            .map(|(_, certificate)| certificate.clone())
            .collect::<Vec<_>>();
        peer.install_seed(
            crate::SeedState {
                workspace_changes: crate::seed_workspace(WORKSPACE, authority, &roster).unwrap(),
                documents: Vec::new(),
            },
            key,
            now,
        )
        .unwrap();
        DirectSync::new(peer, key.clone())
    }

    /// A revocation refuses the device of the root that signed it, and no
    /// other: another root's entry naming the same key leaves the device a
    /// device of the workspace's.
    #[test]
    fn a_revocation_refuses_only_the_device_its_own_root_certified() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let roster = [device(&root, 2), device(&root, 3)];
        let member = roster[1].1.clone();
        let stranger = SigningKey::from_bytes(&[7; 32]);

        let mut theirs = RevocationList::new();
        theirs
            .insert(revocation(&stranger, member.device_public_key))
            .unwrap();
        let sync = link(&roster, theirs);
        assert_eq!(
            sync.admits(member.device_public_key),
            Ok(()),
            "a stranger's entry refuses nothing of the member's device"
        );
        assert_eq!(
            sync.admits(SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes()),
            Err("the device is not in the workspace roster".to_owned())
        );

        // The member's own root's entry is what refuses it.
        assert!(
            sync.0
                .replica
                .write(|peer| {
                    peer.governance_mut().publish_governed_revocations(
                        &[revocation(&root, member.device_public_key)],
                        &roster[0].0,
                        tilo_clock::wall(),
                    )
                })
                .unwrap()
        );
        assert_eq!(
            sync.admits(member.device_public_key),
            Err("the device is revoked".to_owned())
        );
    }
}
