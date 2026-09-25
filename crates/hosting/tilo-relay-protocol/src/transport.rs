//! Wire messages exchanged between a peer and the relay after the
//! [`auth`](crate::auth) handshake.
//!
//! The framing that carries them lives with the stream I/O in
//! `tilo-relay-client`'s `frame` module; this module only defines the values.
//!
//! The relay uses **one long-lived bidirectional stream per
//! connection**. After the handshake, the stream is a sequence of
//! length-prefixed postcard-encoded [`Envelope`] messages:
//!
//! - The relay is not a general-purpose RPC service — its message
//!   shape is a small closed enum.
//! - Length-prefixed framing lets the reader skip malformed messages
//!   without desynchronising the stream.
//! - Postcard keeps the encoding compact + deterministic (no field-
//!   reorder) so integration tests can byte-compare fixtures.

use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};

/// ALPN identifier for the relay protocol. Frozen wire ABI —
/// bumping this string means every already-deployed relay + client
/// stops talking to the new version. Change only via a v2 side-by-
/// side rollout.
pub const ALPN: &[u8] = b"tilo-relay/1";
/// Authenticated one-shot discovery of direct filesystem endpoints.
pub const DISCOVERY_ALPN: &[u8] = b"tilo-relay-discovery/2";
/// Authenticated owner-only health and lifecycle management protocol.
pub const MANAGEMENT_ALPN: &[u8] = b"tilo-home-peer-management/1";

/// Small, stable discovery reply consumed directly by filesystem workers.
/// Keeping this separate from the relay's richer placement profile avoids a
/// relay-only address ever being mistaken for a content endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEndpointAdvertisement {
    /// Explicit schema version for persisted/client registration envelopes.
    pub schema_version: u16,
    /// Relay room whose signed filesystem authority authorised this lookup.
    pub workspace_id: u64,
    /// Direct authenticated filesystem/blob endpoints currently ready for it.
    pub filesystem_endpoints: Vec<EndpointAddr>,
    /// Authenticated Files workspace binding used to join the record plane.
    pub filesystem_ticket: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Complete authenticated registration handed to the local Files daemon.
pub struct ContentPeerRegistration {
    /// Direct Iroh endpoint serving filesystem records and blobs.
    pub endpoint: EndpointAddr,
    /// Encoded Files ticket binding a fresh daemon to the shared namespace.
    pub ticket: String,
    /// Relay service that authenticated this registration.  This binds a
    /// persisted Files endpoint to the enrolled home peer which nominated it,
    /// so removing that peer can revoke its hydration route too.
    pub relay_service_id: [u8; 32],
}

/// Bounded owner operation for an enrolled home peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomePeerManagementRequest {
    /// Read current service and full-replica health.
    Status,
    /// Change the operator-facing server name.
    Rename {
        /// New operator-facing label, bounded to 80 bytes.
        label: String,
    },
    /// Revoke this host and stop all hosted services.
    Decommission {
        /// Root-signed permanent revocation targeting this host certificate.
        revocation: tilo_user_identity::RevocationEntry,
    },
}

/// Authenticated result returned by a home peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomePeerManagementResponse {
    /// Certified host device key.
    pub device_public_key: [u8; 32],
    /// Current network-addressable relay service endpoint. The client has
    /// authenticated the certified host before accepting this advertisement.
    pub relay_endpoint: iroh_base::EndpointAddr,
    /// Current operator-facing label.
    pub label: String,
    /// Whether the full filesystem replica currently proves readiness.
    pub files_ready: bool,
    /// Active service advertisement after capability clamping.
    pub services: crate::mesh::ServiceCapabilities,
    /// Whether this response completed permanent decommissioning.
    pub decommissioned: bool,
}

/// A client must name the relay room it is resolving. The host never exposes
/// its filesystem endpoints as a global, unauthorised discovery answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEndpointRequest {
    /// Relay room whose filesystem endpoint the caller wants to resolve.
    pub workspace_id: u64,
}

/// Hard cap for a single framed envelope on the wire. Guards
/// against a peer that sends `u32::MAX` as a length prefix to
/// exhaust the reader's memory. 64 MiB is a very generous ceiling
/// for a CRDT delta.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// One message on the post-handshake stream.
///
/// Encoded via postcard's default enum representation: a compact
/// varint discriminant + the variant payload. Externally-tagged
/// (JSON-style) representations are not supported by postcard, so
/// we deliberately do not use `#[serde(tag = ...)]` — the wire form
/// is fully binary anyway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Envelope {
    /// Peer asks to receive updates for `workspace_id`. The relay
    /// registers the peer as a subscriber and immediately drains
    /// any pending deltas queued for it (store-and-forward).
    Subscribe {
        /// Workspace id.
        workspace_id: u64,
    },

    /// Peer stops receiving updates for `workspace_id`. The
    /// subscription is removed; any deltas received while
    /// unsubscribed are queued for the next Subscribe.
    Unsubscribe {
        /// Workspace id.
        workspace_id: u64,
    },

    /// A workspace-directory CRDT delta. The relay:
    /// 1. Applies it to its own pinned copy (if pinned).
    /// 2. Fans it out to every other peer currently subscribed.
    /// 3. Queues it for every other member device that is offline.
    Delta {
        /// Workspace id.
        workspace_id: u64,
        /// Author device pubkey (redundant with the peer's cert on
        /// this connection but included so a fan-out recipient can
        /// dedup / attribute without maintaining connection state).
        author_device_public_key: [u8; 32],
        /// Authored-at nanoseconds since epoch.
        authored_at_ns: u64,
        /// Automerge incremental delta bytes.
        #[serde(with = "serde_bytes")]
        delta: Vec<u8>,
        /// Author's signature over the delta — the same
        /// `DirectoryUpdate.signature` shape the identity crate
        /// produces.
        #[serde(with = "crate::sig64")]
        signature: [u8; 64],
    },

    /// A presence event — cheap ephemeral broadcast (cursor
    /// positions, "user is here" markers, typing indicators). Not
    /// persisted; if no subscribers exist, the relay drops it.
    Presence {
        /// Workspace id.
        workspace_id: u64,
        /// Peer-defined presence payload. The relay never inspects
        /// the bytes.
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },

    /// The relay's response to a `Subscribe`: how many pending
    /// deltas the relay is about to flush from the peer's
    /// store-and-forward queue.
    Ready {
        /// Workspace id.
        workspace_id: u64,
        /// Number of pending deltas queued for this peer that the
        /// relay is about to flush.
        pending_deltas: u32,
    },

    /// The relay's response to a subscribe that failed — e.g.
    /// workspace not bound here, peer not a member.
    NotAuthorised {
        /// Workspace id.
        workspace_id: u64,
        /// Human-readable reason (for tracing / logging).
        reason: String,
    },

    /// Ping/keep-alive. Not required but useful for the peer to
    /// detect a dead connection quickly.
    Ping,
    /// Response to Ping.
    Pong,

    /// Sent by a peer relay immediately after auth — advertises the
    /// set of workspaces it holds. The receiver computes the
    /// intersection with its own hosted set and treats the sender as
    /// a federated peer for that intersection.
    ///
    /// Sent by BOTH sides of a peer-relay session: the dialer sends
    /// first, the accepter replies with its own hosted set. Only
    /// meaningful when the sender's cert has
    /// `capabilities.can_act_as_host = true` — otherwise the receiver
    /// ignores it.
    PeerRelayHello {
        /// Workspace ids this peer relay currently hosts.
        hosted_workspaces: Vec<u64>,
    },

    /// Anti-entropy request. The sender advertises the Automerge heads
    /// it currently holds for `workspace_id`; the host replies with a
    /// [`Envelope::SyncResponse`] carrying every change the sender is
    /// missing, computed against the host's *materialized* pinned doc.
    ///
    /// This is the recovery tier that sits underneath the per-peer
    /// store-and-forward queue: a delta lost in a genuine disconnect
    /// race (fanned out to a live subscriber whose wire died before the
    /// write landed, so it was neither delivered nor re-queued) leaves
    /// that peer permanently behind. Reconciling against the host's
    /// authoritative doc converges the peer regardless of what the
    /// queue delivered — run on subscribe, periodically, or on demand.
    SyncRequest {
        /// Workspace id.
        workspace_id: u64,
        /// Automerge change hashes the requester already has (its
        /// `get_heads()`), each a 32-byte hash. An empty vec means "I
        /// hold nothing — send me the whole document."
        have_heads: Vec<[u8; 32]>,
    },

    /// The host's reply to a [`Envelope::SyncRequest`]: the changes the
    /// requester was missing, as a concatenated series of Automerge
    /// changes loadable with `load_incremental`. Empty when the
    /// requester is already up to date, or the workspace is not
    /// materialized on this host (unpinned).
    SyncResponse {
        /// Workspace id.
        workspace_id: u64,
        /// Concatenated Automerge changes the requester was missing.
        #[serde(with = "serde_bytes")]
        delta_since: Vec<u8>,
    },

    /// A remote-action frame (`tilo_actions::remote::RemoteActionFrame`
    /// bytes) addressed to one device of the workspace.
    ///
    /// The relay routes it to the addressed device's live connection and
    /// never stores, replays or executes it: an action that ran late would
    /// be worse than one that did not run. Only when the addressed device is
    /// the relay itself *and* an app-serving plane was supplied does the
    /// relay answer it, through that plane. Anything else comes back as
    /// [`Envelope::RemoteActionUndeliverable`].
    RemoteAction {
        /// Workspace id both devices are subscribed to.
        workspace_id: u64,
        /// Version of this envelope kind, see [`REMOTE_ACTION_ENVELOPE_VERSION`].
        version: u16,
        /// The addressed device.
        to_device_public_key: [u8; 32],
        /// The sending device, stamped by the relay from the authenticated
        /// connection. Whatever a sender puts here is overwritten.
        from_device_public_key: [u8; 32],
        /// The user root the sending device chains to, stamped the same way.
        from_user_public_key: [u8; 32],
        /// Opaque remote-action frame.
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// The relay could not deliver a [`Envelope::RemoteAction`]: the
    /// addressed device is not connected to this workspace, the envelope
    /// version is unknown, or nothing here serves apps.
    RemoteActionUndeliverable {
        /// Workspace id of the undelivered envelope.
        workspace_id: u64,
        /// The device it was addressed to.
        to_device_public_key: [u8; 32],
        /// Human-readable reason (for tracing / logging).
        reason: String,
    },
}

/// Version of the [`Envelope::RemoteAction`] kind this build routes. The
/// relay refuses other versions as undeliverable instead of forwarding
/// frames whose routing fields it may misread.
pub const REMOTE_ACTION_ENVELOPE_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpn_is_stable() {
        assert_eq!(ALPN, b"tilo-relay/1");
    }
}
