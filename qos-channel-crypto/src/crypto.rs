//! OpenMLS provider wiring + per-member key material.
//!
//! ## Cipher suite
//!
//! We pick exactly one suite —
//! `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` — and refuse to
//! negotiate. Reasons:
//!
//! - **Ed25519** lines up with the rest of qOS: identity keys
//!   (`qos-collab-types::Ed25519PublicKey`), the FROST threshold
//!   crate (`qos-threshold-identity`), and libp2p's transport key
//!   all use Ed25519. Picking a different signature scheme inside
//!   MLS would force callers to maintain two key materials per
//!   member, defeating the "identity is one keypair" intent of the
//!   spec.
//! - **X25519 / AES-128-GCM / SHA-256** are the small-ciphersuite
//!   defaults used by every MLS interop suite. We don't need P-384
//!   strength for a channel-scale group.
//! - No negotiation removes a whole class of downgrade attacks and
//!   keeps the implementation honest about what it tests.
//!
//! A future Phase 7.x can introduce a second suite under a versioned
//! handshake; today we ship one, document it here, and move on.
//!
//! ## Provider model
//!
//! `openmls::group::MlsGroup` is parameterised over an
//! `OpenMlsProvider` (storage + crypto). Each `ChannelGroup` owns its
//! own [`OpenMlsRustCrypto`] instance so groups are isolated from
//! each other — leaking storage between groups would be a bug.
//!
//! Signature keypairs (`SignatureKeyPair`) live in the provider's
//! storage and are looked up by public-key bytes whenever openmls
//! needs to sign a commit. Therefore: every `ChannelGroup` storing a
//! signer must also have stored the keypair via
//! `SignatureKeyPair::store(provider.storage())`. The [`MemberKey`]
//! helper owns the provider and ensures this invariant by
//! construction.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider;

use crate::errors::{Error, Result};

/// The single ciphersuite this crate supports.
///
/// Encoded as a constant so callers can refer to it in tests and
/// docs without pulling the full `openmls::prelude` into scope.
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// All key material + storage for one member of one channel.
///
/// A `MemberKey` bundles:
///
/// - The member's display name (used to build the MLS `BasicCredential`).
/// - The Ed25519 signature keypair openmls uses for signing commits +
///   application messages.
/// - The provider holding storage for the keypair *and* for any
///   `MlsGroup` the member becomes part of through this `MemberKey`.
///
/// Bundling is intentional: in MLS, the signer's private key, the
/// HPKE private state of any minted `KeyPackage`, and the group's
/// epoch state must all live in the same provider. Storing them in
/// one struct prevents the calling code from accidentally mixing
/// providers — a class of bug that surfaces only at decryption time
/// as "key not found".
///
/// `MemberKey` is `!Clone` on purpose: the provider holds secret
/// material; cloning would require duplicating that material and is
/// almost never what callers want. If you need a second key, mint a
/// fresh `MemberKey`.
pub struct MemberKey {
    /// Display name embedded in the MLS `BasicCredential`. Not
    /// secret; carried in the on-the-wire Welcome and KeyPackage.
    pub(crate) name: Vec<u8>,
    /// The Ed25519 keypair. Public half is committed to in the
    /// member's credential; secret half is stored under the same key
    /// in `provider.storage()`.
    pub(crate) signer: SignatureKeyPair,
    /// Storage + crypto backend. Owns all secret state derived from
    /// this member's participation in any group built from this
    /// `MemberKey` (signing key, HPKE private keys from key packages,
    /// group epoch state).
    pub(crate) provider: OpenMlsRustCrypto,
}

impl MemberKey {
    /// Mint fresh key material for a member with the given display
    /// name.
    ///
    /// The name is opaque to MLS — it sits inside the `BasicCredential`
    /// and is rendered verbatim in Welcome and KeyPackage on the
    /// wire. We accept anything `AsRef<[u8]>` for the same reason
    /// `String::from` does — callers may want UTF-8 names but
    /// non-textual identifiers are also valid.
    pub fn generate(name: impl AsRef<[u8]>) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(SignatureScheme::ED25519).map_err(Error::openmls)?;
        // Persist the signer's secret half in the provider's storage.
        // Without this, `KeyPackage::builder().build(...)` cannot
        // mint a key package referencing this public key.
        signer.store(provider.storage()).map_err(Error::storage)?;
        Ok(Self {
            name: name.as_ref().to_vec(),
            signer,
            provider,
        })
    }

    /// The display name embedded in this member's credential.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// The 32-byte Ed25519 public key half of this member's signer.
    ///
    /// Useful for tying an MLS member back to a `qos-collab-types`
    /// `Identity` (the broker keeps a map from identity pubkey →
    /// member-of-channel state).
    pub fn signature_public_key(&self) -> &[u8] {
        self.signer.public()
    }

    /// Construct the `CredentialWithKey` openmls APIs accept.
    ///
    /// Crate-internal helper — callers should not need this directly.
    pub(crate) fn credential_with_key(&self) -> CredentialWithKey {
        let credential = BasicCredential::new(self.name.clone());
        CredentialWithKey {
            credential: credential.into(),
            signature_key: self.signer.public().into(),
        }
    }

    /// Mint a fresh `KeyPackage` for this member.
    ///
    /// The key package is the artefact that ships out-of-band to a
    /// channel owner so they can call
    /// [`ChannelGroup::add_member`](crate::ChannelGroup::add_member).
    /// It binds:
    ///
    /// - the ciphersuite and protocol version,
    /// - the member's signature public key,
    /// - a freshly generated HPKE keypair whose private half lives in
    ///   this `MemberKey`'s provider.
    ///
    /// Each call mints a **fresh** HPKE keypair, so calling
    /// `fresh_key_package` twice and shipping both copies out lets
    /// the same member join two different groups — but using one
    /// key package twice violates MLS (last-resort key packages are
    /// a feature we don't enable). We hand back the openmls type
    /// directly because callers immediately ship it to a Welcome
    /// issuer; wrapping would just add an unwrap.
    pub fn fresh_key_package(&self) -> Result<KeyPackage> {
        let bundle = KeyPackage::builder()
            .build(
                CIPHERSUITE,
                &self.provider,
                &self.signer,
                self.credential_with_key(),
            )
            .map_err(Error::openmls)?;
        Ok(bundle.key_package().clone())
    }
}

impl std::fmt::Debug for MemberKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the signing key, the HPKE storage, or any
        // provider internals. The name + the public half of the
        // signer are enough to disambiguate in logs.
        f.debug_struct("MemberKey")
            .field("name", &String::from_utf8_lossy(&self.name))
            .field(
                "signature_public_key",
                &format!(
                    "{:02x}{:02x}{:02x}{:02x}..",
                    self.signer.public()[0],
                    self.signer.public()[1],
                    self.signer.public()[2],
                    self.signer.public()[3],
                ),
            )
            .finish()
    }
}
