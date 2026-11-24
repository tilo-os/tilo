//! `collab-types` — the cryptographic foundation of workspaces,
//! documents and role-based access control.
//!
//! Self-contained: pure types, pure functions, no I/O, no async runtime,
//! no compositor or UI dependencies. Signed-roles wire enforcement, MLS
//! encryption, and threshold identity all layer on the primitives here.
//!
//! - [`identity`] — `AttestationId`, the route tag a signed op carries, and
//!   avatar bytes.
//! - [`artifact`] — `ArtifactId`: the artifact a document's ops belong to.
//! - [`role`] — `Role`, `OpKind`, `ArtifactKind` of the op envelope; who may
//!   make which change is decided by `tilo-action-policy` grants.
//! - [`caps`] — the app sandbox: `(who × scope × access)` capabilities
//!   over paths, file records, devices and network hosts.
//! - [`op`] — `SignedOp`, the per-op envelope wrapping every CRDT
//!   mutation crossing a process or network boundary.
//! - [`commit`] — `WorkspaceCommit`, the one signed record of every unit
//!   change one action made, carried inside the signed payload of the
//!   document frame of each of its parts.
//! - [`canonical`] — deterministic `postcard` serialisation; every
//!   signed payload bottoms out here so signatures are stable across
//!   machines and Rust versions.
//! - [`crypto`] — Ed25519 signing + verification primitives.
//! - [`validator`] — inbound document-op validation: pure functions over an
//!   [`validator::InboundState`] snapshot.
//! - [`scope`] — `Scope`: the workspace or record a live track belongs to,
//!   and the id its name carries.
//!
//! Filesystem records and control protocols live in `fs-protocol`, the
//! first-party app catalogue in `tilo-desktop-model`, and the launcher's collab
//! preamble in `launcher::services`; none of them is collaboration vocabulary.
//!
//! Non-goals: no network code (the validator operates on a snapshot); no
//! cryptographic key handling (this crate carries signed MLS metadata and
//! opaque ciphertext, while the key crate owns group secrets); no
//! FROST/threshold sharing; no persistence.
//!
//! Every on-wire struct derives `Serialize`/`Deserialize` and carries
//! `#[serde(deny_unknown_fields)]` so an extension field can't quietly
//! slip past a future name-based decoder. Canonical bytes for signing
//! flow through [`canonical::canonical_bytes`] (postcard's deterministic
//! byte order); [`canonical::from_canonical_bytes`] enforces exact-length
//! input — trailing bytes are an error, not a silent drop.

pub mod action_authority;
pub mod actors;
pub mod app_surface;
pub mod artifact;
pub mod calls;
pub mod canonical;
pub mod caps;
pub mod channel_key;
pub mod claim;
pub mod commit;
pub mod compaction;
pub mod crypto;
pub mod direct;
pub mod document;
pub mod document_import;
pub mod document_transfer;
pub mod handoff;
pub mod history;
pub mod identity;
pub mod inbound;
pub mod member;
pub mod mls_governance;
pub mod op;
pub mod peer;
pub mod photo;
pub mod presence;
pub mod readers;
pub mod realtime;
pub mod receipt;
pub mod record_id;
pub mod redaction;
pub mod role;
pub mod scope;
pub mod signer;
pub use tilo_actions::stamp;
pub mod surface;
pub mod units;
pub mod validator;
pub mod workspace_document;
pub mod workspace_grant;
pub mod workspace_id;
// A test that reads a whole document's changes loads it in a worker, which
// this test binary serves.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

/// The URI scheme that asks an app to open one of its own documents
/// (`tilo-document:<document id>`): how a launch delivers an invocation to the
/// view of the document it addresses. Every app accepts it; it names a document
/// in the app's own store and grants nothing.
pub const DOCUMENT_URI_SCHEME: &str = "tilo-document";

// Convenience re-exports so downstream callers can do
// `use collab_types::{SignedOp, Role, OpKind, ArtifactKind, ...}`
// without reaching into individual modules.
pub use artifact::{ArtifactId, FILES_TREE_KIND, file_content_artifact};
pub use canonical::{CanonicalError, canonical_bytes};
pub use caps::{
    Access, CapDecision, CapOverride, DeviceCap, Effect, FsAuthority, LaunchGrants, ResourceRef,
    Scope, decide_app, decide_capability, host_covers, path_covers,
};
pub use claim::{
    ChangeClaim, ClaimError, ClaimedAction, CommitNamed, FrameworkChange, Invoker, UnitPath,
    check_claimed, claims_of, field_edit_mutation, mutation_claims, parse_changes, record_unit,
};
pub use commit::{
    COMMIT_VERSION, CommitAuthor, CommitError, CommitGroup, CommitPart, MAX_COMMIT_BYTES,
    UnitMutation, WorkspaceCommit,
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
pub use direct::{canonical_pair, direct_pair_id, direct_resource};
pub use document_transfer::{
    DocumentFrame, DocumentMutationKind, DocumentReassembler, DocumentSegment,
    MAX_DOCUMENT_TRANSFER_BYTES, ReassembledDocument, ReassemblyError,
};
pub use identity::{AttestationId, AvatarBytes};
pub use member::{Member, Person};
pub use mls_governance::{
    JoinMaterial, Joining, MAX_COMMIT_DEVICES, MLS_ACCESS_VERSION, MLS_GOVERNANCE_VERSION,
    MlsAccessRequestV1, MlsGovernanceAction, MlsGovernanceError, MlsGovernanceFrame,
};
pub use op::{
    ContentRetention, DocumentMutation, DocumentMutationEncodeError, DocumentMutationError,
    EncryptionMetadata, EncryptionScheme, GOVERNANCE_NEVER_ARRIVED, MAX_DOCUMENT_MUTATION_BYTES,
    RoutedDocumentPayload, SignedOp, WholeDocumentPayload, document_artifact_id,
    document_attestation_id, governance_head, names_document, next_unordered_document_sequence,
    validate_document_payload_route, validate_whole_document_payload_route,
};
pub use peer::{PeerPresence, PeerPresencePayload};
pub use photo::{MAX_PHOTO_CHARS, PhotoError, avatar_of, photo, photo_bytes, photo_url};
pub use readers::{
    EVERY_MEMBER, NamedReaders, ROLE_READERS, Readers, app_document_id, content_artifact_id,
    is_store_document, reader_group, reader_set_name, store_document_readers,
};
pub use receipt::{
    Allows, ChangeAuthor, Receipt, ReceiptRefusal, ReceiptSurface, SurfaceSource, Surfaces,
    WrittenDocument, check_appearances, check_claims, check_claims_by_kind, check_continuations,
    check_opened, check_opened_mutation, check_reassembled, claim_node, claimed_invokers,
    decide_op, decide_op_imported, decide_op_pinned, decide_op_replayed, decide_opened,
    written_node,
};
pub use record_id::{
    is_personal_cluster, person_document_context, person_document_for_workspace,
    person_document_id, person_resource_type, personal_cluster,
};
pub use redaction::{
    MAX_REDACTION_REASON_BYTES, OpId, RedactRequest, RedactedMarker, RedactionError,
    encode_redact_payload, hash_target_op, parse_redact_payload,
};
pub use role::{ArtifactKind, OpKind, Role};
pub use stamp::Stamp;
pub use tilo_action_policy::{MemberId, PersonId, PersonIdError};
pub use workspace_id::{is_person_workspace, person_workspace, person_workspace_of};

pub use validator::{
    DEFAULT_WORKSPACE, InboundState, ValidationError, document_node, validate_admitted_inbound_op,
    validate_kept_inbound_op, validate_replayed_inbound_op,
};
pub use validator::{
    DocumentAccessRule, document_access_changes, document_access_rules, document_access_scopes,
    document_access_target,
};
pub use workspace_document::{
    CONTENT_REFUSAL_APP, CONTENT_REFUSAL_KIND, WORKSPACE_DOCUMENTS, WorkspaceDocument,
    content_refusal_document_id, is_content_refusal_document, shared_workspace_document,
    workspace_document, workspace_document_id, workspace_document_of_kind,
};
pub use workspace_id::WorkspaceId;
