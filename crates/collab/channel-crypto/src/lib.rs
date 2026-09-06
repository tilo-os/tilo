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
//! three-or-more-member groups; [`ChannelGroup::remove_member`] emits the
//! removal commit that advances surviving members and deactivates the removed
//! member.
//!
//! Out of scope for now: external commits/joins; late-join history
//! backfill (joiners start at the inviter's current epoch, older
//! ciphertexts are dropped); epoch rotation triggered by anything other
//! than membership changes; direct-share/cross-channel envelopes. Sealed
//! persistence exists, but vault-key custody belongs to the production
//! identity service.
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
//! later epoch). [`collab_types::MlsGovernanceFrameV1`] carries versioned,
//! signed clear routing metadata around opaque Welcome/commit bytes;
//! [`GovernanceJournal`] persists its targeted delivery outbox.

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
pub mod governance;
pub mod group;
pub mod persistence;

pub use crypto::{CIPHERSUITE, MemberKey};
pub use errors::{Error, Result};
pub use governance::{DeliveryKind, GovernanceDelivery, GovernanceJournal};
pub use group::ChannelGroup;
pub use persistence::VaultKey;
// Re-exported so callers can take/return KeyPackage values without
// pulling openmls into their own Cargo.toml — a version-pinning
// hazard if openmls reshapes `KeyPackage`, flagged in the README.
pub use openmls::prelude::KeyPackage;

/// Encode a public MLS key package for durable handle registration.
///
/// # Errors
///
/// Returns [`Error::TlsCodec`] if `OpenMLS` cannot encode the package.
pub fn encode_key_package(package: &KeyPackage) -> Result<Vec<u8>> {
    use openmls::prelude::tls_codec::Serialize as _;
    package.tls_serialize_detached().map_err(Error::tls_codec)
}

/// Decode a public MLS key package previously produced by
/// [`encode_key_package`], refusing trailing bytes.
///
/// # Errors
///
/// Returns [`Error::TlsCodec`] for malformed or non-canonical bytes.
pub fn decode_key_package(bytes: &[u8]) -> Result<KeyPackage> {
    use openmls::prelude::OpenMlsProvider as _;
    use openmls::prelude::tls_codec::Deserialize as _;

    let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
    openmls::prelude::KeyPackageIn::tls_deserialize_exact(bytes)
        .map_err(Error::tls_codec)
        .and_then(|package| {
            package
                .validate(provider.crypto(), openmls::prelude::ProtocolVersion::Mls10)
                .map_err(Error::openmls)
        })
}
