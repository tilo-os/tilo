//! `fs-protocol` — the collaborative filesystem's wire records and its local
//! control protocols.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod file_action;
pub mod file_invite;
pub mod files;
pub mod projection;
pub mod tree;

/// The `collab-types` vocabulary these wire types are expressed in: the key
/// type they name and the canonical encoding every control frame and signed
/// record uses. A client of the filesystem protocols needs nothing else from
/// the collaboration core.
pub use collab_types::{Ed25519PublicKey, canonical};

pub use file_action::FileAction;
pub use file_invite::{
    BearerFileInvite, FILE_INVITE_DOMAIN_TAG, FILE_INVITE_LINK_PREFIX, FileInviteError,
    FileInviteSpec, FileInviteSubject, FileInviteV1, FilePermission, bearer_secret_hash,
    mint_bearer_file_invite, mint_file_invite,
};
pub use files::{
    ContentHash, FILE_BINDING_VERSION, FIRST_TREE_CHUNK, FileActor, FileBindingV1,
    FileDocumentError, FileDocumentId, FileNameV1, FileNodeKind, FilePropertiesV1,
    MAX_FILE_NAME_BYTES, TreeChunk,
};
pub use projection::{
    AVAILABILITY_CONTROL_XATTR, AVAILABILITY_XATTR, AvailabilityPolicy, AvailabilityProjectionV1,
    CONTENT_HASH_XATTR, METADATA_XATTR, MetadataProjectionV1, NO_PEER_DELIVERED_ERROR, PIN_XATTR,
    ProjectedFreshness, ProjectedReplicaEvidence, ProjectedTransfer, ProjectionError, Residency,
    SYNC_XATTR, SyncState, THUMB_XATTR, TransferPhase, TransferProgress,
};
pub use tree::{FilesTree, Tree, TreeChunks, TreeEntry};
