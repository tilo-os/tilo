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
//! - [`role`] — `Role`, `OpKind`, `ArtifactKind` of the op envelope; who may
//!   make which change is decided by `tilo-action-policy` grants (step 5).
//! - [`caps`] — the app sandbox: `(subject × scope × access)` capabilities
//!   over paths, file records, devices and network hosts.
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
//! Filesystem records and control protocols live in `fs-protocol`, the
//! first-party app catalogue in `tilo-desktop-model`, and the launcher's collab
//! preamble in `launcher::services`; none of them is collaboration vocabulary.
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod action_authority;
pub mod canonical;
pub mod caps;
pub mod channel;
pub mod claim;
pub mod compaction;
pub mod crypto;
pub mod document_import;
pub mod document_transfer;
pub mod event;
pub mod history;
pub mod identity;
pub mod invite;
pub mod mls_governance;
pub mod op;
pub mod ownership_transfer;
pub mod peer;
pub mod person_room;
pub mod receipt;
pub mod redaction;
pub mod role;
pub mod space;
pub mod surface;
pub mod task_budget;
pub mod units;
pub mod validator;

/// The URI scheme that asks an app to open one of its own records
/// (`tilo-record:<record id>`): how a launch delivers an invocation to the
/// view of the record it addresses. Every app accepts it; it names a record
/// in the app's own store and grants nothing.
pub const RECORD_URI_SCHEME: &str = "tilo-record";

// Convenience re-exports so downstream callers can do
// `use collab_types::{SignedOp, Role, OpKind, ArtifactKind, ...}`
// without reaching into individual modules.
pub use canonical::{CanonicalError, canonical_bytes};
pub use caps::{
    Access, Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope,
    Subject, decide_capability, host_covers, path_covers,
};
pub use channel::{
    Artifact, ArtifactId, ArtifactRef, Channel, ChannelId, ChannelRef, ChannelSettingsDelta,
    DocumentHandle, MemberRef,
};
pub use claim::{
    ChangeClaim, ClaimError, ClaimedAction, CommitNamed, FrameworkChange, UnitPath, check_claimed,
    claims_of, field_edit_mutation, mutation_claims, parse_changes, record_unit,
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
    document_channel_id, live_artifact_id, next_unordered_document_sequence,
    validate_document_payload_route, validate_whole_document_payload_route,
};
pub use ownership_transfer::{
    OWNERSHIP_TRANSFER_VERSION, OwnershipTransferAcceptanceV1, OwnershipTransferAuditV1,
    OwnershipTransferCancellationV1, OwnershipTransferError, OwnershipTransferProposalV1,
};
pub use peer::{PeerPresence, PeerPresencePayload};
pub use person_room::{is_person_room, person_room, person_room_of};
pub use receipt::{
    Receipt, ReceiptRefusal, ReceiptSurface, Record, SurfaceSource, Surfaces, check_claims,
    claim_node, decide_op, record_node,
};
pub use redaction::{
    MAX_REDACTION_REASON_BYTES, OpId, RedactRequest, RedactedMarker, RedactionError,
    encode_redact_payload, hash_target_op, parse_redact_payload,
};
pub use role::{ArtifactKind, OpKind, Role};
pub use space::{Space, SpaceId};
pub use validator::{
    ChannelState, DEFAULT_ROOM, ValidationError, document_node, validate_admitted_inbound_op,
    validate_inbound_op, validate_replayed_inbound_op,
};
