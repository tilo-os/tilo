//! `channel-crypto` — per-channel MLS group encryption for Tilo.
//!
//! Each Tilo channel becomes one MLS group, used for group key agreement:
//! the group's epoch secrets rotate on every membership change. Ops are not
//! encrypted with MLS application messages but under per-epoch *content
//! keys* derived from the MLS exporter ([`content`]), so every member —
//! including one added later — can read the channel's whole history. The
//! envelope's metadata (author, artifact, `kind`, timestamp, seq) stays
//! cleartext so the broker routes by artifact and filters by op class
//! without decrypting.
//!
//! ## Supported operations
//!
//! [`MemberKey::generate`] mints a fresh per-member signer + provider;
//! [`MemberKey::fresh_key_package`] mints a key package to ship to a
//! channel owner; [`ChannelGroup::create`] bootstraps a one-member
//! channel; [`ChannelGroup::admit`] / [`ChannelGroup::add_member_transactional`]
//! admit a candidate and return its Welcome, sealed key history and the
//! commit for existing members; [`ChannelGroup::process_welcome_with_history`]
//! joins with the full key history; [`ChannelGroup::seal_content`] /
//! [`ChannelGroup::open_content`] seal and open op payloads (any epoch the
//! member holds a key for, any number of times); `process_commit` advances
//! existing members; [`ChannelGroup::remove_member`] emits the removal commit
//! after which the removed member derives no further content keys.
//! [`ChannelGroup::encrypt`]/`decrypt` remain for version-1 MLS
//! application messages.
//!
//! This crate does not support external commits or joins, epoch rotation
//! outside membership changes, or direct-share/cross-channel envelopes.
//! Sealed persistence exists, but vault-key custody belongs to the
//! production identity service.
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
//! Option<EncryptionMetadata>` — `None` is plaintext, `Some` names the
//! channel, epoch and versioned [`collab_types::EncryptionScheme`]: version 1
//! MLS application messages (still readable by members at that epoch) or
//! version 2 content-key envelopes. [`collab_types::MlsGovernanceFrameV1`]
//! carries versioned, signed clear routing metadata around opaque
//! Welcome/commit/key-history bytes; [`GovernanceJournal`] persists its
//! targeted delivery outbox.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod content;
pub mod crypto;
pub mod endpoint;
pub mod errors;
pub mod governance;
pub mod group;
pub mod member;
pub mod persistence;
pub mod signer;
pub mod steward;
pub mod store;

pub use content::{ContentKeyring, KeyHistory};
pub use crypto::{CIPHERSUITE, MemberKey};
pub use endpoint::ChannelContentKeys;
pub use errors::{Error, Result};
pub use governance::{
    ChannelGate, DeliveryKind, GovernanceDelivery, GovernanceJournal, MirrorAppend,
};
pub use group::{ChannelGroup, MemberAddition};
pub use member::{Access, ChannelMember, MemberEvent};
pub use persistence::VaultKey;
pub use signer::{SignFuture, StewardSigner};
pub use steward::{ChannelSteward, Outgoing, Reconciled};
pub use store::{FileStewardStore, MemoryStewardStore, StewardRecord, StewardStore};
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
