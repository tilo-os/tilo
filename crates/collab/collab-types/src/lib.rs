//! `collab-types` — the cryptographic foundation of workspaces,
//! documents and role-based access control.
//!
//! Self-contained: pure types, pure functions, no I/O, no async runtime,
//! no compositor or UI dependencies. Signed-roles wire enforcement, MLS
//! encryption, and threshold identity all layer on the primitives here.
//!
//! - [`identity`] — `Identity`, `Attestation`, `Claim`, `Clearance`: key
//!   material and signed claims identities carry.
//! - [`channel`] — `ChannelId`, `ArtifactId`: ids carried by a document's ops.
//! - [`role`] — `Role`, `OpKind`, `ArtifactKind` of the op envelope; who may
//!   make which change is decided by `tilo-action-policy` grants (step 5).
//! - [`caps`] — the app sandbox: `(subject × scope × access)` capabilities
//!   over paths, file records, devices and network hosts.
//! - [`event`] — `ChannelEvent`, the append-only governance log.
//! - [`op`] — `SignedOp`, the per-op envelope wrapping every CRDT
//!   mutation crossing a process or network boundary.
//! - [`commit`] — `WorkspaceCommit`, the one signed record of every unit
//!   change one action made, carried inside the signed payload of the
//!   document frame of each of its parts.
//! - [`canonical`] — deterministic `postcard` serialisation; every
//!   signed payload bottoms out here so signatures are stable across
//!   machines and Rust versions.
//! - [`crypto`] — Ed25519 signing + verification primitives.
//! - [`validator`] — the eight-step inbound-op validation pipeline, a
//!   single pure function ([`validator::validate_inbound_op`]) over a
//!   [`validator::ChannelState`] snapshot.
//! - [`scope`] — `Scope`: the workspace, channel or record whose keys seal a
//!   live track, and the id its name carries.
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

pub mod action_authority;
pub mod canonical;
pub mod caps;
pub mod channel;
pub mod channel_key;
pub mod claim;
pub mod commit;
pub mod compaction;
pub mod crypto;
pub mod direct;
pub mod document_import;
pub mod document_transfer;
pub mod event;
pub mod handoff;
pub mod history;
pub mod identity;
pub mod inbound;
pub mod key_scope;
pub mod mls_governance;
pub mod op;
pub mod peer;
pub mod personal;
pub mod presence;
pub mod realtime;
pub mod receipt;
pub mod redaction;
pub mod role;
pub mod scope;
pub mod signer;
pub mod surface;
pub mod task_budget;
pub mod task_presentation;
pub mod units;
pub mod validator;
pub mod workspace_id;
pub mod workspace_record;

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
    Access, Actor, CapDecision, CapOverride, Effect, Expiry, FsAuthority, LaunchGrants, Principal,
    ResourceRef, Scope, Subject, WorkspaceMember, decide_capability, host_covers, path_covers,
};
pub use channel::{ArtifactId, ChannelId};
pub use workspace_id::WorkspaceId;
pub use claim::{
    ChangeClaim, ClaimError, ClaimedAction, CommitNamed, FrameworkChange, UnitPath, check_claimed,
    claims_of, field_edit_mutation, mutation_claims, parse_changes, record_unit,
};
pub use commit::{
    COMMIT_RECORD_KIND, COMMIT_VERSION, COMMIT_VERSION_NAMED_ACTION, CommitAuthor, CommitError,
    CommitGroup, CommitPart, MAX_COMMIT_BYTES, UnitMutation, WorkspaceCommit,
};
pub use compaction::{
    CompactionCheckpoint, CompactionError, SEALED_COMPACTION_VERSION, SEGMENTED_COMPACTION_VERSION,
    SealedCompaction, SealedCompactionSegment, encode_compaction_payload, hash_op_prefix,
    hash_sealed_prefix, is_compaction_piece, is_compaction_segment, parse_compaction_payload,
    sealed_ciphertext, segment_hash,
};
pub use crypto::{
    Ed25519PublicKey, Signature, VerifyError, generate_keypair, sign_event, sign_op, verify_event,
    verify_op,
};
pub use direct::{canonical_pair, direct_pair_id, direct_record};
pub use document_transfer::{
    DocumentFrame, DocumentMutationKind, DocumentReassembler, DocumentSegment,
    MAX_DOCUMENT_TRANSFER_BYTES, ReassembledDocument, ReassemblyError,
};
pub use event::{ChannelEvent, RoleAttestation};
pub use identity::{Attestation, AttestationId, Claim, Clearance, Identity};
pub use key_scope::{WORKSPACE_READERS, workspace_scope};
pub use mls_governance::{
    ChannelRoute, JoinMaterial, Joining, MAX_CHANNEL_ROUTE_ID_BYTES, MAX_COMMIT_DEVICES,
    MLS_ACCESS_VERSION, MLS_GOVERNANCE_VERSION, MlsAccessRequestV1, MlsGovernanceAction,
    MlsGovernanceError, MlsGovernanceFrame,
};
pub use op::{
    ContentRetention, DocumentMutation, DocumentMutationEncodeError, DocumentMutationError,
    EncryptionMetadata, EncryptionScheme, MAX_DOCUMENT_MUTATION_BYTES, RoutedDocumentPayload,
    SignedOp, WholeDocumentPayload, document_artifact_id, document_attestation_id,
    document_channel_id, live_artifact_id, next_unordered_document_sequence,
    validate_document_payload_route, validate_whole_document_payload_route,
};
pub use peer::{PeerPresence, PeerPresencePayload};
pub use personal::{
    is_personal_cluster, person_record_id, person_record_kind, personal_cluster,
    personal_cluster_of, retired_person_workspace,
};
pub use receipt::{
    Receipt, ReceiptRefusal, ReceiptSurface, Record, SurfaceSource, Surfaces, check_appearances,
    check_claims, check_claims_by_kind, claim_node, decide_op, decide_op_pinned, record_node,
};
pub use redaction::{
    MAX_REDACTION_REASON_BYTES, OpId, RedactRequest, RedactedMarker, RedactionError,
    encode_redact_payload, hash_target_op, parse_redact_payload,
};
pub use role::{ArtifactKind, OpKind, Role};
pub use tilo_action_policy::{member_record, member_record_id};
pub use validator::{
    ChannelState, DEFAULT_WORKSPACE, ValidationError, document_node, named_record_of_channel,
    named_records, validate_admitted_inbound_op, validate_inbound_op, validate_replayed_inbound_op,
};
pub use workspace_record::{
    WORKSPACE_RECORDS, WorkspaceRecord, workspace_record, workspace_record_id,
};
