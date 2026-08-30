//! `OpenMLS` provider wiring + per-member key material.
//!
//! One ciphersuite — `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` —
//! with no negotiation: Ed25519 lines up with the rest of qOS (identity
//! keys, the FROST threshold crate, Iroh's transport key), so a
//! different MLS signature scheme would force two key materials per
//! member; X25519/AES-128-GCM/SHA-256 are the small-ciphersuite MLS
//! interop defaults (no need for P-384 strength at channel scale); and
//! no negotiation removes a class of downgrade attack. A future second
//! suite can arrive under a versioned handshake.
//!
//! `openmls::group::MlsGroup` is parameterised over an `OpenMlsProvider`
//! (storage + crypto). Each `ChannelGroup` owns its own
//! [`OpenMlsRustCrypto`] instance so groups are isolated — leaking
//! storage between groups would be a bug. Signature keypairs live in the
//! provider's storage, looked up by public-key bytes when openmls signs
//! a commit; [`MemberKey`] owns the provider and ensures every stored
//! signer is registered by construction.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider;

use crate::errors::{Error, Result};

/// The single ciphersuite this crate supports, as a constant so callers
/// can refer to it without pulling `openmls::prelude` into scope.
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// All key material + storage for one member of one channel: display
/// name (for the MLS `BasicCredential`), the Ed25519 signature keypair
/// openmls uses for signing, and the provider holding storage for both
/// the keypair and any `MlsGroup` this member joins.
///
/// Bundled deliberately: in MLS the signer's private key, a minted
/// `KeyPackage`'s HPKE private state, and the group's epoch state must
/// all live in the same provider — one struct prevents mixing providers,
/// a bug class that only surfaces at decryption as "key not found".
///
/// `!Clone` on purpose — the provider holds secret material; mint a
/// fresh `MemberKey` instead of duplicating it.
pub struct MemberKey {
    /// Display name embedded in the MLS `BasicCredential`. Not secret —
    /// carried in the on-wire Welcome and `KeyPackage`.
    pub(crate) name: Vec<u8>,
    /// The Ed25519 keypair: public half committed in the credential, secret
    /// half stored under the same key in `provider.storage()`.
    pub(crate) signer: SignatureKeyPair,
    /// Storage + crypto backend, owning all secret state derived from this
    /// member's participation in any group built from this `MemberKey`.
    pub(crate) provider: OpenMlsRustCrypto,
}

impl MemberKey {
    /// Mint fresh key material for a member with the given display name.
    /// The name is opaque to MLS (rendered verbatim in Welcome/KeyPackage);
    /// accepts `AsRef<[u8]>` since non-textual identifiers are also valid.
    pub fn generate(name: impl AsRef<[u8]>) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(SignatureScheme::ED25519).map_err(Error::openmls)?;
        // Persist the signer's secret half in the provider's storage —
        // without this, `KeyPackage::builder().build(...)` can't mint a key
        // package referencing this public key.
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

    /// The 32-byte Ed25519 public key half of this member's signer, useful
    /// for tying an MLS member back to a `collab-types` `Identity` (the
    /// broker maps identity pubkey → member-of-channel state).
    pub fn signature_public_key(&self) -> &[u8] {
        self.signer.public()
    }

    /// Construct the `CredentialWithKey` openmls APIs accept. Crate-internal.
    pub(crate) fn credential_with_key(&self) -> CredentialWithKey {
        let credential = BasicCredential::new(self.name.clone());
        CredentialWithKey {
            credential: credential.into(),
            signature_key: self.signer.public().into(),
        }
    }

    /// Mint a fresh `KeyPackage` for this member — ships out-of-band to a
    /// channel owner for [`ChannelGroup::add_member`](crate::ChannelGroup::add_member).
    /// Binds the ciphersuite/protocol version, the member's signature public
    /// key, and a fresh HPKE keypair (private half in this `MemberKey`'s
    /// provider).
    ///
    /// Each call mints a **fresh** HPKE keypair — calling this twice lets
    /// the member join two different groups, but reusing one key package
    /// twice violates MLS (last-resort key packages aren't enabled here).
    /// Returns the openmls type directly since callers ship it straight to
    /// a Welcome issuer.
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
        // Never print the signing key, HPKE storage, or provider internals —
        // the name + signer's public half are enough to disambiguate in logs.
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
