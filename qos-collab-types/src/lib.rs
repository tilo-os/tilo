//! `qos-collab-types` — the cryptographic foundation for qOS Spaces,
//! Channels, and role-based access control.
//!
//! This crate implements **Phase 2** of the
//! `2026-06-29-spaces-channels-and-cryptographic-roles.md` design spec.
//! It is intentionally self-contained: pure types, pure functions, no
//! I/O, no async runtime, no compositor or UI dependencies. Every later
//! phase of the collaboration roadmap (signed-roles enforcement on the
//! wire in Phase 3, MLS encryption in Phase 7, threshold identity in
//! Phase 8) layers on the primitives defined here.
//!
//! ## What is in this crate
//!
//! - [`identity`] — `Identity`, `Attestation`, `Claim`, `Clearance`.
//!   The human-facing key material plus the signed claims that
//!   identities carry across spaces.
//! - [`space`] — `Space`, the social-formation container that groups
//!   channels and caches a member directory.
//! - [`channel`] — `Channel`, `MemberRef`, `ChannelRef`, `ArtifactRef`,
//!   `Artifact`. The working-area primitive that holds artifacts and
//!   maps members to roles.
//! - [`role`] — `Role`, `OpClass`, `ArtifactKind` plus the
//!   spec-defined static permission table
//!   ([`role::role_permits`]). This is the single source of truth for
//!   "is this op allowed?".
//! - [`event`] — `ChannelEvent`, the append-only governance log:
//!   genesis, attestation issue/revoke, ownership transfer, rename,
//!   settings update.
//! - [`op`] — `SignedOp`, the per-op envelope that wraps every CRDT
//!   mutation crossing a process or network boundary.
//! - [`canonical`] — deterministic byte serialisation via `postcard`.
//!   All signed payloads bottom out here so that signatures are stable
//!   across machines and Rust versions.
//! - [`crypto`] — Ed25519 signing + verification primitives.
//!   `sign_event` / `verify_event` for governance events;
//!   `sign_op` / `verify_op` for `SignedOp`.
//! - [`validator`] — the eight-step inbound-op validation pipeline
//!   from the spec, implemented as a single pure function
//!   ([`validator::validate_inbound_op`]) consuming a
//!   [`validator::ChannelState`] snapshot.
//!
//! ## Non-goals (deliberate)
//!
//! - No network code. The validator operates on a snapshot; resolution
//!   of unknown attestation ids (the spec's "lazy-fetch from peers")
//!   lives in the broker, not here.
//! - No MLS / encryption. The wire format described here is
//!   *plaintext*; Phase 7 layers `openmls` on top.
//! - No FROST / threshold sharing. `Identity::public_key` is a single
//!   Ed25519 public key in this phase; the threshold-shared private
//!   key arrives in Phase 8.
//! - No persistence. Nothing in this crate touches the filesystem.
//!
//! ## Wire-protocol stability
//!
//! All on-wire structures derive `Serialize` + `Deserialize`. Where
//! a future name-based decoder (JSON, RON) might pick them up,
//! every struct also carries `#[serde(deny_unknown_fields)]` so an
//! extension field cannot quietly slip past. Canonical bytes for
//! signing flow through [`canonical::canonical_bytes`], which pins
//! serialisation to `postcard`'s deterministic byte order, and
//! [`canonical::from_canonical_bytes`] enforces exact-length input
//! — an envelope plus trailing bytes is an error, not a successful
//! decode that silently drops the tail.
//!
//! Once we ship Phase 3, any field reorder or addition is a
//! wire-breaking change and needs a version bump.

pub mod canonical;
pub mod channel;
pub mod compaction;
pub mod crypto;
pub mod event;
pub mod identity;
pub mod op;
pub mod role;
pub mod space;
pub mod validator;

// Convenience re-exports so downstream callers can do
// `use qos_collab_types::{SignedOp, Role, OpClass, ArtifactKind, ...}`
// without reaching into individual modules.
pub use canonical::{CanonicalError, canonical_bytes};
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
pub use op::{EncryptionMetadata, SignedOp};
pub use role::{ArtifactKind, OpClass, Role, role_permits};
pub use space::{Space, SpaceId};
pub use validator::{ChannelState, ValidationError, validate_inbound_op};
