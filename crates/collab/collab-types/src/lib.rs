//! `collab-types` — the cryptographic foundation for qOS Spaces,
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
//! lazy-fetch of unknown attestations lives in the broker); no MLS/
//! encryption (the wire format here is plaintext); no FROST/threshold
//! sharing (`Identity::public_key` is a single Ed25519 key); no persistence.
//!
//! Every on-wire struct derives `Serialize`/`Deserialize` and carries
//! `#[serde(deny_unknown_fields)]` so an extension field can't quietly
//! slip past a future name-based decoder. Canonical bytes for signing
//! flow through [`canonical::canonical_bytes`] (postcard's deterministic
//! byte order); [`canonical::from_canonical_bytes`] enforces exact-length
//! input — trailing bytes are an error, not a silent drop.

pub mod canonical;
pub mod caps;
pub mod channel;
pub mod compaction;
pub mod crypto;
pub mod event;
pub mod identity;
pub mod invite;
pub mod op;
pub mod peer;
pub mod redaction;
pub mod role;
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
    CompactionCheckpoint, CompactionError, encode_compaction_payload, hash_op_prefix,
    parse_compaction_payload,
};
pub use crypto::{
    Ed25519PublicKey, Signature, VerifyError, generate_keypair, sign_event, sign_op, verify_event,
    verify_op,
};
pub use event::{ChannelEvent, RoleAttestation};
pub use identity::{Attestation, AttestationId, Claim, Clearance, Identity};
pub use invite::{
    INVITE_DOMAIN_TAG, InviteError, InviteSpec, InviteToken, attenuate, mint_root, verify_chain,
};
pub use op::{EncryptionMetadata, SignedOp};
pub use peer::{PeerPresence, PeerPresencePayload};
pub use redaction::{
    MAX_REDACTION_REASON_BYTES, OpId, RedactRequest, RedactedMarker, RedactionError,
    encode_redact_payload, hash_target_op, parse_redact_payload,
};
pub use role::{ArtifactKind, OpClass, Role, role_permits};
pub use space::{Space, SpaceId};
pub use validator::{ChannelState, ValidationError, validate_inbound_op};
