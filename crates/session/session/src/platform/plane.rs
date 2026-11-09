//! What a session says to the collaboration plane, and how a host carries it.
//!
//! The roles say these commands and never a transport: this desktop's peer actor carries them to
//! the workspaces' devices ([`crate::sync::client`]), and a page's own endpoint carries the same
//! ones. What only a desktop can do is gated, so the vocabulary compiles on every host.

use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::{NeighbourMsg, PeerId};

/// A host's own transport of the collaboration plane, as it and the session hand each other what
/// they have: the session says `outgoing`, reads what `arrived` and what became of what it asked a
/// workspace's host for, and the transport holds the other ends.
pub struct Transport {
    /// What the session says to the plane.
    pub outgoing: tokio::sync::mpsc::UnboundedSender<Cmd>,
    /// What the plane heard, as the session reads it.
    pub arrived: std::sync::mpsc::Receiver<Event>,
    /// What became of the enrollments and entries asked for since the last call.
    pub enrolled: std::sync::mpsc::Receiver<crate::sync::Enrolled>,
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

/// Something the plane heard, as every host hands it to the session.
#[derive(Debug)]
#[cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's transport is handed these next")
)]
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
    /// What one of the person's devices says of its fabric.
    #[cfg(target_os = "linux")]
    PersonalFabric {
        peer: EndpointId,
        payload: tilo_network_actor::PersonalFabricPayload,
        /// When the frame reached this device, read where it was taken off the wire.
        received: std::time::Instant,
    },
    /// How far a stream this device is sending got.
    #[cfg(target_os = "linux")]
    NearbyTransferProgress {
        transfer_id: [u8; 16],
        sent_bytes: u64,
        total_bytes: u64,
        status: tilo_network_actor::PersonalTransferStatus,
    },
}

/// How the plane numbers one workspace's topic.
pub type Topic = u64;

/// The topic a workspace's plane numbers itself by, from the workspace's id: number 0 for a desk
/// that shows no workspace.
#[must_use]
pub fn topic_of(workspace: tilo_desktop_model::WorkspaceId) -> Topic {
    tilo_relay_protocol::workspace_topic(&workspace.to_string())
}

/// A command a session sends to the plane.
#[derive(Debug)]
#[cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's transport is handed these next")
)]
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
    /// Ask `ws`'s hosts for the record on `channel`, which they replay a joining device only once
    /// it asks.
    OpenRecord { ws: Topic, channel: [u8; 32] },
    /// Ask the host of the joined workspace on `ws` to replay what it retains.
    Replay(Topic),
    /// Send this device's owner's outgoing custody of the workspace on `ws` to its peers.
    PublishCustody(Topic),
    /// Say which devices workspace `ws` admits, and until when each of their certificates holds.
    Roster {
        ws: Topic,
        devices: Vec<(EndpointId, Option<u64>)>,
    },
    /// The session is going: stop the plane.
    Stop,
    /// Take part in a workspace through this device's own custody session with its host.
    #[cfg(target_os = "linux")]
    TakePart(tilo_network_actor::native_ingress::Connect),
    /// Send one piece of a record's content to the device of the workspace that asked for it.
    #[cfg(target_os = "linux")]
    Content(tilo_network_actor::RecordContentDelivery),
    /// Redeem an invitation into the workspace the desk made for it.
    #[cfg(target_os = "linux")]
    Enroll(crate::sync::Enrolling),
    /// Take one of the person's workspaces in as their further device, at the host the list names.
    #[cfg(target_os = "linux")]
    Enter(crate::sync::Entering),
    /// Send this device's fabric to the person's other devices, or to the one at `target`.
    #[cfg(target_os = "linux")]
    Fabric {
        user_public_key: [u8; 32],
        target: Option<EndpointId>,
        payload: tilo_network_actor::PersonalFabricPayload,
    },
    /// Stream one staged spool object to one of the person's devices from the checkpoint it
    /// confirmed.
    #[cfg(target_os = "linux")]
    NearbySpool {
        user_public_key: [u8; 32],
        target: EndpointId,
        checkpoint: tilo_network_actor::NearbyTransferCheckpoint,
        file: std::fs::File,
    },
}
