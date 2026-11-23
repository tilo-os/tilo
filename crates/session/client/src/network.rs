//! What a member says to the peer network, and how a host carries it.
//!
//! The client says these commands and never a transport: a peer actor, a page's endpoint or a
//! program's session carries them. What only one host can do is that host's own vocabulary beside
//! this one.

use iroh_base::{EndpointAddr, EndpointId};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::DocumentAnswer;

/// A host's own transport of the peer network: the client says `outgoing`, reads `arrived` and what became
/// of what it asked a host for, and the transport holds the other ends.
pub struct Transport {
    /// What the client says to the peer network.
    pub outgoing: std::sync::Arc<dyn Outbox>,
    /// What the peer network heard, as the client reads it.
    pub arrived: std::sync::mpsc::Receiver<Event>,
    /// What became of the enrollments and entries asked for since the last call.
    pub enrolled: std::sync::mpsc::Receiver<crate::Enrolled>,
    /// The workspace presence tracks this host says on: this desktop's live session, or a page's own
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

/// What writes this device's session still keeps wait for ([`Outbox::kept_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Waits {
    /// Their workspace's keys: they are not sealed yet.
    Keys,
    /// The document they change: the first of them is a change, and this device holds nothing
    /// of the document.
    Document,
    /// What the workspace has not admitted on this device yet, in its replica's words.
    Prerequisite(String),
    /// Their document's history to be compacted: it is full.
    History,
    /// This device's signature.
    Signature,
    /// Their turn: they are held with their keys and not committed to this device's replica yet.
    Commit,
    /// Their holder's answer: they are committed here, and their holder has not said it holds
    /// them.
    Holder,
}

impl std::fmt::Display for Waits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Keys => "its workspace's keys",
            Self::Document => "the document it changes, which is not here",
            Self::Prerequisite(what) => what,
            Self::History => "its document's history to be compacted",
            Self::Signature => "this device's signature",
            Self::Commit => "its turn to be committed here",
            Self::Holder => "its holder's answer",
        })
    }
}

/// The writes of this device to one document that its session still keeps: not durable at their
/// holder yet ([`Outbox::kept_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// The document they were written to.
    pub route: DocumentRoute,
    /// What they wait for.
    pub waits: Waits,
    /// How long they have waited for it.
    pub age: std::time::Duration,
}

/// Where a client says what it says to the peer network.
pub trait Outbox: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Say `command` to the peer network.
    ///
    /// # Errors
    /// The transport is gone.
    fn send(&self, command: Cmd) -> Result<(), Closed>;

    /// Publish `op`, a write of this device that its holder signed and this replica admitted
    /// already: it goes into this device's ownership as it is, sent once its host is reached
    /// and answered for like any write of this device's. Never signed again, and never
    /// admitted twice.
    ///
    /// # Errors
    /// The transport is gone.
    fn publish_signed(
        &self,
        route: DocumentRoute,
        op: collab_types::SignedOp,
    ) -> Result<(), Closed> {
        self.send(Cmd::PublishSigned(route, Box::new(op)))
    }

    /// Write `mutation` to `route` as this device's own: the session's holder drafts, seals and
    /// signs it ([`tilo_peer_state::workspace_documents::DeviceWrites::write`]) and publishes it
    /// as it was signed ([`Outbox::publish_signed`]). A write that cannot be drafted yet — its
    /// record's keys are not here, or the record it changes is not — waits for them.
    ///
    /// # Errors
    /// The transport is gone.
    fn write(
        &self,
        route: DocumentRoute,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<(), String> {
        let _ = (route, mutation);
        Err("this transport carries no write of this device's".to_owned())
    }

    /// Take part in workspace `workspace` (its UUID) of this device's person, named `name`, once the
    /// host governs it ([`Cmd::JoinPlaced`]).
    ///
    /// # Errors
    /// The transport is gone.
    fn join_placed(&self, workspace: &str, name: &str) -> Result<(), Closed> {
        self.send(Cmd::JoinPlaced(workspace.to_owned(), name.to_owned()))
    }

    /// A receipt that completes once every write said before it is durable at its holder, or
    /// with the refusal that prevents it.
    ///
    /// # Errors
    /// The transport is gone.
    fn barrier(&self) -> Result<Receipt<Result<(), String>>, Closed> {
        let (answer, receipt) = answer();
        self.send(Cmd::Barrier(answer))?;
        Ok(receipt)
    }

    /// [`Self::barrier`] for `workspaces` alone: a receipt that completes once every write said
    /// before it to one of them is durable at its holder, or with the refusal that prevents it.
    /// The changes this device made to who is in one of them wait until the host answers the
    /// write that carries them. What this device still holds or owes in a workspace not named does not keep
    /// it waiting.
    ///
    /// # Errors
    /// The transport is gone.
    fn barrier_of(&self, workspaces: &[Topic]) -> Result<Receipt<Result<(), String>>, Closed> {
        let (answer, receipt) = answer();
        self.send(Cmd::BarrierOf(workspaces.to_vec(), answer))?;
        Ok(receipt)
    }

    /// [`Self::barrier`] for the writes a caller made to `routes`: a receipt that completes once
    /// every write said before it to one of those documents is durable at its holder, or with the
    /// refusal of a write to one of them. What this device still holds or owes of a document not
    /// named, of the same workspace too, does not keep it waiting, and the refusal of a write to
    /// another route is its own caller's: a change that waits for its document says nothing about
    /// the writes to another, and a session many callers share answers each for its own writes.
    ///
    /// # Errors
    /// The transport is gone.
    fn barrier_for(&self, routes: &[DocumentRoute]) -> Result<Receipt<Result<(), String>>, Closed> {
        let (answer, receipt) = answer();
        self.send(Cmd::BarrierFor(routes.to_vec(), answer))?;
        Ok(receipt)
    }

    /// What this device's session still keeps of what it wrote to `workspaces`, a document each,
    /// with what its writes wait for and for how long: none once every write to them is durable
    /// at its holder. Whoever finds a write unconfirmed ([`Self::barrier_of`]) reads here why,
    /// instead of only that it is.
    ///
    /// # Errors
    /// The transport is gone.
    fn kept_of(&self, workspaces: &[Topic]) -> Result<Receipt<Vec<Kept>>, Closed> {
        let (answer, receipt) = answer();
        self.send(Cmd::KeptOf(workspaces.to_vec(), answer))?;
        Ok(receipt)
    }

    /// Ask the host's workspace administration `request`: the receipt completes with its answer.
    ///
    /// # Errors
    /// The transport is gone.
    fn ask_workspaces(
        &self,
        request: tilo_server_protocol::workspaces::WorkspacesRequest,
    ) -> Result<Receipt<Result<tilo_server_protocol::workspaces::WorkspacesReply, String>>, Closed>
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

/// Where a client's presence frames go: one host's live session. Presence is ephemeral, so a frame
/// that finds no peer network is dropped rather than held.
pub trait SayPresence: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Say `frames`, one burst, each as the object its number names, on `workspace`'s presence
    /// track: the burst travels whole, so the newest one carries all of it.
    ///
    /// # Errors
    /// This host holds no peer network for the workspace, or the peer network could not carry it.
    fn say(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        frames: &[(u32, collab_types::presence::Presence)],
    ) -> Result<(), String>;
}

/// What a host's transport announces about the device it runs for: where the peer network knows it and
/// where the others reach it, and the person and the name its certificate gives it.
#[derive(Clone, Debug)]
pub struct Device {
    /// Where the peer network knows this device.
    pub publisher: EndpointId,
    /// Where the others reach it.
    pub endpoint: EndpointAddr,
    /// The person this device belongs to, by their root key, as its certificate names them.
    pub person: Option<[u8; 32]>,
    /// What this device calls itself, from its certificate.
    pub name: String,
}

/// Something the peer network heard, as every host hands it to the client.
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
    /// left the host's live session.
    HostMember {
        ws: Topic,
        host: EndpointId,
        device: EndpointId,
        active: bool,
    },
    /// A host of `ws` finished subscribing to this workspace. `documents_when_opened`: it sends a
    /// device only the workspace's governance, its shared desktop and the heads of its chained
    /// records, and every other record once the device opens it.
    HostReady {
        ws: Topic,
        peer: EndpointId,
        documents_when_opened: bool,
    },
    /// A host of `ws` answered a record this device opened on `channel`: it sent everything it
    /// retains of it — none when the workspace has none of it — each landed before this, and its
    /// changes keep arriving from now on; or the record is another person's own, and nothing of it
    /// is sent. `route` names the record where the host carrying it opens records by route.
    DocumentOpened {
        ws: Topic,
        channel: [u8; 32],
        route: Option<DocumentRoute>,
        answer: DocumentAnswer,
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
        refusal: tilo_server_protocol::refusal::Refusal,
    },
    /// A live frame the device `from` published on `ws`, opened with its record's channel keys.
    Live {
        ws: Topic,
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// A remote-action frame another device of the workspace addressed to this one, or word that
    /// one this device sent could not be delivered.
    Remote(tilo_session_actions::Heard),
    /// A piece of content `ws`'s records reference, which `from` sent this device alone: a
    /// postcard-encoded `NeighbourMsg::DocumentContentPiece`.
    DocumentContent {
        ws: Topic,
        from: [u8; 32],
        frame: Vec<u8>,
    },
    /// What `from` sealed on `track` of `ws`'s live session, object `sequence`, opened.
    LiveSession {
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
    /// A joined workspace moved to `state` on this session.
    Workspace {
        ws: Topic,
        state: crate::states::WorkspaceState,
    },
    /// Whether this device's writes are with its host changed.
    Sync(crate::states::SyncState),
    /// One of the person's devices sent `msg` on its session.
    SiblingMsg {
        peer: EndpointId,
        msg: Box<NeighbourMsg>,
    },
    /// The person's workspace list, as their host keeps it now: sent whenever
    /// a workspace of theirs is made, joined, renamed, left or deleted, so no
    /// device has to ask the host again.
    Personal(Box<tilo_server_protocol::workspaces::PersonalList>),
}

/// How the peer network numbers one workspace's topic.
pub type Topic = tilo_desktop_model::WorkspaceId;

/// A device on a workspace's roster, and when its certificate expires (Unix nanoseconds; `None` when
/// it never does).
pub type RosterDevice = (EndpointId, Option<u64>);

/// The topic a workspace's peer network numbers itself by: the workspace's id, and
/// nothing for a layout that shows no workspace.
#[must_use]
pub fn topic_of(workspace: tilo_desktop_model::WorkspaceId) -> Topic {
    workspace
}

/// A command a client sends to the peer network.
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
    /// workspace's live session ([`tilo_live::live::Track::Window`]).
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
    /// Dial the holders of the joined workspace on `ws` again now: the person asked to, after it
    /// could not arrive.
    Redial(Topic),
    /// Ask the hosts of the joined workspace on `ws` for the record on `channel`: what they hold of
    /// it, and their changes to it from then on.
    OpenDocument { ws: Topic, channel: [u8; 32] },
    /// Send this device's owner's outgoing ownership of the workspace on `ws` to its peers.
    PublishOwnership(Topic),
    /// Say which devices workspace `ws` admits, and until when each of their certificates holds.
    Roster {
        ws: Topic,
        devices: Vec<RosterDevice>,
    },
    /// Take part in a workspace of this device's person (its UUID), named as given, once the host
    /// governs it: a host first asked to govern it creates it unless it does.
    JoinPlaced(String, String),
    /// Open a route, an on-demand record of the joined workspace on the topic that a join does not
    /// replay: its history lands, then [`Event::DocumentOpened`], and it is opened again on every
    /// reconnect.
    OpenDocumentAt(Topic, DocumentRoute),
    /// Take part in a route's encrypted channel: ask its members for access.
    OpenRoute(DocumentRoute),
    /// Write one mutation of `route` as this device's own ([`Outbox::write`]): the session drafts
    /// it through the device's holder, and publishes it as the signed op its replica admitted. A
    /// [`crate::Link`] holds the mutation and counts it at once, and says the route alone.
    Write(DocumentRoute),
    /// Publish `op`, a write of this device its holder signed and its replica admitted already
    /// ([`Outbox::publish_signed`]): ownership carries it as it is, once, and sends it.
    PublishSigned(DocumentRoute, Box<collab_types::SignedOp>),
    /// Hold these keys as a route's channel keys, as if a member had admitted this session to
    /// the channel: for an application endpoint of a device that takes part in it.
    InstallGroupKeys(DocumentRoute, key_group::ContentKeys),
    /// Publish one canonical live frame of the workspace on the topic, sealed with the keys of the
    /// record it names; dropped while disconnected or without those keys.
    Live(Topic, Vec<u8>),
    /// Seal one canonical live frame of the workspace on the topic with the keys of the record it
    /// names, for another carrier: the answer is the sealed frame, or `None` without those keys.
    SealLive(Topic, Vec<u8>, Answer<Option<Vec<u8>>>),
    /// Send one remote-action frame to a device of the workspace on the topic.
    RemoteAction(Topic, [u8; 32], Vec<u8>),
    /// Ask the workspace on the topic for content its records reference: a postcard-encoded
    /// `NeighbourMsg::DocumentContentWant`. The pieces come back as [`Event::DocumentContent`].
    WantContent(Topic, Vec<u8>),
    /// Answer the members of the workspace on the topic that ask for content from what is kept.
    KeepContent(Topic, std::sync::Arc<dyn crate::content::Kept>),
    /// Answer once every write said before this is durable at the host ([`Outbox::barrier`]).
    Barrier(Answer<Result<(), String>>),
    /// Answer once every write said before this to one of these workspaces is durable at its
    /// holder ([`Outbox::barrier_of`]).
    BarrierOf(Vec<Topic>, Answer<Result<(), String>>),
    /// Answer once every write said before this to one of these documents is durable at its
    /// holder, or with the refusal of a write to one of them ([`Outbox::barrier_for`]).
    BarrierFor(Vec<DocumentRoute>, Answer<Result<(), String>>),
    /// Answer what this session still keeps of what it wrote to these workspaces, and what each
    /// document's writes wait for ([`Outbox::kept_of`]).
    KeptOf(Vec<Topic>, Answer<Vec<Kept>>),
    /// Say these frames on the presence track of the workspace on the topic.
    Presence(Topic, Vec<collab_types::presence::Presence>),
    /// The host's word for a run it executes, on the workspace on the topic.
    SayForSession(Topic, tilo_server_protocol::host_voice::HostWords),
    /// Microphone frames, said on the peer network of the workspace on the topic as this device's voice.
    CallAudio(Topic, Vec<CallFrame>),
    /// Pictures this device's camera and screen encoded, for the peer network of the workspace on the
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
        tilo_server_protocol::workspaces::WorkspacesRequest,
        Answer<Result<tilo_server_protocol::workspaces::WorkspacesReply, String>>,
    ),
    /// Say this session is one run of the agent the label names: every request it makes from then
    /// on is the agent's, for this device's person, and the person's MCP server lists the run while
    /// the session lasts. The answer is the host's.
    AgentSession(String, Answer<Result<(), String>>),
    /// Enter another workspace through its entry, on a stream of its own on this session's
    /// connection: a device in a session with its host redeems an invitation this way. The answer
    /// is the host's, or why there was none.
    Enter(
        tilo_server_protocol::entering::Entering,
        Answer<Result<tilo_server_protocol::entering::Entered, tilo_server_client::LinkError>>,
    ),
    /// Ask the host a member's root-signed approver command (pairing, agents' questions).
    AskMember(
        Box<tilo_server_protocol::certify::MemberApproval>,
        Answer<
            Result<tilo_server_protocol::certify::ApproverReply, tilo_server_client::CertifyError>,
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
    /// The client is going: stop the peer network.
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

/// One tilo-video/1 object this device encoded for its call's peer network: one layer of its camera or
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

/// What this device shows of one device in its call's peer network.
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
