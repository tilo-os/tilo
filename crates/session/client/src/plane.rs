//! What a participant says to the collaboration plane, and how a host carries it.
//!
//! The client says these commands and never a transport: a peer actor, a page's endpoint or a
//! program's session carries them. What only one host can do is that host's own vocabulary beside
//! this one.

use iroh_base::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::document_frame::DocumentRoute;

/// A host's own transport of the plane: the client says `outgoing`, reads `arrived` and what became
/// of what it asked a host for, and the transport holds the other ends.
pub struct Transport {
    /// What the client says to the plane.
    pub outgoing: std::sync::Arc<dyn Outbox>,
    /// What the plane heard, as the client reads it.
    pub arrived: std::sync::mpsc::Receiver<Event>,
    /// What became of the enrollments and entries asked for since the last call.
    pub enrolled: std::sync::mpsc::Receiver<crate::Enrolled>,
    /// The workspace presence tracks this host says on: this desktop's live plane, or a page's own
    /// endpoint. `None` where the host says presence nowhere.
    pub saying: Option<std::sync::Arc<dyn SayPresence>>,
}

/// The transport a command was said to is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the session is closed")
    }
}

impl std::error::Error for Closed {}

impl From<Closed> for String {
    fn from(closed: Closed) -> Self {
        closed.to_string()
    }
}

/// Where the answer to a command goes. A transport that does not carry the command drops it, which
/// its receipt reads as no answer.
pub type Answer<T> = oneshot::Sender<T>;

/// What a command's answer arrives on: awaited on a page, waited on by a program.
pub type Receipt<T> = oneshot::Receiver<T>;
/// Why a [`Receipt`] holds no answer yet, or never will.
pub use oneshot::{RecvTimeoutError, TryRecvError};

/// A command's answer and its receipt.
#[must_use]
pub fn answer<T>() -> (Answer<T>, Receipt<T>) {
    oneshot::channel()
}

/// Where a client says what it says to the plane.
pub trait Outbox: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Say `command` to the plane.
    ///
    /// # Errors
    /// The transport is gone.
    fn send(&self, command: Cmd) -> Result<(), Closed>;

    /// Write `mutation` to `route`: split into signed frames, committed in order.
    ///
    /// # Errors
    /// The mutation does not encode, or the transport is gone.
    fn write(
        &self,
        route: DocumentRoute,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<(), String> {
        let frames = mutation
            .encode_frames()
            .map_err(|error| error.to_string())?;
        Ok(self.send(Cmd::Write(route, frames))?)
    }

    /// Take part in workspace `workspace` (its UUID) of this device's person, named `name`, once the
    /// host governs it ([`Cmd::JoinPlaced`]).
    ///
    /// # Errors
    /// The transport is gone.
    fn join_placed(&self, workspace: &str, name: &str) -> Result<(), Closed> {
        self.send(Cmd::JoinPlaced(workspace.to_owned(), name.to_owned()))
    }

    /// A receipt that completes once every write said before it is durable at the host, or with
    /// the refusal that prevents it.
    ///
    /// # Errors
    /// The transport is gone.
    fn barrier(&self) -> Result<Receipt<Result<(), String>>, Closed> {
        let (answer, receipt) = answer();
        self.send(Cmd::Barrier(answer))?;
        Ok(receipt)
    }

    /// Ask the host's workspace administration `request`: the receipt completes with its answer.
    ///
    /// # Errors
    /// The transport is gone.
    fn ask_workspaces(
        &self,
        request: tilo_relay_protocol::workspaces::WorkspacesRequest,
    ) -> Result<Receipt<Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>>, Closed>
    {
        let (answer, receipt) = answer();
        self.send(Cmd::Administer(request, answer))?;
        Ok(receipt)
    }
}

impl Outbox for tokio::sync::mpsc::UnboundedSender<Cmd> {
    fn send(&self, command: Cmd) -> Result<(), Closed> {
        tokio::sync::mpsc::UnboundedSender::send(self, command).map_err(|_| Closed)
    }
}

impl Outbox for std::sync::Arc<tokio::sync::mpsc::UnboundedSender<Cmd>> {
    fn send(&self, command: Cmd) -> Result<(), Closed> {
        self.as_ref().send(command).map_err(|_| Closed)
    }
}

/// Where a client's presence frames go: one host's live plane. Presence is ephemeral, so a frame
/// that finds no plane is dropped rather than held.
pub trait SayPresence: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Say `frames`, one burst, each as the object its number names, on `workspace`'s presence
    /// track: the burst travels whole, so the newest one carries all of it.
    ///
    /// # Errors
    /// This host holds no plane for the workspace, or the plane could not carry it.
    fn say(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        frames: &[(u32, collab_types::presence::Presence)],
    ) -> Result<(), String>;
}

/// What a host's transport announces about the device it runs for: where the plane knows it and
/// where the others reach it, and the person and the name its certificate gives it.
#[derive(Clone, Debug)]
pub struct Device {
    /// Where the plane knows this device.
    pub publisher: EndpointId,
    /// Where the others reach it.
    pub endpoint: EndpointAddr,
    /// The person this device belongs to, by their root key, as its certificate names them.
    pub person: Option<[u8; 32]>,
    /// What this device calls itself, from its certificate.
    pub name: String,
}

/// Something the plane heard, as every host hands it to the client.
#[derive(Clone, Debug)]
pub enum Event {
    /// A member of `ws` said `msg`.
    Msg {
        ws: Topic,
        peer: EndpointId,
        msg: Box<NeighbourMsg>,
    },
    /// A member of `ws` is gone.
    PeerLeft { ws: Topic, peer: EndpointId },
    /// A member of `ws` this device reaches through the workspace host `host` joined (`active`) or
    /// left the host's live plane.
    HostMember {
        ws: Topic,
        host: EndpointId,
        device: EndpointId,
        active: bool,
    },
    /// A host of `ws` finished subscribing to this workspace. `records_when_opened`: it sends a
    /// device only the workspace's governance, its shared desktop and the heads of its chained
    /// records, and every other record once the device opens it.
    HostReady {
        ws: Topic,
        peer: EndpointId,
        records_when_opened: bool,
    },
    /// A host of `ws` sent everything it retains of a record this device opened on `channel`,
    /// `frames` of them — none when the workspace has none of it — each landed before this; its
    /// changes keep arriving from now on. `route` names the record where the host carrying it
    /// opens records by route.
    RecordOpened {
        ws: Topic,
        channel: [u8; 32],
        route: Option<DocumentRoute>,
        frames: u32,
    },
    /// A write to `route` landed in this device's replica of its workspace — `author`'s, or
    /// `None` when keys arrived that open what it held sealed: read it again from the
    /// [`crate::engine::Engine`].
    Landed {
        route: DocumentRoute,
        author: Option<[u8; 32]>,
    },
    /// The host refused this device's write to `route`: `changes` are the Automerge changes it
    /// would have brought, which whoever holds the record withdraws.
    Refused {
        route: DocumentRoute,
        changes: Vec<[u8; 32]>,
        reason: String,
    },
    /// A live frame the device `from` published on `ws`, opened with its record's channel keys.
    Live {
        ws: Topic,
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// A remote-action frame another device of the workspace addressed to this one, or word that
    /// one this device sent could not be delivered.
    Remote(tilo_session_core::remote::Heard),
    /// A piece of content `ws`'s records reference, which `from` sent this device alone: a
    /// postcard-encoded `NeighbourMsg::RecordContentPiece`.
    RecordContent {
        ws: Topic,
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// What `from` sealed on `track` of `ws`'s live plane, object `sequence`, opened.
    LivePlane {
        ws: Topic,
        from: [u8; 32],
        track: tilo_live::live::Track,
        sequence: u32,
        body: Vec<u8>,
    },
    /// One Opus frame of `from`'s voice in a call on `ws`, `sequence` in its stream, starting at
    /// `timestamp_48k` in the sender's 48 kHz timeline.
    CallAudio {
        ws: Topic,
        from: [u8; 32],
        sequence: u64,
        timestamp_48k: u64,
        opus: Vec<u8>,
    },
    /// One tilo-video/1 object of `from`'s camera or screen in a call on `ws`; a `keyframe`
    /// starts a group.
    CallPicture {
        ws: Topic,
        from: [u8; 32],
        source: tilo_live::video::Source,
        keyframe: bool,
        object: Vec<u8>,
    },
    /// What this device's call on `ws` now encodes: the layers of its camera and screen the
    /// call's devices show, and the sources they need a keyframe of.
    CallDemand {
        ws: Topic,
        wanted: Vec<(tilo_live::video::Source, tilo_live::video::Layer)>,
        keyframes: Vec<tilo_live::video::Source>,
    },
    /// What the host said about this device's calls.
    Voice(VoiceEvent),
    /// One of the person's devices proved itself, or proved itself anew.
    Sibling {
        peer: EndpointId,
        certs: Vec<tilo_user_identity::DeviceCert>,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    },
    /// The session with one of the person's devices ended.
    SiblingLeft { peer: EndpointId },
    /// One of the person's devices sent `msg` on its session.
    SiblingMsg {
        peer: EndpointId,
        msg: Box<NeighbourMsg>,
    },
}

/// What the host said about this device's calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceEvent {
    /// The answer to asking for a workspace's calls.
    Calls {
        ws: Topic,
        /// Its calls, or why it lists none.
        result: Result<Vec<tilo_relay_protocol::voice::CallSummary>, String>,
    },
    /// The answer to the call change `request`.
    CallResult {
        ws: Topic,
        /// The request's id.
        request: [u8; 16],
        /// The call now, or why the change was refused.
        result: Result<tilo_relay_protocol::voice::CallState, String>,
    },
    /// The participation's current grant: the first after a join, then
    /// whenever the roster, the directions or the token change.
    Granted {
        ws: Topic,
        grant: Box<tilo_relay_protocol::voice::VoiceGrant>,
    },
    /// The participation ended: the call ended, the device may no longer
    /// take part, or the host refused the join.
    Ended {
        ws: Topic,
        call: [u8; 32],
        /// Why.
        reason: String,
    },
}

/// How the plane numbers one workspace's topic.
pub type Topic = u64;

/// A device on a workspace's roster, and when its certificate expires (Unix nanoseconds; `None` when
/// it never does).
pub type RosterDevice = (EndpointId, Option<u64>);

/// The topic a workspace's plane numbers itself by, from the workspace's id: number 0 for a desk
/// that shows no workspace.
#[must_use]
pub fn topic_of(workspace: tilo_desktop_model::WorkspaceId) -> Topic {
    tilo_relay_protocol::workspace_topic(&workspace.to_string())
}

/// A command a client sends to the plane.
#[derive(Debug)]
pub enum Cmd {
    /// Take part in the topic of a workspace.
    Join(Topic),
    /// Stop taking part in the topic of a workspace.
    Leave(Topic),
    /// Publish `msg` on the topic of a workspace.
    Publish { ws: Topic, msg: Box<NeighbourMsg> },
    /// Say one shared window's control message (a viewer's ask, input or keyframe request, a page's
    /// announcement) to `target` alone: over the authenticated direct route while one is up, and
    /// through the workspace's host to that device alone otherwise. A window's pictures travel the
    /// workspace's live plane ([`tilo_live::live::Track::Window`]).
    SendControl {
        ws: Topic,
        msg: Box<NeighbourMsg>,
        target: EndpointId,
    },
    /// Dial a workspace host: an always-on peer that keeps what this person's devices publish while
    /// the others are away.
    Connect(EndpointAddr),
    /// Hear the person's other devices from now on.
    WatchSiblings,
    /// Say `msg` to each of the person's devices whose session is up.
    TellSiblings(Box<NeighbourMsg>),
    /// Dial one of the person's own devices, for the session that carries their device fabric.
    ConnectDevice(EndpointAddr),
    /// Ask the host of the joined workspace on `ws` to replay what it retains.
    Replay(Topic),
    /// Ask the hosts of the joined workspace on `ws` for the record on `channel`: what they hold of
    /// it, and their changes to it from then on.
    OpenRecord { ws: Topic, channel: [u8; 32] },
    /// Send this device's owner's outgoing custody of the workspace on `ws` to its peers.
    PublishCustody(Topic),
    /// Say which devices workspace `ws` admits, and until when each of their certificates holds.
    Roster {
        ws: Topic,
        devices: Vec<RosterDevice>,
    },
    /// Take part in a workspace of this device's person (its UUID), named as given, once the host
    /// governs it: a host first asked to govern it creates it unless it does.
    JoinPlaced(String, String),
    /// Open a route, an on-demand record of the joined workspace on the topic that a join does not
    /// replay: its history lands, then [`Event::RecordOpened`], and it is opened again on every
    /// reconnect.
    OpenRecordAt(Topic, DocumentRoute),
    /// Take part in a route's encrypted channel: ask its participants for access.
    OpenRoute(DocumentRoute),
    /// Commit the frames of one write to a route, in order ([`Outbox::write`]). A
    /// [`crate::Link`] holds them in its engine, readable at once, and says the route alone.
    Write(DocumentRoute, Vec<Vec<u8>>),
    /// Hold these keys as a route's channel keys, as if a participant had admitted this session to
    /// the channel: for an application endpoint of a device that takes part in it.
    InstallChannelKeys(DocumentRoute, channel_crypto::ChannelContentKeys),
    /// Publish one canonical live frame of the workspace on the topic, sealed with the keys of the
    /// record it names; dropped while disconnected or without those keys.
    Live(Topic, Vec<u8>),
    /// Seal one canonical live frame of the workspace on the topic with the keys of the record it
    /// names, for another carrier: the answer is the sealed frame, or `None` without those keys.
    SealLive(Topic, Vec<u8>, Answer<Option<Vec<u8>>>),
    /// Send one remote-action frame to a device of the workspace on the topic.
    RemoteAction(Topic, [u8; 32], Vec<u8>),
    /// Ask the workspace on the topic for content its records reference: a postcard-encoded
    /// `NeighbourMsg::RecordContentWant`. The pieces come back as [`Event::RecordContent`].
    WantContent(Topic, Vec<u8>),
    /// Answer the members of the workspace on the topic that ask for content from what is kept.
    KeepContent(Topic, std::sync::Arc<dyn crate::content::Kept>),
    /// Answer once every write said before this is durable at the host ([`Outbox::barrier`]).
    Barrier(Answer<Result<(), String>>),
    /// Say these frames on the presence track of the workspace on the topic.
    Presence(Topic, Vec<collab_types::presence::Presence>),
    /// The host's word for a run it executes, on the workspace on the topic.
    SayForRun(Topic, tilo_relay_protocol::host_voice::HostWords),
    /// Microphone frames, said on the plane of the workspace on the topic as this device's voice.
    CallAudio(Topic, Vec<CallFrame>),
    /// Ask for the calls of the workspace on the topic: the answer is a [`VoiceEvent::Calls`].
    VoiceCalls(Topic),
    /// Change a call of the workspace on the topic under a request id: the answer is a
    /// [`VoiceEvent::CallResult`].
    VoiceCall(Topic, [u8; 16], tilo_relay_protocol::voice::CallOperation),
    /// Take part in a call of the workspace on the topic.
    VoiceJoin(Topic, [u8; 32]),
    /// Leave the call this device takes part in on the topic.
    VoiceLeave(Topic),
    /// What this device shows of the call it takes part in on the topic.
    VoiceShow(Topic, tilo_relay_protocol::voice::Showing),
    /// Pictures this device's camera and screen encoded, for the plane of the workspace on the
    /// topic.
    CallPictures(Topic, Vec<CallPicture>),
    /// What this device shows of each device in its call on the topic.
    CallShowing(Topic, Vec<CallShown>),
    /// The windows this device draws, by the device hosting them: their pages, or their pictures.
    ShowPages(Topic, Vec<PageShown>),
    /// This device needs a source of a device afresh: a keyframe from its next group on.
    CallPictureRestart(Topic, [u8; 32], tilo_live::video::Source),
    /// Ask the host's workspace administration a request ([`Outbox::ask_workspaces`]).
    Administer(
        tilo_relay_protocol::workspaces::WorkspacesRequest,
        Answer<Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>>,
    ),
    /// Say this session is one run of the agent the label names: every request it makes from then
    /// on is the agent's, for this device's person, and the person's agent door lists the run while
    /// the session lasts. The answer is the host's.
    AgentRun(String, Answer<Result<(), String>>),
    /// Enter another workspace through its door, on a stream of its own on this session's
    /// connection: a device in a session with its host redeems an invitation this way. The answer
    /// is the host's, or why there was none.
    Enter(
        tilo_relay_protocol::entering::Entering,
        Answer<Result<tilo_relay_protocol::entering::Entered, tilo_relay_client::LinkError>>,
    ),
    /// Ask the host a member's root-signed approver command (pairing, agents' questions).
    AskMember(
        Box<tilo_relay_protocol::certify::MemberApproval>,
        Answer<
            Result<tilo_relay_protocol::certify::ApproverReply, tilo_relay_client::CertifyError>,
        >,
    ),
    /// Announce a revocation this device's person's root signed to every workspace this session is
    /// admitted to.
    Revoke(tilo_user_identity::RevocationEntry),
    /// Drop the connection and reconnect now, also after a failure.
    Reconnect,
    /// The network is back: a session waiting out its backoff tries again now.
    Retry,
    /// Ask the host for every joined workspace's shared state again.
    Refresh,
    /// The client is going: stop the plane.
    Stop,
}

/// One Opus frame of this device's microphone, as its call publishes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallFrame {
    /// The first sample's place in the device's 48 kHz timeline.
    pub timestamp_48k: u64,
    /// The Opus frame.
    pub opus: Vec<u8>,
}

/// One tilo-video/1 object this device encoded for its call's plane: one layer of its camera or
/// screen.
#[derive(Clone, Debug)]
pub struct CallPicture {
    pub source: tilo_live::video::Source,
    pub layer: tilo_live::video::Layer,
    /// The object's place in the source's stream.
    pub sequence: u64,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    /// The encoded access unit.
    pub payload: Vec<u8>,
}

/// What this device shows of one device in its call's plane.
#[derive(Clone, Debug)]
pub struct CallShown {
    pub device: [u8; 32],
    /// The camera layers it draws.
    pub camera: Vec<tilo_live::video::Layer>,
    /// The screen layers it draws.
    pub screen: Vec<tilo_live::video::Layer>,
}

/// The windows one device of a workspace hosts, as this device draws them.
#[derive(Clone, Debug, Default)]
pub struct PageShown {
    /// The device hosting the windows.
    pub device: [u8; 32],
    /// The tabs the windows stand in, the same ids on every device of the workspace.
    pub tabs: Vec<u64>,
}
