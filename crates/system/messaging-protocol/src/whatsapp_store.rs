//! Device/account-owned encrypted SDK state. No plaintext database or key file.
//!
//! Each batch is one authenticated, sequence-checked frame, flushed before its
//! values become visible. Interrupted/corrupt logs fail closed, never reset.
//! Compaction atomically publishes an encrypted snapshot. The host must prevent
//! authorized rollback/deletion; AEAD does not prove that a backup is current.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use advisory_lock::FileLock;
use secret_vault::{VaultFormat, VaultKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use whatsapp_rust::wacore::store::error::{Result, StoreError};
use zeroize::{Zeroize, Zeroizing};

use crate::Service;
use crate::journal::Owner;

mod app_state;
mod device;
pub(crate) mod groups;
mod protocol;
mod signal_state;
#[cfg(test)]
mod tests;

const MAX_LIVE_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;

pub const STATE_FORMAT: VaultFormat = VaultFormat {
    name: "tilo.whatsapp-state.v1",
    magic: b"TMWS",
    secret_service_label: "Tilo WhatsApp device state",
    secret_service_attributes: &[
        ("application", "so.tilo.messaging-broker"),
        ("purpose", "whatsapp-state"),
    ],
    dpapi_entropy: b"tilo.whatsapp-state.v1",
};

/// SDK domain traits share one transaction lock and one device-local log.
/// Clone shares that same authority; it never creates another writer.
#[derive(Clone)]
pub struct EncryptedBackend {
    inner: Arc<Mutex<Disk>>,
    owner: Owner,
}

/// The host retains its original native credential grant on SDK state access.
/// This does not mint a credential or authorize network endpoints.
pub trait StateAuthority: Send + Sync {
    fn allowed(&self) -> bool;
}

impl EncryptedBackend {
    /// Look up an existing OS-keychain key. Linking/provisioning is separate.
    ///
    /// # Errors
    /// Refuses missing/locked keys, weaker custody and invalid existing state.
    pub fn open_system(
        root: &Path,
        owner: Owner,
        authority: Arc<dyn StateAuthority>,
    ) -> Result<Self> {
        Self::open_authorized(root, owner, authority, || {
            let store = tilo_secrets::SecretStore::for_app("so.tilo.messaging-broker");
            if store.protection() != tilo_secrets::Protection::OsKeychain {
                return Err(unavailable());
            }
            STATE_FORMAT
                .load_or_create_system_key_in(true, &store)
                .ok_or_else(unavailable)
        })
    }

    fn open_authorized(
        root: &Path,
        owner: Owner,
        authority: Arc<dyn StateAuthority>,
        lookup: impl FnOnce() -> Result<VaultKey>,
    ) -> Result<Self> {
        if !authority.allowed() {
            return Err(unavailable());
        }
        Self::open_state(root, owner, lookup()?, Some(authority))
    }

    /// Use a host's existing scoped key without provisioning or fallback.
    ///
    /// # Errors
    /// Refuses wrong key scope, absent custody and unsafe existing state.
    pub fn open_existing_in(
        root: &Path,
        owner: Owner,
        store: &tilo_secrets::SecretStore,
    ) -> Result<Self> {
        if store.app_id() != "so.tilo.messaging-broker" {
            return Err(unavailable());
        }
        let key = STATE_FORMAT
            .load_or_create_system_key_in(true, store)
            .ok_or_else(unavailable)?;
        Self::open_with_key(root, owner, key)
    }

    /// Explicit host-supplied key, for hosts and isolated fixtures.
    ///
    /// # Errors
    /// Refuses wrong service/authority, competing writers or corrupt state.
    pub fn open_with_key(root: &Path, owner: Owner, key: VaultKey) -> Result<Self> {
        Self::open_state(root, owner, key, None)
    }

    /// Open the host's original approved profile key with its original grant.
    /// The grant is installed before any store initialization and retained by
    /// every SDK reader and writer, including clones.
    /// # Errors
    /// Refuses revoked authority, competing writers or invalid encrypted state.
    pub fn open_profile(
        root: &Path,
        owner: Owner,
        key: VaultKey,
        authority: Arc<dyn StateAuthority>,
    ) -> Result<Self> {
        Self::open_authorized(root, owner, authority, || Ok(key))
    }

    fn open_state(
        root: &Path,
        owner: Owner,
        key: VaultKey,
        authority: Option<Arc<dyn StateAuthority>>,
    ) -> Result<Self> {
        if !owner.valid_principal()
            || owner.service != Service::Whatsapp
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
        {
            return Err(unavailable());
        }
        let disk = Disk::open(root, owner.clone(), key, authority)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(disk)),
            owner,
        })
    }

    /// Retain the original native account grant on every SDK state operation.
    /// The supplied key constructor alone does not establish host authority.
    /// # Errors
    /// Refuses a grant which is already revoked.
    pub fn guarded(self, authority: Arc<dyn StateAuthority>) -> Result<Self> {
        if !authority.allowed() {
            return Err(unavailable());
        }
        {
            let mut disk = self.inner.lock().map_err(|_| unavailable())?;
            if disk.authority.is_some() {
                return Err(unavailable());
            }
            disk.authority = Some(authority);
        }
        Ok(self)
    }

    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Disk) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut disk = inner.lock().map_err(|_| unavailable())?;
            disk.check_authority()?;
            if !disk.usable {
                return Err(unavailable());
            }
            let result = work(&mut disk);
            drop(disk);
            result
        })
        .await
        .map_err(|_| unavailable())?
    }

    async fn get<T: DeserializeOwned + Send + 'static>(&self, key: Key) -> Result<Option<T>> {
        self.run(move |disk| disk.get(&key)).await
    }

    async fn put<T: Serialize + Sync>(&self, key: Key, value: &T) -> Result<()> {
        let change = Change::put(key, value)?;
        self.run(move |disk| disk.commit(vec![change])).await
    }

    async fn delete(&self, key: Key) -> Result<()> {
        self.run(move |disk| disk.commit(vec![Change::delete(key)]))
            .await
    }
}

fn unavailable() -> StoreError {
    StoreError::Validation("private messaging state unavailable".into())
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    domain: String,
    parts: Vec<String>,
}

fn key(domain: &str, parts: &[&str]) -> Key {
    Key {
        domain: domain.into(),
        parts: parts.iter().map(|part| (*part).into()).collect(),
    }
}

fn binary_key(domain: &str, prefix: &[&str], bytes: &[u8]) -> Result<Key> {
    let encoded = serde_json::to_string(bytes).map_err(|_| unavailable())?;
    let mut parts: Vec<_> = prefix.iter().map(|part| (*part).to_owned()).collect();
    parts.push(encoded);
    Ok(Key {
        domain: domain.into(),
        parts,
    })
}

struct Change {
    key: Key,
    value: Option<Zeroizing<Vec<u8>>>,
}
impl Change {
    fn put<T: Serialize + Sync>(key: Key, value: &T) -> Result<Self> {
        let value = Zeroizing::new(serde_json::to_vec(value).map_err(|_| unavailable())?);
        Ok(Self {
            key,
            value: Some(value),
        })
    }
    fn delete(key: Key) -> Self {
        Self { key, value: None }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    key: Key,
    value: Option<String>,
}
impl Drop for Row {
    fn drop(&mut self) {
        if let Some(value) = &mut self.value {
            value.zeroize();
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    sequence: u64,
    owner: Owner,
    rows: Vec<Row>,
}

struct Disk {
    path: PathBuf,
    key: VaultKey,
    owner: Owner,
    values: BTreeMap<Key, Zeroizing<Vec<u8>>>,
    sequence: u64,
    log_bytes: u64,
    live_bytes: usize,
    usable: bool,
    authority: Option<Arc<dyn StateAuthority>>,
    _lock: FileLock,
}

impl Disk {
    fn open(
        root: &Path,
        owner: Owner,
        key: VaultKey,
        authority: Option<Arc<dyn StateAuthority>>,
    ) -> Result<Self> {
        if let Some(grant) = &authority
            && !grant.allowed()
        {
            return Err(unavailable());
        }
        private_fs::directory(root).map_err(|_| unavailable())?;
        let lock = private_fs::lock_file(&root.join("owner.lock")).map_err(|_| unavailable())?;
        let lock = FileLock::try_exclusive(lock).map_err(|_| unavailable())?;
        let mut disk = Self {
            path: root.join("state.enc"),
            key,
            owner,
            values: BTreeMap::new(),
            sequence: 0,
            log_bytes: 0,
            live_bytes: 0,
            usable: true,
            authority,
            _lock: lock,
        };
        match private_fs::open_owned_for_read(&disk.path, MAX_LOG_BYTES) {
            Ok((mut file, len)) => {
                if len == 0 {
                    return Err(unavailable());
                }
                let mut remaining = len;
                let mut expected = 0_u64;
                while remaining > 0 {
                    let mut prefix = [0; 4];
                    file.read_exact(&mut prefix).map_err(|_| unavailable())?;
                    let length =
                        usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| unavailable())?;
                    remaining = remaining.checked_sub(4).ok_or_else(unavailable)?;
                    if length == 0 || length > MAX_FRAME_BYTES || length as u64 > remaining {
                        return Err(unavailable());
                    }
                    let mut sealed = vec![0; length];
                    file.read_exact(&mut sealed).map_err(|_| unavailable())?;
                    let plain = STATE_FORMAT
                        .open(&disk.key, &sealed)
                        .ok_or_else(unavailable)?;
                    let mut frame: Frame =
                        serde_json::from_slice(&plain).map_err(|_| unavailable())?;
                    if frame.owner != disk.owner || frame.sequence != expected {
                        return Err(unavailable());
                    }
                    let mut changes = Vec::with_capacity(frame.rows.len());
                    for row in &mut frame.rows {
                        changes.push(Change {
                            key: row.key.clone(),
                            value: row
                                .value
                                .take()
                                .map(|value| Zeroizing::new(value.into_bytes())),
                        });
                    }
                    disk.apply(changes);
                    disk.validate_live()?;
                    disk.sequence = expected;
                    expected = expected.checked_add(1).ok_or_else(unavailable)?;
                    remaining = remaining
                        .checked_sub(length as u64)
                        .ok_or_else(unavailable)?;
                }
                disk.log_bytes = len;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                disk.check_authority()?;
                let initial = disk.encode(0, &[])?;
                if !private_fs::create_atomic(&disk.path, &initial).map_err(|_| unavailable())? {
                    return Err(unavailable());
                }
                disk.log_bytes = initial.len() as u64;
            }
            Err(_) => return Err(unavailable()),
        }
        disk.check_authority()?;
        Ok(disk)
    }

    fn check_authority(&self) -> Result<()> {
        if let Some(authority) = &self.authority
            && !authority.allowed()
        {
            return Err(unavailable());
        }
        Ok(())
    }

    fn get<T: DeserializeOwned>(&self, key: &Key) -> Result<Option<T>> {
        self.values
            .get(key)
            .map(|value| serde_json::from_slice(value).map_err(|_| unavailable()))
            .transpose()
    }

    fn rows<T: DeserializeOwned>(&self, domain: &str, prefix: &[&str]) -> Result<Vec<(Key, T)>> {
        self.values
            .iter()
            .filter(|(key, _)| {
                key.domain == domain
                    && key
                        .parts
                        .iter()
                        .zip(prefix)
                        .all(|(stored, expected)| stored == expected)
                    && key.parts.len() >= prefix.len()
            })
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    serde_json::from_slice(value).map_err(|_| unavailable())?,
                ))
            })
            .collect()
    }

    fn commit(&mut self, changes: Vec<Change>) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        // Collapse only this batch, not the whole ratchet store. Ordinary
        // writes encode/flush changed rows; no full-state clone per message.
        let changes: BTreeMap<_, _> = changes
            .into_iter()
            .map(|change| (change.key, change.value))
            .collect();
        let changes: Vec<_> = changes
            .into_iter()
            .map(|(key, value)| Change { key, value })
            .collect();
        let mut live = self.live_bytes;
        let mut count = self.values.len();
        for change in &changes {
            validate_key(&change.key)?;
            if let Some(old) = self.values.get(&change.key) {
                live = live.saturating_sub(row_bytes(&change.key, old));
                count = count.saturating_sub(1);
            }
            if let Some(value) = &change.value {
                live = live.saturating_add(row_bytes(&change.key, value));
                count = count.saturating_add(1);
            }
        }
        if live > MAX_LIVE_BYTES || count > MAX_ROWS {
            return Err(unavailable());
        }
        let sequence = self.sequence.checked_add(1).ok_or_else(unavailable)?;
        let encoded = self.encode(sequence, &changes)?;
        let compact_at = (live.saturating_mul(4)).max(4 * 1024 * 1024) as u64;
        let outcome = if self.log_bytes.saturating_add(encoded.len() as u64) > compact_at {
            let mut snapshot = self.values.clone();
            for change in &changes {
                if let Some(value) = &change.value {
                    snapshot.insert(change.key.clone(), value.clone());
                } else {
                    snapshot.remove(&change.key);
                }
            }
            let snapshot: Vec<_> = snapshot
                .into_iter()
                .map(|(key, value)| Change {
                    key,
                    value: Some(value),
                })
                .collect();
            let encoded = self.encode(0, &snapshot)?;
            private_fs::write_atomic(&self.path, &encoded).map(|()| (0, encoded.len() as u64))
        } else {
            (|| {
                let mut file = private_fs::lock_file(&self.path)?;
                if file.metadata()?.len() != self.log_bytes {
                    return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
                }
                file.seek(SeekFrom::End(0))?;
                file.write_all(&encoded)?;
                file.sync_all()?;
                Ok((
                    sequence,
                    self.log_bytes.saturating_add(encoded.len() as u64),
                ))
            })()
        };
        if let Ok((sequence, bytes)) = outcome {
            self.apply(changes);
            self.sequence = sequence;
            self.log_bytes = bytes;
            Ok(())
        } else {
            self.usable = false;
            Err(unavailable())
        }
    }

    fn encode(&self, sequence: u64, changes: &[Change]) -> Result<Vec<u8>> {
        let rows = changes
            .iter()
            .map(|change| {
                Ok(Row {
                    key: change.key.clone(),
                    value: change
                        .value
                        .as_ref()
                        .map(|value| {
                            std::str::from_utf8(value)
                                .map(str::to_owned)
                                .map_err(|_| unavailable())
                        })
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let plain = Zeroizing::new(
            serde_json::to_vec(&Frame {
                sequence,
                owner: self.owner.clone(),
                rows,
            })
            .map_err(|_| unavailable())?,
        );
        let sealed = STATE_FORMAT
            .seal(&self.key, &plain)
            .ok_or_else(unavailable)?;
        if sealed.len() > MAX_FRAME_BYTES {
            return Err(unavailable());
        }
        let length = u32::try_from(sealed.len()).map_err(|_| unavailable())?;
        let mut encoded = Vec::with_capacity(sealed.len().saturating_add(4));
        encoded.extend_from_slice(&length.to_le_bytes());
        encoded.extend_from_slice(&sealed);
        Ok(encoded)
    }

    fn apply(&mut self, changes: Vec<Change>) {
        for change in changes {
            if let Some(old) = self.values.remove(&change.key) {
                self.live_bytes = self.live_bytes.saturating_sub(row_bytes(&change.key, &old));
            }
            if let Some(value) = change.value {
                self.live_bytes = self
                    .live_bytes
                    .saturating_add(row_bytes(&change.key, &value));
                self.values.insert(change.key, value);
            }
        }
    }
    fn validate_live(&self) -> Result<()> {
        validate(&self.values)
    }
}

fn live_bytes(values: &BTreeMap<Key, Zeroizing<Vec<u8>>>) -> usize {
    values.iter().fold(0_usize, |size, (key, value)| {
        size.saturating_add(row_bytes(key, value))
    })
}

fn row_bytes(key: &Key, value: &[u8]) -> usize {
    key.domain
        .len()
        .saturating_add(key.parts.iter().map(String::len).sum::<usize>())
        .saturating_add(value.len())
}

fn validate_key(key: &Key) -> Result<()> {
    if key.domain.len() > 32
        || key.parts.len() > 8
        || key.parts.iter().any(|part| part.len() > 2048)
    {
        return Err(unavailable());
    }
    Ok(())
}

fn validate(values: &BTreeMap<Key, Zeroizing<Vec<u8>>>) -> Result<()> {
    if values.len() > MAX_ROWS
        || live_bytes(values) > MAX_LIVE_BYTES
        || values.keys().any(|key| {
            key.domain.len() > 32
                || key.parts.len() > 8
                || key.parts.iter().any(|part| part.len() > 2048)
        })
    {
        return Err(unavailable());
    }
    Ok(())
}
