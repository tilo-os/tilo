#![allow(
    clippy::missing_errors_doc,
    clippy::significant_drop_tightening,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Broker-backed identity signing on an owned, connected byte stream.
//!
//! [`BrokerSigningIdentity`] requests public metadata and signatures without
//! receiving private key material. The caller must supply a connection to its
//! trusted host; this crate does not discover or authenticate that host.
//! Unix also retains its inherited-descriptor and host-local lifecycle APIs.

mod client;
#[cfg(unix)]
pub mod connected;
pub use client::{BrokerMetadata, BrokerSigningIdentity};
mod async_client;
pub use async_client::AsyncBrokerSigningIdentity;
pub use runtime_portal::NotAvailable;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

/// Outside Unix (the browser) no identity service is ever handed to an app,
/// so there is nothing to load; callers fall back as they do outside Tilo.
///
/// # Errors
///
/// Always [`NotAvailable::ServiceFdMissing`].
#[cfg(not(unix))]
pub fn load() -> Result<BrokerSigningIdentity, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

pub const IDENTITY_SERVICE: &str = "identity";
const COLLABORATION_JOIN_TAG: &[u8] = b"tilo-collaboration-join-v1\0";

/// Domain-separated bytes signed by an enrolled device when joining one
/// collaboration workspace. The NUL-terminated domain tag and the remaining
/// workspace bytes make the single-field encoding unambiguous; this exact
/// concatenation is fixed wire ABI for peer verification.
#[must_use]
pub fn collaboration_join_signing_bytes(workspace_id: &str) -> Vec<u8> {
    let workspace = workspace_id.as_bytes();
    let mut message = Vec::new();
    message.extend_from_slice(COLLABORATION_JOIN_TAG);
    message.extend_from_slice(workspace);
    message
}

/// Small synchronous wire protocol used over a connected identity stream. The launcher writes `Handshake` before the sandbox starts, so
/// an app cannot claim another package or workspace.
pub mod wire {
    use serde::{Deserialize, Serialize};
    use wire_frame::Framing;

    /// The launcher's [`Handshake`]: a little-endian `u32` length, then JSON.
    pub const HANDSHAKE_FRAMING: Framing = Framing::little_endian(1024 * 1024);

    /// Every [`Request`] and [`Response`] after the handshake: a little-endian
    /// `u32` length, then postcard.
    ///
    /// The bound admits real document snapshots in app-controlled traffic.
    /// Postcard keeps byte vectors compact; the preceding JSON wire
    /// represented every byte as a decimal number and hit its 1 MiB cap much
    /// earlier than the underlying document did.
    pub const FRAMING: Framing = Framing::little_endian(64 * 1024 * 1024);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Handshake {
        pub workspace_id: String,
        pub app_id: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub enum Request {
        Metadata,
        Sign {
            action: String,
            message: Vec<u8>,
        },
        /// Prove possession of the enrolled device key for the workspace the
        /// launcher bound into the handshake. The broker constructs the
        /// domain-separated message; callers cannot use this as a generic
        /// signing escape hatch.
        CollaborationJoin {
            workspace_id: String,
        },
        /// Sign a pre-constructed `collab_types::SignedOp` only after the
        /// workspace-bound broker verifies its route, certified device author,
        /// document payload kind, policy class, timestamp, and live role.
        CollaborationOp {
            workspace_id: String,
            app_id: String,
            record_id: String,
            unsigned_op: Vec<u8>,
        },
        /// The content keys of one document route of the bound workspace
        /// and app, once this device has been admitted to its encrypted
        /// channel. `known_generation` is the generation of the keys the
        /// caller already holds; an unchanged state answers without keys.
        /// Appended so earlier requests keep their encoding.
        ChannelKeys {
            workspace_id: String,
            app_id: String,
            record_id: String,
            known_generation: u64,
        },
    }

    /// What the device may do with a route's content, as the identity
    /// service reports it to the route's endpoints.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ChannelAccess {
        /// Admitted; the response carries the channel's content keys.
        Ready,
        /// Asked the channel's steward for access and not admitted yet.
        Waiting,
        /// This device was removed from the channel.
        Removed,
        /// The channel cannot be provisioned on this device right now.
        Unavailable,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub enum Response {
        Metadata {
            display_name: String,
            avatar_color: [u8; 3],
            group_public_key: [u8; 32],
            signing_public_key: [u8; 32],
            /// JSON-encoded root-signed `tilo_user_identity::DeviceCert`.
            device_certificate: Option<Vec<u8>>,
            threshold: u16,
            total_shares: u16,
            share_index: u16,
        },
        Signature(Vec<u8>),
        Denied(String),
        Error(String),
        /// Answer to [`Request::ChannelKeys`]. `keys` holds the encoded
        /// content keys when `access` is ready and `generation` differs
        /// from the caller's; it is empty otherwise. Appended so earlier
        /// responses keep their encoding.
        ChannelKeys {
            access: ChannelAccess,
            generation: u64,
            keys: Vec<u8>,
        },
    }

    /// A decoded [`Response::ChannelKeys`].
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ChannelKeysReply {
        pub access: ChannelAccess,
        pub generation: u64,
        /// Encoded content keys; `None` when unchanged or not ready.
        pub keys: Option<Vec<u8>>,
    }
}
