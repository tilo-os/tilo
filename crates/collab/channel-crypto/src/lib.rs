//! `channel-crypto` — per-channel MLS group encryption for qOS.
//!
//! Each qOS channel becomes one MLS group; the group key rotates on
//! every membership change; `SignedOp.payload` ships encrypted while
//! the envelope's metadata (author, artifact, `op_class`, timestamp, seq)
//! stays cleartext so the broker routes by artifact and filters by op
//! class without decrypting.
//!
//! ## Minimum-viable scope
//!
//! [`MemberKey::generate`] mints a fresh per-member signer + provider;
//! [`MemberKey::fresh_key_package`] mints a key package to ship to a
//! channel owner; [`ChannelGroup::create`] bootstraps a one-member
//! channel; [`ChannelGroup::add_member`] admits a candidate (returns a
//! Welcome); [`ChannelGroup::process_welcome`] joins from a Welcome;
//! [`ChannelGroup::encrypt`]/[`decrypt`] round-trip application
//! messages; [`add_member_with_commit`]/[`process_commit`] handle
//! three-or-more-member groups.
//!
//! Out of scope for now: member removal (`remove_member` is a
//! documented `todo!()`); external commits/joins; late-join history
//! backfill (joiners start at the inviter's current epoch, older
//! ciphertexts are dropped); epoch rotation triggered by anything other
//! than `add_member`; direct-share/cross-channel envelopes; on-disk
//! persistence (storage is in-memory per group).
//!
//! One fixed ciphersuite: [`crypto::CIPHERSUITE`] =
//! `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (rationale in
//! [`crypto`]); negotiation is deferred.
//!
//! Welcome, commit, and application-message bytes are TLS-CODEC
//! serialised `MlsMessageOut` envelopes; the broker treats them as
//! opaque, [`ChannelGroup`] gives them meaning.
//!
//! ## Integration with `collab-types`
//!
//! [`collab_types::SignedOp`] carries `encryption:
//! Option<EncryptionMetadata>` — `None` is plaintext, `Some` is
//! ciphertext from this crate's [`ChannelGroup::encrypt`], requiring the
//! recipient to hold a matching `ChannelGroup` (same channel, same or
//! later epoch). [`collab_types::ChannelEvent`] gains `MemberAdded`/
//! `MemberRemoved` governance variants carrying the Welcome/removal
//! bytes, stored in the channel event log and replayed for joiners.

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

pub mod crypto;
pub mod errors;
pub mod group;

pub use crypto::{CIPHERSUITE, MemberKey};
pub use errors::{Error, Result};
pub use group::ChannelGroup;
// Re-exported so callers can take/return KeyPackage values without
// pulling openmls into their own Cargo.toml — a version-pinning
// hazard if openmls reshapes `KeyPackage`, flagged in the README.
pub use openmls::prelude::KeyPackage;
