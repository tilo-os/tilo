//! `fs-protocol` — the collaborative filesystem's wire records and its local
//! control protocols.
//!
//! Pure types and pure functions, no I/O. Shared by the filesystem daemon
//! (`tilo-fs`), the Files app, the toolkit's history and conflict widgets and
//! the launcher, none of which then link one another.
//!
//! - [`files`] — stable file records: [`FileRecordId`], versions, bindings,
//!   content revisions, conflicts, history and merge-preflight projections, and
//!   the signed [`FileMutationTransaction`].
//! - [`file_invite`] — signed, record-scoped file invitations
//!   ([`FileInviteV1`], [`BearerFileInvite`]).
//! - [`share_control`] — the bounded local share-control socket protocol.
//! - [`recipient_control`] — the local protocol for admitting and observing
//!   received shares.
//!
//! Every signed payload canonicalises and signs through `collab-types`
//! ([`collab_types::canonical`], [`collab_types::crypto`]); the dependency
//! points one way, from here to the collaboration core.

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

pub mod file_invite;
pub mod files;
pub mod recipient_control;
pub mod share_control;

pub use file_invite::{
    BearerFileInvite, FILE_INVITE_DOMAIN_TAG, FILE_INVITE_LINK_PREFIX, FileInviteError,
    FileInviteSpec, FileInviteSubject, FileInviteV1, FilePermission, bearer_secret_hash,
    mint_bearer_file_invite, mint_file_invite,
};
pub use files::{
    CONTENT_REVISION_VERSION, ConflictMergeCodecV1, ConflictResolution, ConflictStatus,
    ConflictStrategy, ContentHash, ContentRevisionV1, FILE_BINDING_VERSION,
    FILE_MUTATION_TRANSACTION_SIGNATURE_DOMAIN, FILE_MUTATION_TRANSACTION_VERSION, FileActor,
    FileBindingV1, FileConflictV1, FileMetadataV1, FileMutationOperation, FileMutationParticipant,
    FileMutationParticipantRole, FileMutationTransaction, FileMutationTransactionError,
    FileMutationTransactionManifest, FileNameV1, FileNodeKind, FilePropertiesV1, FileProvenance,
    FileRecordError, FileRecordId, FileVersionId, FileVersionV1, HISTORY_CONTROL_VERSION,
    HISTORY_PREVIEW_XATTR_PREFIX, HistoryControlOperationV1, HistoryControlRequestV1, IndexStatus,
    MAX_FILE_NAME_BYTES, MAX_HISTORY_CONTROL_BYTES, MAX_HISTORY_PREVIEW_BYTES,
    MAX_MERGE_PREFLIGHT_BYTES, MAX_TRANSACTION_PARTICIPANTS, MERGE_PREFLIGHT_VERSION,
    MERGE_PREFLIGHT_XATTR_PREFIX, MergePreflightProjectionV1, PreviewStatus, PreviewVariant,
    prefer_conflict_resolution,
};
#[allow(
    deprecated,
    reason = "stable source compatibility for the moved collab-types API"
)]
pub use files::{
    FileMutationOperationV1, FileMutationParticipantRoleV1, FileMutationParticipantV1,
    FileMutationTransactionManifestV1, FileMutationTransactionV1,
};
pub use recipient_control::{
    MAX_RECIPIENT_CONTROL_CONNECTIONS, MAX_RECIPIENT_CONTROL_FRAME_BYTES,
    MAX_RECIPIENT_CONTROL_REQUESTS_PER_CONNECTION, MAX_RECIPIENT_MOUNT_PATH_BYTES,
    RECIPIENT_CONTROL_FRAMING, RECIPIENT_CONTROL_VERSION, RecipientControlErrorCode,
    RecipientControlOperationV1, RecipientControlRequestV1, RecipientControlResponseV1,
    RecipientControlResultV1, RecipientFailureCodeV1, RecipientFailureV1, RecipientMountStatusV1,
    RecipientSubscriptionKeyV1, RecipientSubscriptionStatusV1,
};
pub use share_control::{
    MAX_SHARE_CONTROL_FRAME_BYTES, NamedGrantMetadataV1, NamedShareDeliveryStateV1,
    SHARE_CONTROL_FRAMING, SHARE_CONTROL_VERSION, ShareControlErrorCode, ShareControlOperationV1,
    ShareControlRequestV1, ShareControlResponseV1, ShareControlResultV1,
};
