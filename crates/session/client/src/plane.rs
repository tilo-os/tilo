//! What a participant says to the collaboration plane, and how a host carries it.
//!
//! The client says these commands and never a transport: a peer actor, a page's endpoint or a
//! program's session carries them. What only one host can do is that host's own vocabulary beside
//! this one.

use iroh_base::{EndpointAddr, EndpointId};
use neighbour_wire::{NeighbourMsg, PeerId};
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

/// Where a client says what it says to the plane.
pub trait Outbox: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Say `command` to the plane. `false` when the transport is gone.
    fn send(&self, command: Cmd) -> bool;
}

impl Outbox for tokio::sync::mpsc::UnboundedSender<Cmd> {
    fn send(&self, command: Cmd) -> bool {
        tokio::sync::mpsc::UnboundedSender::send(self, command).is_ok()
    }
}

impl Outbox for std::sync::Arc<tokio::sync::mpsc::UnboundedSender<Cmd>> {
    fn send(&self, command: Cmd) -> bool {
        self.as_ref().send(command).is_ok()
    }
}

/// Where a client's presence frames go: one host's live plane. Presence is ephemeral, so a frame
/// that finds no plane is dropped rather than held.
pub trait SayPresence: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Say `frame` as object `sequence` of `workspace`'s presence track.
    ///
    /// # Errors
    /// This host holds no plane for the workspace, or the plane could not carry it.
    fn say(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        sequence: u32,
        frame: &collab_types::presence::Presence,
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
    /// Send one window's stream on the topic of the workspace it is shared in, to the members that
    /// asked to view it and no one else.
    PublishSurfaceTo {
        ws: Topic,
        msg: Box<NeighbourMsg>,
        viewers: Vec<PeerId>,
    },
    /// Say one workspace-control message to `target` alone, over the authenticated direct route.
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
    /// The client is going: stop the plane.
    Stop,
}
