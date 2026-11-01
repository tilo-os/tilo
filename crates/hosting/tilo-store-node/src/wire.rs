//! What a member and a store say to each other on [`ALPN`](crate::ALPN).
//!
//! One framed postcard message each way, in request/reply order: a device
//! proves itself once with its certificate, then asks from its cursors what
//! it lacks and offers the frames it published. Frames travel as the
//! encoded [`NeighbourMsg`](neighbour_wire::NeighbourMsg) they are, so a
//! store hands back exactly what a member published.

use crate::store::{Cursor, Floor};
use tilo_user_identity::{DeviceCert, RevocationEntry};

/// The version both sides must speak. Two devices exchange frames only when
/// theirs are equal.
pub const PROTOCOL_VERSION: u32 = 1;

/// One request of a member.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum StoreRequest {
    /// First, once: who the connecting device is.
    Hello {
        /// The sender's [`PROTOCOL_VERSION`].
        protocol: u32,
        /// The device's certificate, whose device key is the connection's
        /// endpoint id.
        cert: Box<DeviceCert>,
        /// Root-signed revocations the sender knows of.
        revocations: Vec<RevocationEntry>,
    },
    /// The frames of `workspace` after these cursors.
    Want {
        /// The workspace to replay.
        workspace: u64,
        /// Where the sender stands on each lineage it follows; lineages it
        /// omits are replayed from their first retained frame.
        cursors: Vec<Cursor>,
    },
    /// Frames to keep for the workspace's other members.
    Push {
        /// The workspace the frames belong to.
        workspace: u64,
        /// Each frame as an encoded
        /// [`NeighbourMsg`](neighbour_wire::NeighbourMsg).
        frames: Vec<Vec<u8>>,
    },
}

/// One reply of a store.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum StoreReply {
    /// The sender is admitted and speaks this version.
    Welcome {
        /// The store's [`PROTOCOL_VERSION`].
        protocol: u32,
    },
    /// The store will not answer this connection or request.
    Refused {
        /// Why, for the sender's log.
        reason: String,
    },
    /// Frames the sender lacked, and where the store has nothing.
    Frames {
        /// Each frame as an encoded
        /// [`NeighbourMsg`](neighbour_wire::NeighbourMsg), in storage order.
        frames: Vec<Vec<u8>>,
        /// Lineages the store cannot continue from the cursors it was given.
        floors: Vec<Floor>,
        /// Whether frames are left for a further [`StoreRequest::Want`].
        more: bool,
    },
    /// What keeping the pushed frames did.
    Acked {
        /// How many the store now holds.
        accepted: u32,
        /// The frames it refused, with why.
        refused: Vec<String>,
    },
}
