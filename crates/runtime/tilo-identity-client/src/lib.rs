#![allow(
    clippy::missing_errors_doc,
    clippy::significant_drop_tightening,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Identity-service-backed signing on an owned, connected byte stream:
//! [`BrokerSigningIdentity`] asks the workspace identity service for public
//! metadata and signatures and never receives private key material.
//!
//! The caller supplies the connection to its trusted host, which this crate
//! neither discovers nor authenticates. The inherited service descriptor is
//! the only capability it takes from the environment.

mod public_metadata;
pub use public_metadata::{IdentitySummary, KeyProtection};

mod profile;
pub use profile::{MAX_PHOTO_BYTES, MAX_PHOTO_DIMENSION, ProfilePhoto};

mod client;
pub use client::{BrokerMetadata, BrokerSigningIdentity};
mod async_client;
pub use async_client::AsyncBrokerSigningIdentity;
pub use runtime_portal::NotAvailable;

#[cfg(unix)]
mod public_record;
#[cfg(unix)]
pub use public_record::{load_metadata_from_bytes, load_metadata_from_path};

#[cfg(unix)]
pub mod inherited;
#[cfg(unix)]
pub use inherited::{load, load_bound_metadata, load_metadata};

#[cfg(not(unix))]
pub fn load() -> Result<std::sync::Arc<BrokerSigningIdentity>, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

#[cfg(not(unix))]
pub fn load_metadata() -> Result<IdentityMetadata, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

#[cfg(not(unix))]
pub use load_metadata as load_bound_metadata;

#[cfg(not(unix))]
pub fn load_metadata_from_path(_path: &std::path::Path) -> Result<IdentityMetadata, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

/// Public fields of an identity record: reading them never unlocks the account
/// signing share, which app-facing brokers never see either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityMetadata {
    pub profile_photo: Option<ProfilePhoto>,
    pub display_name: String,
    pub avatar_color: [u8; 3],
    pub group_public_key: [u8; 32],
    pub threshold: u16,
    pub total_shares: u16,
    pub share_index: u16,
    pub single_device: bool,
}

/// Non-secret facts about a portable backup that was written and verified.
///
/// The identity store writes backups only where it holds the person's
/// identity; its callers speak of the result by this type everywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableBackupInfo {
    pub group_public_key: [u8; 32],
    /// Exact number of envelope bytes published at the destination.
    pub bytes_written: u64,
}

/// The avatar from the workspace identity service only: no config record,
/// account name or signing share is read, and an app without a bound
/// workspace shows none.
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
const COLLABORATION_JOIN_TAG: &[u8] = b"tilo-collaboration-join-v2\0";

/// The digest an enrolled device signs when it joins a workspace: the BLAKE3
/// digest of the join's domain label and the workspace id. This is fixed wire
/// ABI for peer verification.
///
/// # Errors
///
/// [`tilo_user_identity::canonical::CanonicalError::Serialize`] when the join
/// cannot be encoded (effectively unreachable for a workspace id).
pub fn collaboration_join_digest(
    workspace_id: &str,
) -> Result<[u8; 32], tilo_user_identity::canonical::CanonicalError> {
    #[derive(serde::Serialize)]
    struct View<'a> {
        workspace: &'a str,
    }
    tilo_user_identity::canonical::signing_digest(
        COLLABORATION_JOIN_TAG,
        &View {
            workspace: workspace_id,
        },
    )
}

/// The synchronous wire protocol over a connected identity stream. The
/// launcher writes `Handshake` before the sandbox starts, so an app cannot
/// claim another package or workspace.
pub mod wire {
    use serde::{Deserialize, Serialize};
    use wire_frame::Framing;

    pub const NO_IDENTITY_SELECTED: &str = "no identity selected for this workspace";

    /// The launcher's [`Handshake`]: a little-endian `u32` length, then JSON.
    pub const HANDSHAKE_FRAMING: Framing = Framing::little_endian(1024 * 1024);

    /// Every [`Request`] and [`Response`] after the handshake: a little-endian
    /// `u32` length, then postcard, whose byte vectors are compact enough for a
    /// real document snapshot.
    pub const FRAMING: Framing = Framing::little_endian(64 * 1024 * 1024);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Handshake {
        pub workspace_id: String,
        pub app_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub surface: Option<String>,
        /// The one call this connection serves, when the launcher opened it for one: every
        /// request on it is decided as whoever made that call, and it names no other.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub call: Option<[u8; 16]>,
    }

    /// Additive JSON negotiation: zero serializes exactly the older handshake, and
    /// a host that knows no presentation ignores the field.
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
    pub const PRESENTATION_SERVICE_VERSION: u32 = runtime_portal::RUNTIME_INTERFACE_VERSION;

    #[derive(Debug, Serialize, Deserialize)]
    pub enum Request {
        Metadata,
        CollaborationJoin {
            workspace_id: String,
        },
        /// Sign a pre-constructed `collab_types::SignedOp`, once the workspace-bound
        /// broker has verified its route, device author, payload kind, policy class,
        /// timestamp and the caller's live role.
        CollaborationOp {
            workspace_id: String,
            app_id: String,
            document_id: String,
            unsigned_op: Vec<u8>,
        },
        /// The content keys of one document route of the bound workspace and app, once
        /// this device is admitted to its encrypted channel. `known_generation` is the
        /// generation the caller already holds, which answers without keys when
        /// unchanged. Appended, so earlier requests keep their encoding.
        GroupKeys {
            workspace_id: String,
            app_id: String,
            document_id: String,
            known_generation: u64,
        },
        /// `request`, made while the app serves the invocation `invocation`: decided as the one
        /// who made that call, where a device runs apps for the people who call them. Appended,
        /// so earlier requests keep their encoding.
        ForCall {
            invocation: [u8; 16],
            request: Box<Self>,
        },
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum KeyAccess {
        Ready,
        /// Asked the channel's key holders for access and not admitted yet.
        Waiting,
        Removed,
        /// The channel cannot be provisioned on this device right now.
        Unavailable,
        /// This device has no secret store open; only the workspaces whose host it
        /// takes part in have keys without one. Appended, so earlier answers keep
        /// their encoding.
        NoSecretStore,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub enum Response {
        Metadata {
            display_name: String,
            avatar_color: [u8; 3],
            group_public_key: [u8; 32],
            signing_public_key: [u8; 32],
            device_certificate: Option<Vec<u8>>,
            threshold: u16,
            total_shares: u16,
            share_index: u16,
        },
        Signature(Vec<u8>),
        Denied(String),
        Error(String),
        /// Answer to [`Request::GroupKeys`]. `keys` holds the encoded
        /// content keys when `access` is ready and `generation` differs
        /// from the caller's; it is empty otherwise. Appended so earlier
        /// responses keep their encoding.
        GroupKeys {
            access: KeyAccess,
            generation: u64,
            keys: Vec<u8>,
        },
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

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ChannelKeysReply {
        pub access: KeyAccess,
        pub generation: u64,
        /// Encoded content keys; `None` when unchanged or not ready.
        pub keys: Option<Vec<u8>>,
    }
}

#[cfg(test)]
#[cfg(unix)]
mod inherited_tests;

#[cfg(test)]
#[cfg(unix)]
mod presentation_wire_tests {
    use super::wire::*;

    tilo_test_env::scenario! {
        fn the_presentation_wire;

        phase presentation_negotiation_preserves_legacy_handshake_and_old_host_decode;
        {
            let legacy = Handshake {
                workspace_id: "w1".into(),
                app_id: "app".into(),
                surface: None,
                call: None,
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
            assert_eq!(old_host.workspace_id, "w1");
            assert_eq!(old_host.app_id, "app");
            let new_host: PresentationHandshake = serde_json::from_slice(&legacy_bytes).unwrap();
            assert_eq!(new_host.presentation_version, 0);
            assert_eq!(postcard::to_allocvec(&Request::Metadata).unwrap(), [0]);
            assert_eq!(
                postcard::to_allocvec(&Response::Signature(vec![1, 2])).unwrap(),
                [1, 2, 1, 2]
            );
        }

        phase negotiated_response_keeps_legacy_metadata_tuple_encoding;
        {
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
}
