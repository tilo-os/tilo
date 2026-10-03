//! Vault-key-sealed persistence for endpoint MLS state.
//!
//! The caller supplies a 256-bit key obtained from the platform secret service.
//! This module never creates a key beside the state file and never falls back to
//! plaintext. Writes use an owner-only temporary file, `fsync`, and atomic rename.
//!
//! Current files are `tilo-seal` at-rest envelopes
//! ([`tilo_seal::Label::ChannelMlsState`]) holding the `OpenMLS` provider
//! storage, signer, group id, epoch and the member's content keyring.

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
        let sealed = self.seal(key)?;
        tilo_files::native::atomic::write_private(path, &sealed).map_err(Error::Io)
    }

    /// This group's state sealed under `key`, as [`Self::save_sealed`]
    /// writes it, for a store that is not a file.
    ///
    /// # Errors
    ///
    /// Returns an explicit serialization or encryption error.
    pub fn seal(&self, key: &VaultKey) -> Result<Vec<u8>> {
        let mut state = self.snapshot()?;
        let encoded = postcard::to_allocvec(&state);
        state.zeroize_secret_material();
        let plaintext =
            Zeroizing::new(encoded.map_err(|error| Error::Serialization(error.to_string()))?);
        tilo_seal::envelope::seal(
            &key.0,
            Context::new(Label::ChannelMlsState, STATE_RECORD),
            &plaintext,
        )
        .map_err(|_| Error::StateCrypto("state encryption failed"))
    }

    fn snapshot(&self) -> Result<StateV2> {
        let (member_name, signer, storage) = self.member.snapshot()?;
        Ok(StateV2 {
            version: STATE_VERSION,
            mls: StateV1 {
                channel_id: self.channel_id,
                member_name,
                signer,
                group_id: self.group.group_id().as_slice().to_vec(),
                epoch: self.epoch(),
                storage,
            },
            content_keys: self.keyring.to_wire(),
        })
    }

    fn restore_v2(mut state: StateV2) -> Result<Self> {
        if state.version != STATE_VERSION {
            state.zeroize_secret_material();
            return Err(Error::InvalidState(
                "unsupported sealed-state version".into(),
            ));
        }
        let keyring = ContentKeyring::from_wire(std::mem::take(&mut state.content_keys));
        let restored = Self::restore(&state.mls, keyring.ok());
        state.zeroize_secret_material();
        restored
    }

    /// Restore a group previously written by [`Self::save_sealed`]. Wrong keys, tampering, truncation, unknown
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
        Self::open_sealed(&fs::read(path)?, key)
    }

    /// Restore a group from what [`Self::seal`] produced, with the checks of
    /// [`Self::load_sealed`].
    ///
    /// # Errors
    ///
    /// As [`Self::load_sealed`].
    pub fn open_sealed(sealed: &[u8], key: &VaultKey) -> Result<Self> {
        if u64::try_from(sealed.len()).unwrap_or(u64::MAX) > MAX_SEALED_STATE {
            return Err(Error::InvalidState("sealed state exceeds 64 MiB".into()));
        }
        let plaintext = tilo_seal::envelope::open(
            &key.0,
            Context::new(Label::ChannelMlsState, STATE_RECORD),
            sealed,
        )
        .map_err(|_| Error::StateCrypto("state authentication failed"))?;
        let state: StateV2 = postcard::from_bytes(&plaintext)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        Self::restore_v2(state)
    }

    fn restore(state: &StateV1, keyring: Option<ContentKeyring>) -> Result<Self> {
        let keyring =
            keyring.ok_or_else(|| Error::InvalidState("conflicting stored content keys".into()))?;
        let member = MemberKey::restore(&state.member_name, &state.signer, &state.storage)?;
        let group_id = GroupId::from_slice(&state.group_id);
        let group = MlsGroup::load(member.provider.storage(), &group_id)
            .map_err(Error::storage)?
            .ok_or_else(|| Error::InvalidState("group is absent from OpenMLS storage".into()))?;
        if group.epoch().as_u64() != state.epoch {
            return Err(Error::InvalidState(
                "group epoch does not match sealed manifest".into(),
            ));
        }
        Ok(Self {
            channel_id: state.channel_id,
            member,
            group,
            keyring,
        })
    }
}

/// A member key's name, encoded signer and provider storage.
type MemberSnapshot = (Vec<u8>, Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>);

impl MemberKey {
    fn snapshot(&self) -> Result<MemberSnapshot> {
        let storage = self
            .provider
            .storage()
            .values
            .read()
            .map_err(|_| Error::InvalidState("OpenMLS storage lock poisoned".into()))?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let signer = postcard::to_allocvec(&self.signer)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        Ok((self.name.clone(), signer, storage))
    }

    fn restore(name: &[u8], signer: &[u8], storage: &[(Vec<u8>, Vec<u8>)]) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();
        {
            let mut values = provider
                .storage()
                .values
                .write()
                .map_err(|_| Error::InvalidState("OpenMLS storage lock poisoned".into()))?;
            for (storage_key, value) in storage {
                if values.insert(storage_key.clone(), value.clone()).is_some() {
                    return Err(Error::InvalidState("duplicate OpenMLS storage key".into()));
                }
            }
        }
        let signer: SignatureKeyPair = postcard::from_bytes(signer)
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
        Ok(Self {
            name: name.to_vec(),
            signer,
            provider,
        })
    }

    /// An independent copy of this member key: same signer, same stored
    /// key-package secrets. Used to keep a pending member usable when a
    /// Welcome addressed to it fails to process.
    pub(crate) fn duplicate(&self) -> Result<Self> {
        let (name, mut signer, mut storage) = self.snapshot()?;
        let copy = Self::restore(&name, &signer, &storage);
        signer.zeroize();
        for (key, value) in &mut storage {
            key.zeroize();
            value.zeroize();
        }
        copy
    }
}

/// Record bound into every device member-state envelope.
const MEMBER_RECORD: &[u8] = b"channel-crypto/device-member";
const MEMBER_RECORD_VERSION: u8 = 1;

/// A device's durable state for one channel it takes part in without
/// stewarding it.
pub(crate) enum MemberRecord {
    /// Asked for access; holds the member key whose key packages are out.
    Waiting(Box<MemberKey>),
    /// Admitted.
    Joined(Box<ChannelGroup>),
    /// Removed by the steward.
    Removed,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum MemberRecordWire {
    Waiting {
        name: Vec<u8>,
        signer: Vec<u8>,
        storage: Vec<(Vec<u8>, Vec<u8>)>,
    },
    Joined(StateV2),
    Removed,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberFile {
    version: u8,
    record: MemberRecordWire,
}

impl MemberRecordWire {
    fn zeroize_secret_material(&mut self) {
        match self {
            Self::Waiting {
                name,
                signer,
                storage,
            } => {
                name.zeroize();
                signer.zeroize();
                for (key, value) in storage.iter_mut() {
                    key.zeroize();
                    value.zeroize();
                }
                storage.clear();
            }
            Self::Joined(state) => state.zeroize_secret_material(),
            Self::Removed => {}
        }
    }
}

impl MemberRecord {
    pub(crate) fn save_sealed(&self, path: &Path, key: &VaultKey) -> Result<()> {
        let sealed = self.seal(key)?;
        tilo_files::native::atomic::write_private(path, &sealed).map_err(Error::Io)
    }

    pub(crate) fn load_sealed(path: &Path, key: &VaultKey) -> Result<Self> {
        if fs::metadata(path)?.len() > MAX_SEALED_STATE {
            return Err(Error::InvalidState("sealed state exceeds 64 MiB".into()));
        }
        Self::open_sealed(&fs::read(path)?, key)
    }

    /// This record sealed under `key`, as [`Self::save_sealed`] writes it.
    pub(crate) fn seal(&self, key: &VaultKey) -> Result<Vec<u8>> {
        let record = match self {
            Self::Waiting(member) => {
                let (name, signer, storage) = member.snapshot()?;
                MemberRecordWire::Waiting {
                    name,
                    signer,
                    storage,
                }
            }
            Self::Joined(group) => MemberRecordWire::Joined(group.snapshot()?),
            Self::Removed => MemberRecordWire::Removed,
        };
        let mut file = MemberFile {
            version: MEMBER_RECORD_VERSION,
            record,
        };
        let encoded = postcard::to_allocvec(&file);
        file.record.zeroize_secret_material();
        let plaintext =
            Zeroizing::new(encoded.map_err(|error| Error::Serialization(error.to_string()))?);
        tilo_seal::envelope::seal(
            &key.0,
            Context::new(Label::ChannelMlsState, MEMBER_RECORD),
            &plaintext,
        )
        .map_err(|_| Error::StateCrypto("state encryption failed"))
    }

    /// Open what [`Self::seal`] produced.
    pub(crate) fn open_sealed(sealed: &[u8], key: &VaultKey) -> Result<Self> {
        if u64::try_from(sealed.len()).unwrap_or(u64::MAX) > MAX_SEALED_STATE {
            return Err(Error::InvalidState("sealed state exceeds 64 MiB".into()));
        }
        let plaintext = tilo_seal::envelope::open(
            &key.0,
            Context::new(Label::ChannelMlsState, MEMBER_RECORD),
            sealed,
        )
        .map_err(|_| Error::StateCrypto("state authentication failed"))?;
        let file: MemberFile = postcard::from_bytes(&plaintext)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if file.version != MEMBER_RECORD_VERSION {
            let mut file = file;
            file.record.zeroize_secret_material();
            return Err(Error::InvalidState(
                "unsupported member-state version".into(),
            ));
        }
        match file.record {
            MemberRecordWire::Waiting {
                mut name,
                mut signer,
                mut storage,
            } => {
                let member = MemberKey::restore(&name, &signer, &storage);
                name.zeroize();
                signer.zeroize();
                for (key, value) in &mut storage {
                    key.zeroize();
                    value.zeroize();
                }
                member.map(Box::new).map(Self::Waiting)
            }
            MemberRecordWire::Joined(state) => ChannelGroup::restore_v2(state)
                .map(Box::new)
                .map(Self::Joined),
            MemberRecordWire::Removed => Ok(Self::Removed),
        }
    }
}
