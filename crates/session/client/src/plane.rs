//! What a participant says to the collaboration plane, and how a host carries it.
//!
//! The client says these commands and never a transport: a peer actor, a page's endpoint or a
//! program's session carries them. What only one host can do is that host's own vocabulary beside
//! this one.

use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::{NeighbourMsg, PeerId};

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
#[derive(Debug)]
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
    /// A host of `ws` finished subscribing to this workspace.
    HostReady { ws: Topic, peer: EndpointId },
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
