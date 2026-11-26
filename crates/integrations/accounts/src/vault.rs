//! The encrypted **secret vault** — credentials at rest, never in the accounts
//! record ([`crate::person::AccountList`]).
//!
//! Every credential an [`crate::Account`] needs is a long-lived bearer secret, sealed with
//! **XChaCha20-Poly1305** (192-bit-nonce AEAD, audited `RustCrypto` stack) under a 256-bit key,
//! stored as `vault.enc` under `$XDG_CONFIG_HOME/tilo/accounts/vault/`. A fresh random nonce
//! is drawn per write, so re-saving an unchanged vault still produces distinct ciphertext and
//! no nonce is ever reused. Sealing and key storage live in the `secret-vault` crate, shared
//! with the authenticator; this module owns only the account vault's format identity.
//!
//! # Key ownership
//!
//! Windows protects keys with current-user DPAPI and private file ACLs, including
//! explicit-root stores. Existing private plaintext keys migrate only after
//! authenticated restoration, and an existing key that becomes unavailable is
//! never replaced.
//! The person's own store has one key storage on every host Tilo runs on:
//! [`SecretVault::open`] holds the key in the platform's secret store
//! (`tilo-secrets`: the Secret Service on Linux, the Keychain on macOS) where
//! one answers, and otherwise in an owner-only `vault.key` beside the
//! ciphertext. In an app process, that store is the workspace's vault: the key
//! is an item the app keeps (`vault#keep`), found and released back to it over
//! the connection its launcher handed it, so the person's other devices open
//! the same account vault. A host process keeps it in the operating system's
//! keychain. An owner-only key left by that fallback migrates into the
//! secret store once it answers, after a verified round trip.
//! [`SecretVault::open_in`] opens a store of someone else's root (a capture,
//! an imported copy, a test's) under its own private key, so such a store
//! never reads or replays the person's key.
//!
//! Failed loads are recorded for callers and prevent saves until recovery and
//! reopen. Saves revalidate existing ciphertext before replacement; a missing
//! key never authorizes re-keying an existing vault. The convenience `save`
//! method logs errors, while `try_save` returns them to the caller.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use secret_vault::{VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::AccountId;

pub use secret_vault::VaultBackend;

/// One account's secret material. Kept out of the plaintext account store entirely — it
/// only ever lives in the sealed vault (or in memory while an app is connecting).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// An `OAuth2` token bundle (serialised by the owning type) for accounts that sign in instead
    /// of using a password. Unused by current consumers but reserved so the vault shape doesn't churn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret").finish_non_exhaustive()
    }
}

impl Secret {
    pub fn password(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
            token: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.password.is_empty() && self.token.is_none()
    }
}

/// The persisted identity of the account vault. Every value here is part of
/// the on-disk format and of where its key is held.
const FORMAT: VaultFormat = VaultFormat {
    name: "accounts",
    magic: b"QASV",
    secret_service_label: "Tilo account credentials",
    secret_service_attributes: &[("application", "Tilo"), ("purpose", "account-vault")],
    dpapi_entropy: b"Tilo Accounts vault key v1",
};

type PendingEdits = BTreeMap<String, (Option<Secret>, Option<Secret>)>;

/// The encrypted, per-[`AccountId`] credential store. Held in memory after
/// [`SecretVault::open`]; mutations are staged and written by [`SecretVault::save`].
#[derive(Debug)]
pub struct SecretVault {
    dir: PathBuf,
    entries: BTreeMap<String, Secret>,
    // Explicit edits merge into the latest inventory while the write lock is held.
    pending: std::sync::Mutex<PendingEdits>,
    /// A write on a thread of its own, for a caller that must not wait on
    /// the disk ([`Self::commit`] on the compositor's frame loop).
    native_in_flight: std::sync::Mutex<Option<NativeWrite>>,
    backend: VaultBackend,
    load_error: Option<String>,
    read_only: bool,
    /// The file the entries were read from, as it was then: another process
    /// that saved since leaves a different one, and [`Self::refresh`] reads
    /// it again.
    stamp: Option<FileStamp>,
}

/// What tells one written `vault.enc` from the next: every save replaces
/// the file atomically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

impl FileStamp {
    fn of(dir: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(dir.join("vault.enc")).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: std::os::unix::fs::MetadataExt::ino(&metadata),
        })
    }
}

pub(crate) fn explicit_root_backend() -> VaultBackend {
    #[cfg(windows)]
    {
        VaultBackend::WindowsDpapi
    }
    #[cfg(not(windows))]
    {
        VaultBackend::LocalFile
    }
}
impl SecretVault {
    /// Open the vault at the default location (`$XDG_CONFIG_HOME/tilo/accounts/vault`). An empty
    /// vault when nothing is stored. Load failures yield an empty view with
    /// [`Self::recovery_error`] set and writes disabled until recovery and reopen.
    ///
    /// This is the person's store, whatever holds its key: the platform's
    /// secret store where it answers, otherwise an owner-only key file
    /// beside the ciphertext ([`VaultBackend::for_session`]).
    pub fn open() -> Self {
        match crate::config_root() {
            Some(root) => {
                let dir = crate::vault_dir_in(&root);
                Self::open_at(dir, VaultBackend::for_session())
            }
            // A browser page has no config directory, and keeps no secrets of its own: they are
            // entries of the workspace's vault. Its store holds nothing and takes no write.
            None => Self {
                dir: PathBuf::from("accounts/vault"),
                entries: BTreeMap::new(),
                pending: std::sync::Mutex::default(),
                native_in_flight: std::sync::Mutex::default(),
                backend: explicit_root_backend(),
                stamp: None,
                read_only: true,
                load_error: None,
            },
        }
    }

    /// Open the vault `config_root` names. The person's own root is the
    /// person's store, opened as [`Self::open`] opens it. Any other root (a
    /// capture, an imported copy, a test's) is a store of its own under its
    /// own private key, so it never reads or replays the person's.
    pub fn open_person(config_root: Option<&Path>) -> Self {
        match config_root {
            Some(root) if crate::config_root().as_deref() != Some(root) => Self::open_in(root),
            _ => Self::open(),
        }
    }

    /// Open the vault under an explicit config root. Windows uses the same
    /// DPAPI ownership as the default store; other hosts use a private local key.
    pub fn open_in(config_root: &Path) -> Self {
        let dir = crate::vault_dir_in(config_root);
        let backend = explicit_root_backend();
        Self::open_at(dir, backend)
    }

    /// Read an existing explicit-root vault without changing its source ownership.
    /// Missing ciphertext or keys are recovery errors. This view cannot be saved,
    /// including after in-memory edits; it never creates or restricts source files.
    pub fn open_read_only_in(config_root: &Path) -> Self {
        let dir = crate::vault_dir_in(config_root);
        let backend = explicit_root_backend();
        Self::open_read_only_at(dir, backend)
    }

    fn open_read_only_at(dir: PathBuf, backend: VaultBackend) -> Self {
        let stamp = FileStamp::of(&dir);
        let (entries, load_error) = match load_entries_read_only(&dir, backend) {
            Ok(entries) => (entries, None),
            Err(error) => (BTreeMap::new(), Some(error)),
        };
        Self {
            dir,
            entries,
            pending: std::sync::Mutex::default(),
            native_in_flight: std::sync::Mutex::default(),
            backend,
            stamp,
            load_error,
            read_only: true,
        }
    }

    fn open_at(dir: PathBuf, backend: VaultBackend) -> Self {
        let stamp = FileStamp::of(&dir);
        let (entries, load_error) = match load_entries(&dir, backend) {
            Ok(entries) => (entries, None),
            Err(error) => {
                tracing::warn!(%error, "accounts: vault unavailable; writes disabled until reopen");
                (BTreeMap::new(), Some(error))
            }
        };
        Self {
            dir,
            entries,
            pending: std::sync::Mutex::default(),
            native_in_flight: std::sync::Mutex::default(),
            backend,
            stamp,
            load_error,
            read_only: false,
        }
    }

    /// Read the file again when another process saved it since this view
    /// read it, keeping this view's own unsaved edits on top. Every process
    /// that holds the vault sees what the others wrote (a grant Passwords
    /// revoked, a key the messaging service prepared); saves already merge
    /// into the file under its lock. A file that cannot be read now leaves
    /// the view as it was.
    pub fn refresh(&mut self) {
        if self.load_error.is_some() {
            return;
        }
        let stamp = FileStamp::of(&self.dir);
        if stamp == self.stamp {
            return;
        }
        let loaded = if self.read_only {
            load_entries_read_only(&self.dir, self.backend)
        } else {
            load_entries(&self.dir, self.backend)
        };
        match loaded {
            Ok(mut entries) => {
                let pending = self
                    .pending
                    .get_mut()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for (id, (_, replacement)) in pending.iter() {
                    match replacement {
                        Some(secret) => {
                            entries.insert(id.clone(), secret.clone());
                        }
                        None => {
                            entries.remove(id);
                        }
                    }
                }
                self.entries = entries;
                self.stamp = stamp;
            }
            Err(error) => {
                tracing::warn!(%error, "accounts: vault changed but could not be read again");
            }
        }
    }

    /// A failed initial load requires recovery and a fresh open before saving.
    pub fn recovery_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn backend(&self) -> VaultBackend {
        self.backend
    }

    /// Store (or replace) `secret` for `id`. In-memory until [`Self::save`].
    pub fn put(&mut self, id: &AccountId, secret: &Secret) {
        // A secret that changes with no word of where it was typed for is
        // typed for no server yet ([`crate::typed_for`]).
        if self.entries.get(id.as_str()) != Some(secret) {
            self.typed_for_nowhere(id);
        }
        let original = self.entries.insert(id.as_str().to_string(), secret.clone());
        let change = self
            .pending
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.as_str().to_string())
            .or_insert((original, None));
        change.1 = Some(secret.clone());
    }

    pub fn get(&self, id: &AccountId) -> Option<Secret> {
        self.entries.get(id.as_str()).cloned()
    }
    pub(crate) fn contains(&self, id: &AccountId) -> bool {
        self.entries.contains_key(id.as_str())
    }

    /// Internal snapshot used by the typed stores layered over this encrypted
    /// container. Kept crate-private so applications cannot enumerate account
    /// credentials by bypassing their broker.
    pub(crate) fn entries_with_prefix(&self, prefix: &str) -> Vec<(String, Secret)> {
        self.entries
            .iter()
            .filter(|(id, _)| id.starts_with(prefix))
            .map(|(id, secret)| (id.clone(), secret.clone()))
            .collect()
    }

    /// Remove the secret for `id`, returning whether one existed. In-memory until [`Self::save`].
    pub fn remove(&mut self, id: &AccountId) -> bool {
        self.typed_for_nowhere(id);
        let original = self.entries.remove(id.as_str());
        let existed = original.is_some();
        let change = self
            .pending
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.as_str().to_string())
            .or_insert((original, None));
        change.1 = None;
        existed
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Seal + write the vault. Best-effort: logs and returns on any error so a read-only config
    /// dir never crashes the caller.
    pub fn save(&self) {
        if let Err(e) = self.try_save() {
            tracing::warn!(error = %e, "accounts: vault write failed");
        }
    }

    /// Seal and atomically persist the vault, returning failures to callers
    /// that must not publish account metadata unless credential storage
    /// succeeded first.
    pub fn try_save(&self) -> Result<(), String> {
        match self.written(true)? {
            Written::Durable => Ok(()),
            Written::InFlight => Err(SAVE_PENDING.to_owned()),
        }
    }

    /// Save the edits as [`Self::try_save`] does, telling a write still in
    /// flight apart from a failure. On a thread that must not wait on storage
    /// (the compositor's frame loop, [`tilo_files::waiting`]) the vault is
    /// written on a thread of its own, [`Saved::Pending`] until it is written,
    /// and the edits stay until then. Saving again reports how it ended.
    ///
    /// # Errors
    ///
    /// As [`Self::try_save`], except a write in flight.
    pub fn commit(&self) -> Result<Saved, String> {
        let wait = !tilo_files::waiting::forbidden();
        Ok(match self.written(wait)? {
            Written::Durable => Saved::Durable,
            Written::InFlight => Saved::Pending,
        })
    }

    fn written(&self, wait: bool) -> Result<Written, String> {
        if self.read_only {
            return Err("read-only credential vault cannot be saved".to_string());
        }
        if let Some(error) = &self.load_error {
            return Err(format!(
                "vault recovery required; restore access and reopen: {error}"
            ));
        }
        // One writer at a time: a caller that may wait writes under this.
        let mut in_flight = self
            .native_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.write_native_vault(&mut in_flight, wait)
    }

    /// Settle the write `in_flight`, then write the pending edits: here when
    /// the caller may `wait`, else on a thread of its own.
    fn write_native_vault(
        &self,
        in_flight: &mut Option<NativeWrite>,
        wait: bool,
    ) -> Result<Written, String> {
        if let Some(write) = in_flight.take() {
            if !wait && !write.thread.is_finished() {
                *in_flight = Some(write);
                return Ok(Written::InFlight);
            }
            // A failed write leaves its edits pending, to be written again.
            let written = write
                .thread
                .join()
                .unwrap_or_else(|_| Err("the credential vault writer stopped".to_owned()));
            written?;
            self.settle(write.submitted);
        }
        let submitted = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if wait {
            // Written even with no edits: a vault that cannot be read back
            // is refused here rather than reported saved.
            write_native(&self.dir, self.backend, &submitted)?;
            self.settle(submitted);
            return Ok(Written::Durable);
        }
        if submitted.is_empty() {
            return Ok(Written::Durable);
        }
        let (dir, backend, edits) = (self.dir.clone(), self.backend, submitted.clone());
        let thread = std::thread::Builder::new()
            .name("credential-vault".into())
            .spawn(move || write_native(&dir, backend, &edits))
            .map_err(|error| format!("could not start writing the credential vault: {error}"))?;
        *in_flight = Some(NativeWrite { submitted, thread });
        Ok(Written::InFlight)
    }

    /// Forget the edits a finished write kept, retaining any made since.
    fn settle(&self, submitted: PendingEdits) {
        settle_edits(
            &mut self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            submitted,
        );
    }
}

/// Forget from `pending` the edits a finished write kept, retaining any
/// made since.
fn settle_edits(pending: &mut PendingEdits, submitted: PendingEdits) {
    for (id, (_, saved)) in submitted {
        if let Some((original, replacement)) = pending.get_mut(&id) {
            if replacement == &saved {
                pending.remove(&id);
            } else {
                // Edited again while that write was under way: the newer
                // edit stays, based on the value now written.
                *original = saved;
            }
        }
    }
}

struct NativeWrite {
    submitted: PendingEdits,
    thread: std::thread::JoinHandle<Result<(), String>>,
}

impl std::fmt::Debug for NativeWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeWrite").finish_non_exhaustive()
    }
}

/// Seal and atomically write the vault in `dir` with `edits` merged into
/// what it holds, under its lock.
fn write_native(dir: &Path, backend: VaultBackend, edits: &PendingEdits) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create vault directory: {error}"))?;
    let _lock =
        advisory_lock::FileLock::open(&dir.join("vault.lock"), advisory_lock::LockMode::Exclusive)
            .map_err(|error| format!("could not lock credential vault: {error}"))?;
    let entries = merge_edits(load_entries(dir, backend)?, edits)?;
    let key =
        load_or_create_key(dir, backend).ok_or_else(|| "no vault key is available".to_string())?;
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&entries)
            .map_err(|error| format!("could not serialize credential vault: {error}"))?,
    );
    let blob =
        seal(&key, &plaintext).ok_or_else(|| "credential vault encryption failed".to_string())?;
    tilo_files::native::atomic::write(&dir.join("vault.enc"), &blob)
        .map_err(|error| format!("could not write credential vault: {error}"))
}

/// Why [`SecretVault::try_save`] refuses while a write is in flight.
const SAVE_PENDING: &str = "credential save pending; retry once the vault is written";

#[derive(Clone, Copy)]
enum Written {
    Durable,
    InFlight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    Durable,
    Pending,
}

/// Bounded, owner-checked ciphertext reads preserve inherited ciphertext modes;
/// the existing key retains strict canonical private-file ownership.
fn load_entries_read_only(
    dir: &Path,
    backend: VaultBackend,
) -> Result<BTreeMap<String, Secret>, String> {
    const MAX_BLOB: u64 = 8 * 1024 * 1024;
    let file = tilo_files::native::private::read_ciphertext(&dir.join("vault.enc"))
        .map_err(|_| "existing credential vault unavailable".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "credential vault metadata unavailable".to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BLOB {
        return Err("credential vault exceeds read limit".to_string());
    }
    let mut blob = Vec::new();
    file.take(MAX_BLOB + 1)
        .read_to_end(&mut blob)
        .map_err(|_| "credential vault read failed".to_string())?;
    if blob.len() as u64 > MAX_BLOB {
        return Err("credential vault exceeds read limit".to_string());
    }
    let key = FORMAT
        .read_existing_key(dir, backend)
        .ok_or_else(|| "existing credential vault key unavailable".to_string())?;
    let plaintext =
        open(&key, &blob).ok_or_else(|| "credential vault integrity check failed".to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| "decrypted credential vault is invalid".to_string())
}

fn merge_edits(
    mut entries: BTreeMap<String, Secret>,
    edits: &PendingEdits,
) -> Result<BTreeMap<String, Secret>, String> {
    for (id, (original, replacement)) in edits {
        let current = entries.get(id);
        if current != original.as_ref() && current != replacement.as_ref() {
            return Err("credential changed concurrently; reopen and retry".into());
        }
        if let Some(secret) = replacement {
            entries.insert(id.clone(), secret.clone());
        } else {
            entries.remove(id);
        }
    }
    Ok(entries)
}

fn load_entries(dir: &Path, backend: VaultBackend) -> Result<BTreeMap<String, Secret>, String> {
    let blob = match std::fs::read(dir.join("vault.enc")) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("could not read credential vault: {error}")),
    };
    let key = load_or_create_key(dir, backend)
        .ok_or_else(|| "credential vault key unavailable".to_string())?;
    let plaintext =
        open(&key, &blob).ok_or_else(|| "credential vault integrity check failed".to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| "decrypted credential vault is invalid".to_string())
}

/// Read the vault key from `dir`, generating + persisting a fresh random one the first time.
/// `None` only when the key can neither be read nor created. The single point of key
/// ownership — see module docs.
fn load_or_create_key(dir: &Path, backend: VaultBackend) -> Option<VaultKey> {
    FORMAT.load_or_create_key(dir, backend)
}

fn seal(key: &VaultKey, plaintext: &[u8]) -> Option<Vec<u8>> {
    FORMAT.seal(key, plaintext)
}

fn open(key: &VaultKey, blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    FORMAT.open(key, blob)
}
