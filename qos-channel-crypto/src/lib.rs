//! `qos-channel-crypto` — per-channel MLS group encryption for qOS.
//!
//! This crate implements **Phase 7** of the
//! `2026-06-29-spaces-channels-and-cryptographic-roles.md` design
//! spec. Each qOS channel becomes one MLS group; the group key
//! rotates on every membership change; `SignedOp.payload` ships
//! encrypted while the envelope's metadata (author, artifact,
//! op_class, timestamp, seq) stays cleartext so the broker can route
//! by artifact and filter by op class without decrypting.
//!
//! ## Minimum-viable scope
//!
//! Phase 7 deliberately ships a narrow surface:
//!
//! - [`MemberKey::generate`] — mint a fresh per-member signer + provider.
//! - [`MemberKey::fresh_key_package`] — mint a key package to ship to
//!   a channel owner.
//! - [`ChannelGroup::create`] — bootstrap a one-member channel.
//! - [`ChannelGroup::add_member`] — admit a candidate; returns a Welcome.
//! - [`ChannelGroup::process_welcome`] — join an existing channel from a Welcome.
//! - [`ChannelGroup::encrypt`] / [`ChannelGroup::decrypt`] — application-message round-trip.
//! - [`ChannelGroup::add_member_with_commit`] +
//!   [`ChannelGroup::process_commit`] — three-or-more-member groups.
//!
//! Out of scope until Phase 7.1+:
//!
//! - Member removal (`remove_member` is a documented `todo!()`).
//! - External commits / external joins.
//! - Late-join history backfill (joiners start at the inviter's
//!   current epoch; older ciphertexts they receive are dropped).
//! - Epoch rotation triggered by anything other than `add_member`.
//! - Direct-share / cross-channel envelope.
//! - On-disk persistence (storage is in-memory per group).
//!
//! ## Cipher suite
//!
//! One fixed suite: [`crypto::CIPHERSUITE`] =
//! `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`. Negotiation is
//! deferred to a future phase. Rationale lives in [`crypto`].
//!
//! ## Wire shapes
//!
//! Welcome, commit, and application-message bytes are TLS-CODEC
//! serialised `MlsMessageOut` envelopes. Decoders reconstruct an
//! `MlsMessageIn`. The broker treats these as opaque; the encrypting
//! and decrypting [`ChannelGroup`] is what gives them meaning.
//!
//! ## Integration with `qos-collab-types`
//!
//! - [`qos_collab_types::SignedOp`] now carries an
//!   `encryption: Option<EncryptionMetadata>` field. `None` means
//!   "payload is plaintext"; `Some` means the payload is a
//!   ciphertext from this crate's [`ChannelGroup::encrypt`] and the
//!   recipient must hold a matching `ChannelGroup` (same channel id,
//!   same or later epoch) to decrypt.
//! - [`qos_collab_types::ChannelEvent`] gains `MemberAdded` and
//!   `MemberRemoved` variants carrying the Welcome / removal bytes.
//!   These are governance events: the broker stores them in the
//!   channel event log and replays for joiners.

pub mod crypto;
pub mod errors;
pub mod group;

pub use crypto::{CIPHERSUITE, MemberKey};
pub use errors::{Error, Result};
pub use group::ChannelGroup;
// Re-exported so callers can take/return KeyPackage values without
// pulling the openmls crate into their own Cargo.toml. This is a
// stability hazard if openmls re-shapes `KeyPackage` between minor
// versions — flagged in the README under "version-pinning".
pub use openmls::prelude::KeyPackage;
