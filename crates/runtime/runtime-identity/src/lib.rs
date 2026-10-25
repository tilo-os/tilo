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

mod profile;
pub use profile::{MAX_PHOTO_BYTES, MAX_PHOTO_DIMENSION, ProfilePhoto};

mod client;
#[cfg(unix)]
pub mod connected;
pub use client::{BrokerMetadata, BrokerSigningIdentity};
mod async_client;
pub use async_client::AsyncBrokerSigningIdentity;
pub use runtime_portal::NotAvailable;

/// Why the identity service refused: the workspace the app was launched into
/// has no identity selected yet. The service says so in
/// [`wire::NO_IDENTITY_SELECTED`]; a client turns that refusal into this
/// error (inside [`NotAvailable::ConnectionFailed`], kind permission denied),
/// so a caller tells it apart from every other refusal with
/// [`is_no_identity_selected`] rather than by reading the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoIdentitySelected;

impl std::fmt::Display for NoIdentitySelected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(wire::NO_IDENTITY_SELECTED)
    }
}

impl std::error::Error for NoIdentitySelected {}

/// Whether `error` is the identity service saying the workspace has no
/// identity selected ([`NoIdentitySelected`]): someone has to choose one
/// before the app can join its workspace. Every other failure reads `false`.
#[must_use]
pub fn is_no_identity_selected(error: &NotAvailable) -> bool {
    matches!(
        error,
        NotAvailable::ConnectionFailed(error)
            if error.get_ref().is_some_and(<dyn std::error::Error + Send + Sync>::is::<NoIdentitySelected>)
    )
}

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

/// Outside Unix no identity service is handed to an app.
///
/// # Errors
///
/// Always [`NotAvailable::ServiceFdMissing`].
#[cfg(not(unix))]
pub fn load_metadata() -> Result<IdentityMetadata, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

/// Outside Unix no identity record is kept on disk.
///
/// # Errors
///
/// Always [`NotAvailable::ServiceFdMissing`].
#[cfg(not(unix))]
pub fn load_metadata_from_path(_path: &std::path::Path) -> Result<IdentityMetadata, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

/// Outside Unix no identity record is kept on disk.
#[cfg(not(unix))]
pub fn resolve_active_identity(_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    None
}

/// Public fields from an identity record. Reading these never unlocks or
/// materializes the account signing share; app-facing brokers use the
/// certified device key for signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityMetadata {
    pub profile_photo: Option<ProfilePhoto>,
    pub display_name: String,
    pub avatar_color: [u8; 3],
    pub group_public_key: [u8; 32],
    pub threshold: u16,
    pub total_shares: u16,
    pub share_index: u16,
    /// Whether the record is a 1-of-1 identity holding a local share.
    pub single_device: bool,
}

/// Public avatar from the launcher's authenticated identity broker only.
///
/// Never reads a config record, resolves an account name, or unlocks a signing
/// share. Hosts without an inherited broker return no presentation.
#[must_use]
pub fn bound_profile_photo() -> Option<([u8; 32], ProfilePhoto)> {
    #[cfg(unix)]
    {
        let metadata = load_bound_metadata().ok()?;
        Some((metadata.group_public_key, metadata.profile_photo?))
    }
    #[cfg(not(unix))]
    {
        None
    }
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

    /// What the service refuses with while the workspace the connection is
    /// bound to has no identity selected (see [`crate::NoIdentitySelected`]).
    pub const NO_IDENTITY_SELECTED: &str = "no identity selected for this workspace";

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
        /// The surface file the app was started with (`TILO_SURFACE`, its
        /// hash checked): what a development host takes as the surface of
        /// an app it holds no package of.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub surface: Option<String>,
    }

    /// Additive JSON negotiation; zero serializes exactly the legacy handshake.
    /// Old hosts ignore the optional field and return legacy Metadata.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct PresentationHandshake {
        #[serde(flatten)]
        pub handshake: Handshake,
        #[serde(default, skip_serializing_if = "presentation_version_is_zero")]
        pub presentation_version: u16,
    }
    fn presentation_version_is_zero(version: impl std::borrow::Borrow<u16>) -> bool {
        is_zero(*version.borrow())
    }
    fn is_zero(version: u16) -> bool {
        version == 0
    }
    pub const PRESENTATION_VERSION: u16 = 1;
    /// The identity service version that understands presentation: the
    /// runtime interface's (`runtime_portal::RUNTIME_INTERFACE_VERSION`),
    /// every session service speaking the runtime's one version
    /// (docs/architecture/compatibility.md).
    pub const PRESENTATION_SERVICE_VERSION: u32 = runtime_portal::RUNTIME_INTERFACE_VERSION;

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
        Removed,
        /// The channel cannot be provisioned on this device right now.
        Unavailable,
        /// This device has no secret store open, and only rooms its own
        /// room host stewards have keys without one. Appended so earlier
        /// answers keep their encoding.
        NoSecretStore,
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
        /// Negotiated public presentation; appended to preserve all old encodings.
        MetadataWithPresentation {
            display_name: String,
            avatar_color: [u8; 3],
            group_public_key: [u8; 32],
            signing_public_key: [u8; 32],
            device_certificate: Option<Vec<u8>>,
            threshold: u16,
            total_shares: u16,
            share_index: u16,
            photo: Option<crate::ProfilePhoto>,
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

#[cfg(all(test, unix))]
mod presentation_wire_tests {
    use super::wire::*;

    #[test]
    fn presentation_negotiation_preserves_legacy_handshake_and_old_host_decode() {
        let legacy = Handshake {
            workspace_id: "room".into(),
            app_id: "app".into(),
            surface: None,
        };
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        let compatibility = PresentationHandshake {
            handshake: legacy,
            presentation_version: 0,
        };
        assert_eq!(serde_json::to_vec(&compatibility).unwrap(), legacy_bytes);
        let enabled = PresentationHandshake {
            handshake: compatibility.handshake,
            presentation_version: PRESENTATION_VERSION,
        };
        let old_host: Handshake =
            serde_json::from_slice(&serde_json::to_vec(&enabled).unwrap()).unwrap();
        assert_eq!(old_host.workspace_id, "room");
        assert_eq!(old_host.app_id, "app");
        let new_host: PresentationHandshake = serde_json::from_slice(&legacy_bytes).unwrap();
        assert_eq!(new_host.presentation_version, 0);
        assert_eq!(postcard::to_allocvec(&Request::Metadata).unwrap(), [0]);
        assert_eq!(
            postcard::to_allocvec(&Response::Signature(vec![1, 2])).unwrap(),
            [1, 2, 1, 2]
        );
    }
    #[test]
    fn negotiated_response_keeps_legacy_metadata_tuple_encoding() {
        let fields = (
            "Ada",
            [1_u8, 2, 3],
            [4_u8; 32],
            [5_u8; 32],
            Some(vec![6_u8; 4]),
            1_u16,
            1_u16,
            0_u16,
        );
        let expected = [vec![0], postcard::to_allocvec(&fields).unwrap()].concat();
        let legacy = Response::Metadata {
            display_name: fields.0.into(),
            avatar_color: fields.1,
            group_public_key: fields.2,
            signing_public_key: fields.3,
            device_certificate: fields.4.clone(),
            threshold: fields.5,
            total_shares: fields.6,
            share_index: fields.7,
        };
        assert_eq!(postcard::to_allocvec(&legacy).unwrap(), expected);
        let extended = Response::MetadataWithPresentation {
            display_name: fields.0.into(),
            avatar_color: fields.1,
            group_public_key: fields.2,
            signing_public_key: fields.3,
            device_certificate: fields.4,
            threshold: fields.5,
            total_shares: fields.6,
            share_index: fields.7,
            photo: None,
        };
        assert_eq!(postcard::to_allocvec(&extended).unwrap().first(), Some(&5));
    }
}
