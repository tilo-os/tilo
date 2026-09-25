//! Vault-key-sealed persistence for endpoint MLS state.
//!
//! The caller supplies a 256-bit key obtained from the platform secret service.
//! This module never creates a key beside the state file and never falls back to
//! plaintext. Writes use an owner-only temporary file, `fsync`, and atomic rename.
//!
//! Current files are `tilo-seal` at-rest envelopes
//! ([`tilo_seal::Label::ChannelMlsState`]) holding the `OpenMLS` provider
//! storage, signer, group id, epoch and the member's content keyring.
//! Files from earlier builds (`"QMLSG" || 0x01 || nonce || ciphertext`,
//! XChaCha20-Poly1305 with the magic as associated data, no keyring) still
//! load; their keyring starts at the loaded epoch, and the next save writes
//! the current format.

use std::fs;
use std::path::Path;

use openmls::prelude::{GroupId, MlsGroup, SignatureScheme};
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, SymmetricKey};
use zeroize::{Zeroize as _, Zeroizing};

use crate::content::ContentKeyring;
use crate::crypto::MemberKey;
use crate::{ChannelGroup, Error, Result};

const LEGACY_MAGIC: &[u8; 5] = b"QMLSG";
const LEGACY_FORMAT: u8 = 1;
const LEGACY_NONCE_START: usize = 6;
const LEGACY_NONCE_END: usize = 30;
const STATE_VERSION: u8 = 2;
const MAX_SEALED_STATE: u64 = 64 * 1024 * 1024;
/// Record bound into every state envelope, so the file cannot be swapped
/// with another sealed value under the same vault key.
const STATE_RECORD: &[u8] = b"channel-crypto/member-state";

/// A vault-derived state-encryption key. It is zeroized on drop and its
/// `Debug` representation never reveals bytes.
pub struct VaultKey(SymmetricKey);

impl VaultKey {
    /// Wrap key bytes returned by a platform secret service or hardware vault.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(SymmetricKey::from_bytes(bytes))
    }

    /// Wrap an existing symmetric key.
    pub fn from_key(key: SymmetricKey) -> Self {
        Self(key)
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateV2 {
    version: u8,
    mls: StateV1,
    content_keys: Vec<(u64, [u8; 32])>,
}

impl StateV2 {
    fn zeroize_secret_material(&mut self) {
        self.mls.zeroize_secret_material();
        for (_, key) in &mut self.content_keys {
            key.zeroize();
        }
        self.content_keys.clear();
    }
}

impl ChannelGroup {
    /// Atomically persist all MLS group, ratchet, key-package, signer and
    /// content-key state sealed under `key`.
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
        let mut state = StateV2 {
            version: STATE_VERSION,
            mls: StateV1 {
                channel_id: self.channel_id,
                member_name: self.member.name.clone(),
                signer,
                group_id: self.group.group_id().as_slice().to_vec(),
                epoch: self.epoch(),
                storage,
            },
            content_keys: self.keyring.to_wire(),
        };
        let encoded = postcard::to_allocvec(&state);
        state.zeroize_secret_material();
        let plaintext =
            Zeroizing::new(encoded.map_err(|error| Error::Serialization(error.to_string()))?);
        let sealed = tilo_seal::envelope::seal(
            &key.0,
            Context::new(Label::ChannelMlsState, STATE_RECORD),
            &plaintext,
        )
        .map_err(|_| Error::StateCrypto("state encryption failed"))?;
        atomic_file::write_private(path, &sealed).map_err(Error::Io)
    }

    /// Restore a group previously written by [`Self::save_sealed`], by this
    /// build or an earlier one. Wrong keys, tampering, truncation, unknown
    /// versions, and missing `OpenMLS` records are explicit errors; a fresh
    /// group is never substituted.
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
        if tilo_seal::envelope::is_envelope(&bytes) {
            let plaintext = tilo_seal::envelope::open(
                &key.0,
                Context::new(Label::ChannelMlsState, STATE_RECORD),
                &bytes,
            )
            .map_err(|_| Error::StateCrypto("state authentication failed"))?;
            let mut state: StateV2 = postcard::from_bytes(&plaintext)
                .map_err(|error| Error::Serialization(error.to_string()))?;
            if state.version != STATE_VERSION {
                state.zeroize_secret_material();
                return Err(Error::InvalidState(
                    "unsupported sealed-state version".into(),
                ));
            }
            let keyring = ContentKeyring::from_wire(std::mem::take(&mut state.content_keys));
            let restored = Self::restore(&state.mls, keyring.ok());
            state.zeroize_secret_material();
            return restored;
        }
        let plaintext = open_legacy(&bytes, &key.0)?;
        let mut state: StateV1 = postcard::from_bytes(&plaintext)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        let restored = Self::restore(&state, Some(ContentKeyring::new()));
        state.zeroize_secret_material();
        let mut group = restored?;
        // Earlier builds kept no content keys: start the keyring at the
        // loaded epoch.
        group.record_epoch_key()?;
        Ok(group)
    }

    fn restore(state: &StateV1, keyring: Option<ContentKeyring>) -> Result<Self> {
        let keyring =
            keyring.ok_or_else(|| Error::InvalidState("conflicting stored content keys".into()))?;
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
        Ok(Self {
            channel_id: state.channel_id,
            member: MemberKey {
                name: state.member_name.clone(),
                signer,
                provider,
            },
            group,
            keyring,
        })
    }
}

/// Open a state file written before `tilo-seal`: `"QMLSG" || 0x01 ||
/// nonce(24) || XChaCha20-Poly1305 ciphertext`, with the magic as AAD.
fn open_legacy(bytes: &[u8], key: &SymmetricKey) -> Result<Zeroizing<Vec<u8>>> {
    if bytes.len() <= LEGACY_NONCE_END || bytes.get(..LEGACY_MAGIC.len()) != Some(LEGACY_MAGIC) {
        return Err(Error::InvalidState("bad sealed-state header".into()));
    }
    if bytes.get(LEGACY_MAGIC.len()).copied() != Some(LEGACY_FORMAT) {
        return Err(Error::InvalidState(
            "unsupported sealed-state version".into(),
        ));
    }
    let nonce = bytes
        .get(LEGACY_NONCE_START..LEGACY_NONCE_END)
        .and_then(|nonce| <&[u8; 24]>::try_from(nonce).ok())
        .ok_or_else(|| Error::InvalidState("truncated sealed-state nonce".into()))?;
    let ciphertext = bytes
        .get(LEGACY_NONCE_END..)
        .ok_or_else(|| Error::InvalidState("truncated sealed-state ciphertext".into()))?;
    tilo_seal::legacy::open_xchacha20poly1305(key, nonce, LEGACY_MAGIC, ciphertext)
        .map_err(|_| Error::StateCrypto("state authentication failed"))
}
