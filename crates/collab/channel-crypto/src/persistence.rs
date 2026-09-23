//! Vault-key-sealed persistence for endpoint MLS state.
//!
//! The caller supplies a 256-bit key obtained from the platform secret service.
//! This module never creates a key beside the state file and never falls back to
//! plaintext. Writes use an owner-only temporary file, `fsync`, and atomic rename.

use std::fs;
use std::path::Path;

use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use openmls::prelude::{GroupId, MlsGroup, SignatureScheme};
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize as _, Zeroizing};

use crate::crypto::MemberKey;
use crate::{ChannelGroup, Error, Result};

const MAGIC: &[u8; 5] = b"QMLSG";
const FORMAT: u8 = 1;
const NONCE_LEN: usize = 24;
const FORMAT_OFFSET: usize = 5;
const NONCE_START: usize = 6;
const NONCE_END: usize = 30;
const HEADER_LEN: usize = 30;
const MAX_SEALED_STATE: u64 = 64 * 1024 * 1024;

/// A vault-derived state-encryption key. It is zeroized on drop and its
/// `Debug` representation never reveals bytes.
pub struct VaultKey([u8; 32]);

impl VaultKey {
    /// Wrap key bytes returned by a platform secret service or hardware vault.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(**redacted**)")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateV1 {
    channel_id: collab_types::ChannelId,
    member_name: Vec<u8>,
    signer: Vec<u8>,
    group_id: Vec<u8>,
    epoch: u64,
    storage: Vec<(Vec<u8>, Vec<u8>)>,
}

impl StateV1 {
    fn zeroize_secret_material(&mut self) {
        self.member_name.zeroize();
        self.signer.zeroize();
        self.group_id.zeroize();
        for (key, value) in &mut self.storage {
            key.zeroize();
            value.zeroize();
        }
        self.storage.clear();
    }
}

impl ChannelGroup {
    /// Atomically persist all MLS group, ratchet, key-package, and signer
    /// state sealed under `key`.
    ///
    /// The operation fails closed: no key means this API cannot be called,
    /// and serialization/encryption/I/O failures leave the previous file in
    /// place. The parent directory must be private and is created mode 0700
    /// on Unix when absent.
    ///
    /// # Errors
    ///
    /// Returns an explicit serialization, encryption, lock, or I/O error.
    pub fn save_sealed(&self, path: &Path, key: &VaultKey) -> Result<()> {
        let storage = self
            .member
            .provider
            .storage()
            .values
            .read()
            .map_err(|_| Error::InvalidState("OpenMLS storage lock poisoned".into()))?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let signer = postcard::to_allocvec(&self.member.signer)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        let mut state = StateV1 {
            channel_id: self.channel_id,
            member_name: self.member.name.clone(),
            signer,
            group_id: self.group.group_id().as_slice().to_vec(),
            epoch: self.epoch(),
            storage,
        };
        let encoded = postcard::to_allocvec(&state);
        state.zeroize_secret_material();
        let plaintext =
            Zeroizing::new(encoded.map_err(|error| Error::Serialization(error.to_string()))?);
        let cipher = XChaCha20Poly1305::new_from_slice(&key.0)
            .map_err(|_| Error::StateCrypto("invalid vault key"))?;
        let mut nonce = [0_u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ciphertext = cipher
            .encrypt(
                <&XNonce>::from(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: MAGIC,
                },
            )
            .map_err(|_| Error::StateCrypto("state encryption failed"))?;

        let sealed = [&MAGIC[..], &[FORMAT], &nonce, &ciphertext].concat();
        atomic_file::write_private(path, &sealed).map_err(Error::Io)
    }

    /// Restore a group previously written by [`Self::save_sealed`]. Wrong
    /// keys, tampering, truncation, unknown versions, and missing `OpenMLS`
    /// records are explicit errors; a fresh group is never substituted.
    ///
    /// # Errors
    ///
    /// Returns an explicit authentication, decoding, consistency, or I/O
    /// error. It never substitutes a new group.
    pub fn load_sealed(path: &Path, key: &VaultKey) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        if metadata.len() > MAX_SEALED_STATE {
            return Err(Error::InvalidState("sealed state exceeds 64 MiB".into()));
        }
        let bytes = fs::read(path)?;
        if bytes.len() <= HEADER_LEN || bytes.get(..MAGIC.len()) != Some(MAGIC) {
            return Err(Error::InvalidState("bad sealed-state header".into()));
        }
        if bytes.get(FORMAT_OFFSET).copied() != Some(FORMAT) {
            return Err(Error::InvalidState(
                "unsupported sealed-state version".into(),
            ));
        }
        let nonce = bytes
            .get(NONCE_START..NONCE_END)
            .ok_or_else(|| Error::InvalidState("truncated sealed-state nonce".into()))?;
        let ciphertext = bytes
            .get(NONCE_END..)
            .ok_or_else(|| Error::InvalidState("truncated sealed-state ciphertext".into()))?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key.0)
            .map_err(|_| Error::StateCrypto("invalid vault key"))?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    <&XNonce>::try_from(nonce)
                        .map_err(|_| Error::InvalidState("truncated sealed-state nonce".into()))?,
                    Payload {
                        msg: ciphertext,
                        aad: MAGIC,
                    },
                )
                .map_err(|_| Error::StateCrypto("state authentication failed"))?,
        );
        let mut state: StateV1 = postcard::from_bytes(&plaintext)
            .map_err(|error| Error::Serialization(error.to_string()))?;

        let provider = OpenMlsRustCrypto::default();
        {
            let mut values = provider
                .storage()
                .values
                .write()
                .map_err(|_| Error::InvalidState("OpenMLS storage lock poisoned".into()))?;
            for (storage_key, value) in &state.storage {
                if values.insert(storage_key.clone(), value.clone()).is_some() {
                    return Err(Error::InvalidState("duplicate OpenMLS storage key".into()));
                }
            }
        }
        let signer: SignatureKeyPair = postcard::from_bytes(&state.signer)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        let stored_signer = SignatureKeyPair::read(
            provider.storage(),
            signer.public(),
            SignatureScheme::ED25519,
        )
        .ok_or_else(|| Error::InvalidState("signer is absent from OpenMLS storage".into()))?;
        if stored_signer.public() != signer.public() {
            return Err(Error::InvalidState("signer/storage mismatch".into()));
        }
        let group_id = GroupId::from_slice(&state.group_id);
        let group = MlsGroup::load(provider.storage(), &group_id)
            .map_err(Error::storage)?
            .ok_or_else(|| Error::InvalidState("group is absent from OpenMLS storage".into()))?;
        if group.epoch().as_u64() != state.epoch {
            return Err(Error::InvalidState(
                "group epoch does not match sealed manifest".into(),
            ));
        }
        let channel_id = state.channel_id;
        let member_name = state.member_name.clone();
        state.zeroize_secret_material();
        Ok(Self {
            channel_id,
            member: MemberKey {
                name: member_name,
                signer,
                provider,
            },
            group,
        })
    }
}
