#![allow(
    clippy::large_enum_variant,
    reason = "migration stack lint debt is scoped to this target"
)]

//! `collab-types` — the cryptographic foundation for Tilo Spaces,
//! Channels, and role-based access control.
//!
//! Self-contained: pure types, pure functions, no I/O, no async runtime,
//! no compositor or UI dependencies. Signed-roles wire enforcement, MLS
//! encryption, and threshold identity all layer on the primitives here.
//!
//! - [`identity`] — `Identity`, `Attestation`, `Claim`, `Clearance`: key
//!   material and signed claims identities carry across spaces.
//! - [`space`] — `Space`, the social-formation container grouping channels.
//! - [`channel`] — `Channel`, `MemberRef`, `ChannelRef`, `ArtifactRef`,
//!   `Artifact`: the working-area primitive mapping members to roles.
//! - [`role`] — `Role`, `OpClass`, `ArtifactKind` plus the static
//!   permission table ([`role::role_permits`]) — the coarse base layer.
//! - [`caps`] — the richer `(subject × scope × verb)` capability model
//!   stacked on the role table; wired into the validator's step 5a.
//! - [`invite`] — attenuable capability invite tokens
//!   ([`invite::InviteToken`]): a signed, delegable grant that can only narrow.
//! - [`event`] — `ChannelEvent`, the append-only governance log.
//! - [`op`] — `SignedOp`, the per-op envelope wrapping every CRDT
//!   mutation crossing a process or network boundary.
//! - [`canonical`] — deterministic `postcard` serialisation; every
//!   signed payload bottoms out here so signatures are stable across
//!   machines and Rust versions.
//! - [`crypto`] — Ed25519 signing + verification primitives.
//! - [`validator`] — the eight-step inbound-op validation pipeline, a
//!   single pure function ([`validator::validate_inbound_op`]) over a
//!   [`validator::ChannelState`] snapshot.
//!
//! Non-goals: no network code (the validator operates on a snapshot;
//! lazy-fetch of unknown attestations lives in the broker); no cryptographic
//! key handling (this crate carries signed MLS metadata and opaque ciphertext,
//! while `channel-crypto` owns group secrets); no FROST/threshold sharing
//! (`Identity::public_key` is a single Ed25519 key); no persistence.
//!
//! Every on-wire struct derives `Serialize`/`Deserialize` and carries
//! `#[serde(deny_unknown_fields)]` so an extension field can't quietly
//! slip past a future name-based decoder. Canonical bytes for signing
//! flow through [`canonical::canonical_bytes`] (postcard's deterministic
//! byte order); [`canonical::from_canonical_bytes`] enforces exact-length
//! input — trailing bytes are an error, not a silent drop.

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

pub mod canonical;
pub mod caps;
pub mod channel;
pub mod compaction;
pub mod crypto;
pub mod document_import;
pub mod document_transfer;
pub mod event;
pub mod file_invite;
pub mod files;
pub mod first_party_app_ids;
pub mod foreign_device;
pub mod history;
pub mod identity;
pub mod invite;
pub mod mls_governance;
pub mod op;
pub mod ownership_transfer;
pub mod peer;
pub mod permission_profile;
pub mod recipient_control;
pub mod redaction;
pub mod role;
pub mod share_control;
pub mod space;
pub mod validator;

// Convenience re-exports so downstream callers can do
// `use collab_types::{SignedOp, Role, OpClass, ArtifactKind, ...}`
// without reaching into individual modules.
pub use canonical::{CanonicalError, canonical_bytes};
pub use caps::{
    Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, ResourceScope, Scope,
    Subject, Verb, decide, decide_actor, decide_capability, host_covers, path_covers,
    role_base_applies, role_caps, verb_for_opclass,
};
pub use channel::{
    Artifact, ArtifactId, ArtifactRef, Channel, ChannelId, ChannelRef, ChannelSettingsDelta,
    DocumentHandle, MemberRef,
};
pub use compaction::{
    CompactionCheckpoint, CompactionError, SEALED_COMPACTION_VERSION, SealedCompaction,
    encode_compaction_payload, hash_op_prefix, hash_sealed_prefix, parse_compaction_payload,
    sealed_ciphertext,
};
pub use crypto::{
    Ed25519PublicKey, Signature, VerifyError, generate_keypair, sign_event, sign_op, verify_event,
    verify_op,
};
pub use document_transfer::{
    DocumentFrame, DocumentMutationKind, DocumentReassembler, DocumentSegment,
    MAX_DOCUMENT_TRANSFER_BYTES, ReassembledDocument, ReassemblyError,
};
pub use event::{ChannelEvent, RoleAttestation};
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
#[allow(deprecated, reason = "stable collab-types source compatibility")]
pub use files::{
    FileMutationOperationV1, FileMutationParticipantRoleV1, FileMutationParticipantV1,
    FileMutationTransactionManifestV1, FileMutationTransactionV1,
};
pub use identity::{Attestation, AttestationId, Claim, Clearance, Identity};
pub use invite::{
    INVITE_DOMAIN_TAG, InviteError, InviteSpec, InviteToken, attenuate, mint_root, verify_chain,
};
pub use mls_governance::{
    MLS_ACCESS_VERSION, MLS_GOVERNANCE_VERSION, MlsAccessRequestV1, MlsGovernanceAckV1,
    MlsGovernanceAction, MlsGovernanceContext, MlsGovernanceError, MlsGovernanceFrameV1,
};
pub use op::{
    ContentRetention, DocumentMutation, DocumentMutationEncodeError, DocumentMutationError,
    EncryptionMetadata, EncryptionScheme, MAX_DOCUMENT_MUTATION_BYTES, RoutedDocumentPayload,
    SignedOp, WholeDocumentPayload, document_artifact_id, document_attestation_id,
    document_channel_id, validate_document_payload_route, validate_whole_document_payload_route,
};
pub use ownership_transfer::{
    OWNERSHIP_TRANSFER_VERSION, OwnershipTransferAcceptanceV1, OwnershipTransferAuditV1,
    OwnershipTransferCancellationV1, OwnershipTransferError, OwnershipTransferProposalV1,
};
pub use peer::{PeerPresence, PeerPresencePayload};
pub use permission_profile::{PermissionProfile, PermissionProfileError, PermissionProfileRule};
pub use recipient_control::{
    MAX_RECIPIENT_CONTROL_CONNECTIONS, MAX_RECIPIENT_CONTROL_FRAME_BYTES,
    MAX_RECIPIENT_CONTROL_REQUESTS_PER_CONNECTION, MAX_RECIPIENT_MOUNT_PATH_BYTES,
    RECIPIENT_CONTROL_FRAMING, RECIPIENT_CONTROL_VERSION, RecipientControlErrorCode,
    RecipientControlOperationV1, RecipientControlRequestV1, RecipientControlResponseV1,
    RecipientControlResultV1, RecipientFailureCodeV1, RecipientFailureV1, RecipientMountStatusV1,
    RecipientSubscriptionKeyV1, RecipientSubscriptionStatusV1,
};
pub use redaction::{
    MAX_REDACTION_REASON_BYTES, OpId, RedactRequest, RedactedMarker, RedactionError,
    encode_redact_payload, hash_target_op, parse_redact_payload,
};
pub use role::{ArtifactKind, OpClass, Role, role_permits};
pub use share_control::{
    MAX_SHARE_CONTROL_FRAME_BYTES, NamedGrantMetadataV1, NamedShareDeliveryStateV1,
    SHARE_CONTROL_FRAMING, SHARE_CONTROL_VERSION, ShareControlErrorCode, ShareControlOperationV1,
    ShareControlRequestV1, ShareControlResponseV1, ShareControlResultV1,
};
pub use space::{Space, SpaceId};
pub use validator::{
    ChannelState, ValidationError, validate_admitted_inbound_op, validate_inbound_op,
    validate_replayed_inbound_op,
};

pub mod local_transport;
