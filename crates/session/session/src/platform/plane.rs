//! What a session says to the collaboration plane, and how a host carries it.
//!
//! The roles say these commands and never a transport: this desktop's peer actor carries them to
//! the workspaces' devices ([`crate::sync::client`]), and a page's own endpoint carries the same
//! ones. What only a desktop can do is gated, so the vocabulary compiles on every host.

use iroh::{EndpointAddr, EndpointId};
use neighbour_wire::{NeighbourMsg, PeerId};

/// How the plane numbers one workspace's topic.
pub(crate) type Topic = u64;

/// A command a session sends to the plane.
#[derive(Debug)]
#[cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's transport is handed these next")
)]
pub(crate) enum Cmd {
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
