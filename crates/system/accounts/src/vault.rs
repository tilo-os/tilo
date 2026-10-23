//! The encrypted **secret vault** — credentials at rest, never in the plain
//! [`crate::AccountStore`] JSON.
//!
//! Every credential an [`crate::Account`] needs is a long-lived bearer secret, sealed with
//! **XChaCha20-Poly1305** (192-bit-nonce AEAD, audited `RustCrypto` stack) under a 256-bit key,
//! stored as `vault.enc` under `$XDG_CONFIG_HOME/tilo/accounts/vault/`. A fresh random nonce
//! is drawn per write, so re-saving an unchanged vault still produces distinct ciphertext and
//! no nonce is ever reused. Sealing and key custody live in the `secret-vault` crate, shared
//! with the authenticator; this module owns only the account vault's format identity.
//!
//! # Key custody
//!
//! Windows protects keys with current-user DPAPI and private file ACLs, including
//! explicit-root stores. Existing private plaintext keys migrate only after
//! authenticated restoration; unavailable protected keys never fall back.
//! On a Linux desktop session the symmetric key is held by the freedesktop
//! Secret Service through `tilo-secrets`; it is not stored beside the
//! ciphertext. New default stores refuse unavailable protected custody rather
//! than silently creating a local key. Existing owner-only local keys remain
//! readable for recovery and migration; [`SecretVault::open_in`] explicitly
//! opts into local custody for headless recovery and tests.
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

pub use secret_vault::Custody as SecretCustody;

/// One account's secret material. Kept out of the plaintext account store entirely — it
/// only ever lives in the sealed vault (or in memory while an app is connecting).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    /// The password / app-password for password-auth accounts.
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
    /// A password-only secret.
    pub fn password(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
            token: None,
        }
    }

    /// Whether this secret carries nothing worth persisting.
    pub fn is_empty(&self) -> bool {
        self.password.is_empty() && self.token.is_none()
    }
}

/// The persisted identity of the account vault. Every value here is part of
/// the on-disk format and of where its key is held.
const FORMAT: VaultFormat = VaultFormat {
    name: "accounts",
    // Tilo Accounts Secret Vault.
    magic: b"QASV",
    secret_service_label: "Tilo account credentials",
    secret_service_attributes: &[("application", "Tilo"), ("purpose", "account-vault")],
    dpapi_entropy: b"Tilo Accounts vault key v1",
};

/// Original and staged replacement per credential, for conflict-checked saves.
type PendingEdits = BTreeMap<String, (Option<Secret>, Option<Secret>)>;

/// The encrypted, per-[`AccountId`] credential store. Held in memory after
/// [`SecretVault::open`]; mutations are staged and written by [`SecretVault::save`].
#[derive(Debug)]
pub struct SecretVault {
    /// The vault directory (`.../tilo/accounts/vault`).
    dir: PathBuf,
    /// Set where there is no native config directory (a browser page): the
    /// vault lives in the shared config location's storage instead, its key
    /// in the platform's secret store.
    page: Option<tilo_files::Dir>,
    /// Secrets keyed by the account id string.
    entries: BTreeMap<String, Secret>,
    // Explicit edits merge into the latest inventory while the write lock is held.
    pending: std::sync::Mutex<PendingEdits>,
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    page_in_flight: std::sync::Mutex<Option<PendingEdits>>,
    /// A write on a thread of its own, for a caller that must not wait on
    /// the disk ([`Self::commit`] on the compositor's frame loop).
    native_in_flight: std::sync::Mutex<Option<NativeWrite>>,
    custody: SecretCustody,
    load_error: Option<String>,
    read_only: bool,
}

pub(crate) fn explicit_root_custody() -> SecretCustody {
    #[cfg(windows)]
    {
        SecretCustody::WindowsDpapi
    }
    #[cfg(not(windows))]
    {
        SecretCustody::LocalFile
    }
}
impl SecretVault {
    /// Open the vault at the default location (`$XDG_CONFIG_HOME/tilo/accounts/vault`). An empty
    /// vault when nothing is stored. Load failures yield an empty view with
    /// [`Self::recovery_error`] set and writes disabled until recovery and reopen.
    pub fn open() -> Self {
        match crate::config_root() {
            Some(root) => {
                let dir = crate::vault_dir_in(&root);
                Self::open_session_at(dir, SecretCustody::for_session())
            }
            None => match tilo_files::Dir::open(tilo_paths::AppPaths::for_app("tilo").config()) {
                Ok(page) => Self::open_page(page),
                Err(error) => Self {
                    dir: PathBuf::from("accounts/vault"),
                    page: None,
                    entries: BTreeMap::new(),
                    pending: std::sync::Mutex::default(),
                    native_in_flight: std::sync::Mutex::default(),
                    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
                    page_in_flight: std::sync::Mutex::default(),
                    custody: explicit_root_custody(),
                    read_only: false,
                    load_error: Some(format!("user config directory unavailable: {error}")),
                },
            },
        }
    }

    /// Open the vault under an explicit config root. Windows uses the same
    /// DPAPI custody as the default store; other hosts use a private local key.
    pub fn open_in(config_root: &Path) -> Self {
        let dir = crate::vault_dir_in(config_root);
        let custody = explicit_root_custody();
        Self::open_at(dir, custody)
    }

    /// Read an existing explicit-root vault without changing its source custody.
    /// Missing ciphertext or keys are recovery errors. This view cannot be saved,
    /// including after in-memory edits; it never creates or restricts source files.
    pub fn open_read_only_in(config_root: &Path) -> Self {
        let dir = crate::vault_dir_in(config_root);
        let custody = explicit_root_custody();
        Self::open_read_only_at(dir, custody)
    }

    /// Read existing native session custody without creating keys or files.
    /// An unavailable native config root is a recovery error; no page fallback
    /// or new credential namespace is created.
    pub fn open_read_only() -> Self {
        match crate::config_root() {
            Some(root) => {
                Self::open_read_only_at(crate::vault_dir_in(&root), SecretCustody::for_session())
            }
            None => Self {
                dir: PathBuf::new(),
                page: None,
                entries: BTreeMap::new(),
                pending: std::sync::Mutex::default(),
                native_in_flight: std::sync::Mutex::default(),
                #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
                page_in_flight: std::sync::Mutex::default(),
                custody: explicit_root_custody(),
                read_only: true,
                load_error: Some("native credential custody unavailable".into()),
            },
        }
    }

    fn open_read_only_at(dir: PathBuf, custody: SecretCustody) -> Self {
        let (entries, load_error) = match load_entries_read_only(&dir, custody) {
            Ok(entries) => (entries, None),
            Err(error) => (BTreeMap::new(), Some(error)),
        };
        Self {
            dir,
            page: None,
            entries,
            pending: std::sync::Mutex::default(),
            native_in_flight: std::sync::Mutex::default(),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            page_in_flight: std::sync::Mutex::default(),
            custody,
            load_error,
            read_only: true,
        }
    }

    /// Open the encrypted vault in a supplied profile-bound config directory.
    /// Browser hosts must load that directory's file mirror and secret custody
    /// before opening. The Dir capability, not an app-chosen user label, selects
    /// the inventory; native key custody is unaffected.
    pub fn open_page(page: tilo_files::Dir) -> Self {
        let (entries, load_error) = match load_page_entries(&page) {
            Ok(entries) => (entries, None),
            Err(error) => {
                tracing::warn!(%error, "accounts: vault unavailable; writes disabled until reopen");
                (BTreeMap::new(), Some(error))
            }
        };
        Self {
            dir: PathBuf::from("accounts/vault"),
            page: Some(page),
            entries,
            pending: std::sync::Mutex::default(),
            native_in_flight: std::sync::Mutex::default(),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            page_in_flight: std::sync::Mutex::default(),
            custody: SecretCustody::System,
            load_error,
            read_only: false,
        }
    }

    /// Compatibility local keys are retained, but lack of a keyring must not
    /// silently choose weaker custody for a new default store.
    fn open_session_at(dir: PathBuf, custody: SecretCustody) -> Self {
        if custody == SecretCustody::LocalFile && !dir.join("vault.key").exists() {
            return Self {
                dir,
                page: None,
                entries: BTreeMap::new(),
                pending: std::sync::Mutex::default(),
                native_in_flight: std::sync::Mutex::default(),
                #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
                page_in_flight: std::sync::Mutex::default(),
                custody,
                read_only: false,
                load_error: Some(
                    "protected credential storage unavailable; restore the system keyring or explicitly open a recovery store".into(),
                ),
            };
        }
        Self::open_at(dir, custody)
    }

    fn open_at(dir: PathBuf, custody: SecretCustody) -> Self {
        let (entries, load_error) = match load_entries(&dir, custody) {
            Ok(entries) => (entries, None),
            Err(error) => {
                tracing::warn!(%error, "accounts: vault unavailable; writes disabled until reopen");
                (BTreeMap::new(), Some(error))
            }
        };
        Self {
            dir,
            page: None,
            entries,
            pending: std::sync::Mutex::default(),
            native_in_flight: std::sync::Mutex::default(),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            page_in_flight: std::sync::Mutex::default(),
            custody,
            load_error,
            read_only: false,
        }
    }

    /// A failed initial load requires recovery and a fresh open before saving.
    pub fn recovery_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn custody(&self) -> SecretCustody {
        self.custody
    }

    /// Store (or replace) `secret` for `id`. In-memory until [`Self::save`].
    pub fn put(&mut self, id: &AccountId, secret: &Secret) {
        let original = self.entries.insert(id.as_str().to_string(), secret.clone());
        let change = self
            .pending
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.as_str().to_string())
            .or_insert((original, None));
        change.1 = Some(secret.clone());
    }

    /// Fetch the secret for `id`, if the vault holds one.
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

    /// Whether the vault currently holds no secrets.
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
    /// that must not publish account metadata unless credential custody
    /// succeeded first.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error when the vault directory or key cannot be
    /// prepared, serialization or encryption fails, or the sealed file cannot
    /// be atomically persisted.
    pub fn try_save(&self) -> Result<(), String> {
        match self.written(true)? {
            Written::Durable => Ok(()),
            Written::InFlight => Err(SAVE_PENDING.to_owned()),
            Written::Remaining => Err("credential save pending; retry remaining edits".to_owned()),
        }
    }

    /// Save the edits as [`Self::try_save`] does, telling a write still in
    /// flight apart from a failure: a browser page commits through its
    /// storage asynchronously, so its first save of an edit is
    /// [`Saved::Pending`] and the edit stays. Saving again reports how it
    /// ended.
    ///
    /// # Errors
    ///
    /// As [`Self::try_save`], except a write in flight.
    ///
    /// On a thread that must not wait on storage (the compositor's frame
    /// loop, [`durable_store::waiting`]) a native vault is written the same
    /// way: on a thread of its own, [`Saved::Pending`] until it is written.
    pub fn commit(&self) -> Result<Saved, String> {
        let wait = !durable_store::waiting::forbidden();
        Ok(match self.written(wait)? {
            Written::Durable => Saved::Durable,
            Written::InFlight | Written::Remaining => Saved::Pending,
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
        if self.page.is_some() {
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            return self.save_page();
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            return Err("page credential storage is only available in a browser".into());
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
            write_native(&self.dir, self.custody, &submitted)?;
            self.settle(submitted);
            return Ok(Written::Durable);
        }
        if submitted.is_empty() {
            return Ok(Written::Durable);
        }
        let (dir, custody, edits) = (self.dir.clone(), self.custody, submitted.clone());
        let thread = std::thread::Builder::new()
            .name("credential-vault".into())
            .spawn(move || write_native(&dir, custody, &edits))
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

/// A native vault write under way on its own thread, and the edits it writes.
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
fn write_native(dir: &Path, custody: SecretCustody, edits: &PendingEdits) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create vault directory: {error}"))?;
    let _lock =
        advisory_lock::FileLock::open(&dir.join("vault.lock"), advisory_lock::LockMode::Exclusive)
            .map_err(|error| format!("could not lock credential vault: {error}"))?;
    let entries = merge_edits(load_entries(dir, custody)?, edits)?;
    let key =
        load_or_create_key(dir, custody).ok_or_else(|| "no vault key is available".to_string())?;
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&entries)
            .map_err(|error| format!("could not serialize credential vault: {error}"))?,
    );
    let blob =
        seal(&key, &plaintext).ok_or_else(|| "credential vault encryption failed".to_string())?;
    tilo_files::native::atomic::write(&dir.join("vault.enc"), &blob)
        .map_err(|error| format!("could not write credential vault: {error}"))
}

/// Why [`SecretVault::try_save`] refuses while a browser write is in flight.
const SAVE_PENDING: &str = "credential save pending; retry after browser storage completes";

/// How far a save got, in the detail [`SecretVault::try_save`] reports.
#[derive(Clone, Copy)]
#[cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    allow(dead_code, reason = "only a browser page writes asynchronously")
)]
enum Written {
    Durable,
    /// The browser is still writing the edits.
    InFlight,
    /// A write landed, and edits made meanwhile are written next.
    Remaining,
}

/// How far a [`SecretVault::commit`] got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    /// On disk, or in the browser's storage.
    Durable,
    /// The browser is still writing it; save again to learn how it ended.
    Pending,
}

impl SecretVault {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    fn save_page(&self) -> Result<Written, String> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_empty() {
            return Ok(Written::Durable);
        }
        let mut in_flight = self
            .page_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let submitted = in_flight.get_or_insert_with(|| pending.clone()).clone();
        // Identify the requested result, not the freshly loaded baseline. A
        // synchronous facade may reopen its vault on each retry, after the
        // committed file has already refreshed the mirror.
        let replacements: BTreeMap<_, _> = submitted
            .iter()
            .map(|(id, (_, replacement))| (id, replacement))
            .collect();
        let operation = Zeroizing::new(
            serde_json::to_string(&replacements)
                .map_err(|_| "could not encode credential edits")?,
        );
        let edits = submitted.clone();
        let page = self.page.as_ref().ok_or("page vault unavailable")?;
        let committed = tilo_files::web::update_atomic_blocking(
            page,
            &page_vault(),
            &operation,
            move |blob| async move {
                let key = FORMAT
                    .load_or_create_page_key(blob.is_some())
                    .await
                    .map_err(tilo_files::FsError::Io)?;
                let entries = decode_page_entries(blob.as_deref(), &key)
                    .and_then(|entries| merge_edits(entries, &edits))
                    .map_err(tilo_files::FsError::Io)?;
                let plaintext = Zeroizing::new(
                    serde_json::to_vec(&entries)
                        .map_err(|error| tilo_files::FsError::Io(error.to_string()))?,
                );
                seal(&key, &plaintext).ok_or_else(|| {
                    tilo_files::FsError::Io("credential vault encryption failed".into())
                })
            },
        );
        let committed = match committed {
            Ok(committed) => committed,
            Err(error) => {
                // Keep the immutable snapshot for retry: an I/O failure may
                // follow a visible replacement. Reestablish its durable result
                // before rebasing any edits made while it was in flight.
                return Err(format!("credential storage failed: {error}"));
            }
        };
        if !committed {
            return Ok(Written::InFlight);
        }
        *in_flight = None;
        drop(in_flight);
        for (id, (_, saved)) in submitted {
            if let Some((original, replacement)) = pending.get_mut(&id) {
                if replacement == &saved {
                    pending.remove(&id);
                } else {
                    // The user edited again while this snapshot was committing.
                    // Retain the newer edit, based on the acknowledged value.
                    *original = saved;
                }
            }
        }
        if pending.is_empty() {
            Ok(Written::Durable)
        } else {
            // Edits made while that write was in flight: written next.
            Ok(Written::Remaining)
        }
    }
}

fn page_vault() -> tilo_files::VPath {
    tilo_files::VPath::new("accounts/vault/vault.enc").unwrap_or_else(|_| tilo_files::VPath::root())
}

fn load_page_entries(page: &tilo_files::Dir) -> Result<BTreeMap<String, Secret>, String> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        // An unreleased browser implementation stored an origin-wide inventory.
        // It has no profile provenance: never import it into the current user.
        // Retain it for explicit recovery, without making it authoritative.
        let store = tilo_secrets::SecretStore::for_app("tilo.accounts");
        if matches!(store.get_blocking("vault.entries"), Ok(Some(_))) {
            tracing::warn!(
                "unassigned origin-wide account inventory retained in tilo-secrets tilo.accounts/vault.entries; recover explicitly into the verified owner's profile"
            );
        }
    }
    let blob = match tilo_files::blocking::read(page, &page_vault()) {
        Ok(blob) => blob,
        Err(tilo_files::FsError::NotFound) => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("could not read credential vault: {error}")),
    };
    let key = FORMAT
        .load_or_create_system_key(true)
        .ok_or_else(|| "credential vault key unavailable".to_string())?;
    decode_page_entries(Some(&blob), &key)
}

fn decode_page_entries(
    blob: Option<&[u8]>,
    key: &VaultKey,
) -> Result<BTreeMap<String, Secret>, String> {
    let Some(blob) = blob else {
        return Ok(BTreeMap::new());
    };
    let plaintext =
        open(key, blob).ok_or_else(|| "credential vault integrity check failed".to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| "decrypted credential vault is invalid".to_string())
}

/// Bounded, owner-checked ciphertext reads preserve inherited ciphertext modes;
/// the existing key retains strict canonical private-file custody.
fn load_entries_read_only(
    dir: &Path,
    custody: SecretCustody,
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
        .read_existing_key(dir, custody)
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

/// Read and authenticate existing entries, distinguishing absence from damage.
fn load_entries(dir: &Path, custody: SecretCustody) -> Result<BTreeMap<String, Secret>, String> {
    let blob = match std::fs::read(dir.join("vault.enc")) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("could not read credential vault: {error}")),
    };
    let key = load_or_create_key(dir, custody)
        .ok_or_else(|| "credential vault key unavailable".to_string())?;
    let plaintext =
        open(&key, &blob).ok_or_else(|| "credential vault integrity check failed".to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| "decrypted credential vault is invalid".to_string())
}

/// Read the vault key from `dir`, generating + persisting a fresh random one the first time.
/// `None` only when the key can neither be read nor created. The single point of key
/// custody — see module docs.
fn load_or_create_key(dir: &Path, custody: SecretCustody) -> Option<VaultKey> {
    FORMAT.load_or_create_key(dir, custody)
}

fn seal(key: &VaultKey, plaintext: &[u8]) -> Option<Vec<u8>> {
    FORMAT.seal(key, plaintext)
}

fn open(key: &VaultKey, blob: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    FORMAT.open(key, blob)
}

#[cfg(test)]
mod tests {
    use tilo_files::native::private as private_fs;

    #[test]
    fn secret_debug_never_exposes_password_or_token() {
        let secret = super::Secret {
            password: "debug-password-marker".into(),
            token: Some("debug-token-marker".into()),
        };
        let output = format!("{secret:?}");
        assert!(!output.contains("debug-password-marker"));
        assert!(!output.contains("debug-token-marker"));
        let encoded = serde_json::to_string(&secret).unwrap();
        assert_eq!(
            serde_json::from_str::<super::Secret>(&encoded).unwrap(),
            secret
        );
    }

    use super::*;
    use secret_vault::KEY_LEN;
    #[cfg(windows)]
    use std::time::Duration;

    #[test]
    fn page_inventory_writes_fail_closed_without_cross_page_transactions() {
        let page = tilo_files::Dir::memory();
        let mut first = SecretVault::open_page(page.clone());
        let mut second = SecretVault::open_page(page.clone());
        first.put(&AccountId::new("first"), &Secret::password("first-secret"));
        second.put(
            &AccountId::new("second"),
            &Secret::password("second-secret"),
        );
        assert!(first.try_save().is_err());
        assert!(second.try_save().is_err());
        assert!(matches!(
            tilo_files::blocking::read(&page, &page_vault()),
            Err(tilo_files::FsError::NotFound)
        ));
        assert!(!first.pending.lock().unwrap().is_empty());
    }

    fn local_file_key(dir: &Path) -> Option<VaultKey> {
        load_or_create_key(dir, SecretCustody::LocalFile)
    }

    fn read_key_bytes(path: &Path) -> std::io::Result<Zeroizing<Vec<u8>>> {
        use std::io::Read as _;
        let mut bytes = Zeroizing::new(Vec::new());
        tilo_files::native::private::read(path)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    #[cfg(windows)]
    mod readonly_windows {
        use super::*;
        use std::os::windows::fs::MetadataExt as _;

        #[derive(Debug, PartialEq, Eq)]
        struct Witness {
            name: std::ffi::OsString,
            bytes: Vec<u8>,
            attributes: u32,
            created: u64,
            modified: u64,
            acl: Vec<u8>,
        }

        fn acl(path: &Path) -> Vec<u8> {
            let result = bounded_command::run(
                "icacls.exe",
                &[path.to_str().unwrap()],
                None,
                16 * 1024,
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(result.status.success(), "{result:?}");
            result.stdout
        }

        fn snapshot(directory: &Path) -> Vec<Witness> {
            let mut paths: Vec<_> = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            paths.sort();
            paths.insert(0, directory.to_owned());
            paths
                .into_iter()
                .map(|path| {
                    let metadata = std::fs::symlink_metadata(&path).unwrap();
                    let bytes = if metadata.is_file() {
                        let private = private_fs::read(&path).unwrap();
                        assert_eq!(private.metadata().unwrap().len(), metadata.len());
                        std::fs::read(&path).unwrap()
                    } else {
                        Vec::new()
                    };
                    Witness {
                        name: path.file_name().unwrap().to_owned(),
                        bytes,
                        attributes: metadata.file_attributes(),
                        created: metadata.creation_time(),
                        modified: metadata.last_write_time(),
                        acl: acl(&path),
                    }
                })
                .collect()
        }

        fn source(root: &Path) -> (AccountId, PathBuf) {
            let id = AccountId::from("readonly-dpapi-account");
            let mut writable = SecretVault::open_in(root);
            writable.put(&id, &Secret::password("original-private-secret"));
            writable.try_save().unwrap();
            let directory = crate::vault_dir_in(root);
            assert!(private_fs::read(&directory.join("vault.key.dpapi")).is_ok());
            assert!(!directory.join("vault.key").exists());
            std::fs::remove_file(directory.join("vault-key.lock")).unwrap();
            (id, directory)
        }

        #[test]
        fn readonly_dpapi_source_authenticates_without_recreating_lock_or_changing_acl() {
            let root = temp_root();
            let (id, directory) = source(&root);
            let before = snapshot(&directory);
            let mut readonly = SecretVault::open_read_only_in(&root);
            assert!(readonly.recovery_error().is_none());
            assert_eq!(readonly.custody(), SecretCustody::WindowsDpapi);
            assert_eq!(
                readonly.get(&id),
                Some(Secret::password("original-private-secret"))
            );
            readonly.put(&id, &Secret::password("memory-only-edit"));
            assert!(readonly.try_save().is_err());
            assert!(!directory.join("vault-key.lock").exists());
            assert_eq!(snapshot(&directory), before);
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn readonly_missing_or_corrupt_dpapi_key_never_creates_or_repairs_source() {
            let root = temp_root();
            let missing = root.join("absent");
            assert!(
                SecretVault::open_read_only_in(&missing)
                    .recovery_error()
                    .is_some()
            );
            assert!(!missing.exists());
            let (_, directory) = source(&root);
            let key = directory.join("vault.key.dpapi");
            let ciphertext = std::fs::read(directory.join("vault.enc")).unwrap();
            std::fs::remove_file(&key).unwrap();
            let missing_before = snapshot(&directory);
            assert!(
                SecretVault::open_read_only_in(&root)
                    .recovery_error()
                    .is_some()
            );
            assert!(!key.exists());
            assert!(!directory.join("vault-key.lock").exists());
            assert_eq!(snapshot(&directory), missing_before);
            private_fs::create_atomic(&key, b"invalid DPAPI ciphertext").unwrap();
            let corrupt_before = snapshot(&directory);
            assert!(
                SecretVault::open_read_only_in(&root)
                    .recovery_error()
                    .is_some()
            );
            assert!(!directory.join("vault-key.lock").exists());
            assert_eq!(snapshot(&directory), corrupt_before);
            assert_eq!(
                std::fs::read(directory.join("vault.enc")).unwrap(),
                ciphertext
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn readonly_source_vault_authenticates_without_changing_files_or_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = temp_root();
        let id = AccountId::from("source-account");
        let mut writable = SecretVault::open_in(&root);
        writable.put(&id, &Secret::password("source-secret"));
        writable.try_save().unwrap();
        let directory = crate::vault_dir_in(&root);
        let snapshot = || {
            let mut entries: Vec<_> = std::fs::read_dir(&directory)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry.file_name(),
                        std::fs::read(entry.path()).unwrap(),
                        entry.metadata().unwrap().permissions().mode(),
                    )
                })
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            entries
        };
        let before = snapshot();
        let mut readonly = SecretVault::open_read_only_in(&root);
        assert!(
            readonly.recovery_error().is_none(),
            "{:?}",
            readonly.recovery_error()
        );
        assert_eq!(readonly.get(&id), Some(Secret::password("source-secret")));
        readonly.put(&id, &Secret::password("changed-in-memory"));
        assert!(readonly.try_save().is_err());
        assert_eq!(snapshot(), before);
        let ciphertext = directory.join("vault.enc");
        let retained = directory.join("original.enc");
        std::fs::rename(&ciphertext, &retained).unwrap();
        std::os::unix::fs::symlink(&retained, &ciphertext).unwrap();
        assert!(
            SecretVault::open_read_only_in(&root)
                .recovery_error()
                .is_some()
        );
        assert_eq!(
            std::fs::read(&retained).unwrap(),
            before
                .iter()
                .find(|(name, _, _)| name == "vault.enc")
                .unwrap()
                .1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn readonly_missing_or_corrupt_source_custody_never_creates_or_repairs_files() {
        use std::io::Write as _;
        let root = temp_root();
        let missing = root.join("absent");
        assert!(
            SecretVault::open_read_only_in(&missing)
                .recovery_error()
                .is_some()
        );
        assert!(!missing.exists());
        let mut writable = SecretVault::open_in(&root);
        writable.put(&AccountId::from("account"), &Secret::password("secret"));
        writable.try_save().unwrap();
        let directory = crate::vault_dir_in(&root);
        let blob = directory.join("vault.enc");
        let key = directory.join("vault.key");
        let ciphertext = std::fs::read(&blob).unwrap();
        // The writable setup leaves its advisory lock behind. Remove it so
        // read-only recovery must prove it never recreates a writer's lock.
        std::fs::remove_file(directory.join("vault.lock")).unwrap();
        std::fs::remove_file(&key).unwrap();
        assert!(
            SecretVault::open_read_only_in(&root)
                .recovery_error()
                .is_some()
        );
        assert!(!key.exists());
        assert_eq!(std::fs::read(&blob).unwrap(), ciphertext);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        private_fs::create_new(&key)
            .unwrap()
            .write_all(b"malformed")
            .unwrap();
        assert!(
            SecretVault::open_read_only_in(&root)
                .recovery_error()
                .is_some()
        );
        assert_eq!(std::fs::read(&key).unwrap(), b"malformed");
        assert_eq!(std::fs::read(&blob).unwrap(), ciphertext);
        assert!(!directory.join("vault.lock").exists());
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unavailable_protected_custody_does_not_create_a_default_local_key() {
        let root = temp_root();
        let dir = crate::vault_dir_in(&root);
        let mut refused = SecretVault::open_session_at(dir.clone(), SecretCustody::LocalFile);
        assert!(refused.recovery_error().is_some());
        refused.put(&AccountId::from("a"), &Secret::password("secret"));
        assert!(refused.try_save().is_err());
        assert!(!dir.exists());

        // Explicit recovery stores remain supported, and their existing data
        // is still readable through the default compatibility path.
        let mut recovery = SecretVault::open_in(&root);
        recovery.put(&AccountId::from("a"), &Secret::password("secret"));
        recovery.try_save().unwrap();
        let reopened = SecretVault::open_session_at(dir, explicit_root_custody());
        assert!(reopened.recovery_error().is_none());
        assert_eq!(
            reopened.get(&AccountId::from("a")).unwrap().password,
            "secret"
        );
        assert_eq!(
            format!("{:?}", Secret::password("sensitive-value")),
            "Secret { .. }"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vault_key_is_created_once_and_owner_only() {
        {
            let root = temp_root();
            let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
            // Spawn every participant before joining: interleaving spawn/join
            // would deadlock the barrier.
            let mut workers = Vec::with_capacity(4);
            for _ in 0..4 {
                let root = root.clone();
                let gate = std::sync::Arc::clone(&gate);
                workers.push(std::thread::spawn(move || {
                    gate.wait();
                    local_file_key(&root).unwrap()
                }));
            }
            let keys: Vec<_> = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect();
            let persisted = read_key_bytes(&root.join("vault.key")).unwrap();
            assert!(
                keys.iter()
                    .all(|key| key.expose_secret().as_slice() == persisted.as_slice())
            );
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
            std::fs::remove_dir_all(&root).unwrap();
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let root = temp_root();
            let mut v = SecretVault::open_in(&root);
            v.put(&AccountId::from("a"), &Secret::password("pw"));
            v.save();
            let mode = std::fs::metadata(crate::vault_dir_in(&root).join("vault.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "vault key must be owner-only");
            std::fs::remove_dir_all(&root).ok();
        }
    }

    #[cfg(windows)]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_key_custody_never_leaks_rotates_or_falls_back() {
        {
            let root = temp_root();
            let mut vault =
                SecretVault::open_at(crate::vault_dir_in(&root), SecretCustody::LocalFile);
            vault.put(&AccountId::from("protected"), &Secret::password("secret"));
            vault.try_save().unwrap();
            let directory = crate::vault_dir_in(&root);
            let key_path = directory.join("vault.key");
            let sealed_path = directory.join("vault.enc");
            let key_before = std::fs::read(&key_path).unwrap();
            let sealed_before = std::fs::read(&sealed_path).unwrap();
            let result = bounded_command::run(
                "icacls.exe",
                &[key_path.to_str().unwrap(), "/grant", "*S-1-1-0:(R)"],
                None,
                4096,
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(local_file_key(&directory).is_none());
            assert!(vault.try_save().is_err());
            assert_eq!(std::fs::read(&key_path).unwrap(), key_before);
            assert_eq!(std::fs::read(&sealed_path).unwrap(), sealed_before);
            std::fs::remove_dir_all(&root).unwrap();
        }

        {
            let root = temp_root();
            let mut item = crate::SecretItem::login("Owned fixture", "user", "fixture secret");
            let mut store = crate::SecretStore::open_in(&root);
            store.put(&mut item).unwrap();
            let dir = crate::vault_dir_in(&root);
            assert!(!dir.join("vault.key").exists());
            assert!(tilo_files::native::private::read(&dir.join("vault.key.dpapi")).is_ok());
            let reopened = crate::SecretStore::open_in(&root);
            assert!(reopened.recovery_error().is_none());
            assert_eq!(
                reopened.reveal_password(&item.id).unwrap().as_str(),
                "fixture secret"
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            vault.put(&AccountId::from("protected"), &Secret::password("fixture"));
            vault.try_save().unwrap();
            let dir = crate::vault_dir_in(&root);
            let key_path = dir.join("vault.key.dpapi");
            let key_before = std::fs::read(&key_path).unwrap();
            let encrypted = std::fs::read(dir.join("vault.enc")).unwrap();
            let result = bounded_command::run(
                "icacls.exe",
                &[key_path.to_str().unwrap(), "/grant", "*S-1-1-0:(R)"],
                None,
                4096,
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(SecretVault::open_in(&root).recovery_error().is_some());
            assert!(vault.try_save().is_err());
            assert!(!dir.join("vault.key").exists());
            assert_eq!(std::fs::read(key_path).unwrap(), key_before);
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), encrypted);
            std::fs::remove_dir_all(root).unwrap();
        }

        {
            let root = temp_root();
            let dir = crate::vault_dir_in(&root);
            let mut vault = SecretVault::open_in(&root);
            vault.put(&AccountId::from("original"), &Secret::password("fixture"));
            vault.try_save().unwrap();
            let encrypted = std::fs::read(dir.join("vault.enc")).unwrap();
            let stray_key = [7; KEY_LEN];
            tilo_files::native::private::create_atomic(&dir.join("vault.key"), &stray_key).unwrap();
            tilo_files::native::private::write_atomic(
                &dir.join("vault.key.dpapi"),
                b"corrupt protected key",
            )
            .unwrap();
            let failed = SecretVault::open_in(&root);
            assert!(failed.recovery_error().is_some());
            assert!(failed.try_save().is_err());
            assert_eq!(
                std::fs::read(dir.join("vault.key")).unwrap(),
                stray_key.as_slice()
            );
            assert_eq!(
                std::fs::read(dir.join("vault.key.dpapi")).unwrap(),
                b"corrupt protected key"
            );
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), encrypted);
            std::fs::remove_dir_all(root).unwrap();
        }

        {
            let root = temp_root();
            let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
            let mut workers = Vec::new();
            for _ in 0..4 {
                let root = root.clone();
                let gate = gate.clone();
                workers.push(std::thread::spawn(move || {
                    gate.wait();
                    load_or_create_key(&root, SecretCustody::WindowsDpapi).unwrap()
                }));
            }
            let keys: Vec<_> = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect();
            let persisted = load_or_create_key(&root, SecretCustody::WindowsDpapi).unwrap();
            assert!(keys.iter().all(|key| *key == persisted));
            assert!(!root.join("vault.key").exists());
            assert!(tilo_files::native::private::read(&root.join("vault.key.dpapi")).is_ok());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_vault_that_cannot_be_opened_is_never_overwritten_or_misread() {
        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            vault.put(
                &AccountId::from("existing"),
                &Secret::password("preserve me"),
            );
            vault.try_save().unwrap();
            let dir = crate::vault_dir_in(&root);
            let before = std::fs::read(dir.join("vault.enc")).unwrap();
            std::fs::remove_file(dir.join(active_key_filename())).unwrap();
            let reopened = SecretVault::open_in(&root);
            assert!(reopened.try_save().is_err());
            assert!(!dir.join(active_key_filename()).exists());
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), before);
            std::fs::remove_dir_all(root).unwrap();
        }

        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            let id = AccountId::from("existing");
            vault.put(&id, &Secret::password("preserve me"));
            vault.try_save().unwrap();
            let dir = crate::vault_dir_in(&root);
            let before = std::fs::read(dir.join("vault.enc")).unwrap();
            let key = std::fs::read(dir.join(active_key_filename())).unwrap();
            std::fs::write(dir.join(active_key_filename()), [0u8; KEY_LEN]).unwrap();
            let mut failed = SecretVault::open_in(&root);
            std::fs::write(dir.join(active_key_filename()), key).unwrap();
            failed.put(&AccountId::from("new"), &Secret::password("new value"));
            assert!(failed.try_save().is_err());
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), before);
            assert_eq!(
                SecretVault::open_in(&root).get(&id).unwrap().password,
                "preserve me"
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        {
            let root = temp_root();
            let mut vault = SecretVault::open_in(&root);
            vault.put(
                &AccountId::from("existing"),
                &Secret::password("preserve me"),
            );
            vault.try_save().unwrap();
            let enc = crate::vault_dir_in(&root).join("vault.enc");
            std::fs::write(&enc, b"corrupt existing vault").unwrap();
            assert!(vault.try_save().is_err());
            assert_eq!(std::fs::read(&enc).unwrap(), b"corrupt existing vault");
            std::fs::remove_dir_all(root).unwrap();
        }

        {
            let root = temp_root();
            let dir = crate::vault_dir_in(&root);
            let key = local_file_key(&dir).unwrap();
            let blob = seal(&key, b"not valid JSON").unwrap();
            std::fs::write(dir.join("vault.enc"), &blob).unwrap();
            let vault = SecretVault::open_in(&root);
            assert!(vault.recovery_error().is_some());
            assert!(vault.try_save().is_err());
            assert_eq!(std::fs::read(dir.join("vault.enc")).unwrap(), blob);
            std::fs::remove_dir_all(root).unwrap();
        }

        {
            let root = temp_root();
            let id = AccountId::from("x");
            {
                let mut v = SecretVault::open_in(&root);
                v.put(&id, &Secret::password("pw"));
                v.save();
            }
            // Corrupt the key file: the sealed vault can no longer be opened, and it degrades to empty
            // rather than surfacing garbage.
            let key_path = crate::vault_dir_in(&root).join(active_key_filename());
            std::fs::write(&key_path, [0u8; KEY_LEN]).unwrap();
            let v = SecretVault::open_in(&root);
            assert!(
                v.get(&id).is_none(),
                "a wrong key must not decrypt the vault"
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }

    fn active_key_filename() -> &'static str {
        if cfg!(windows) {
            "vault.key.dpapi"
        } else {
            "vault.key"
        }
    }
    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("tilo-accounts-vault-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn a_thread_that_must_not_wait_commits_the_vault_on_a_writer_of_its_own() {
        let root = temp_root();
        let mut vault = SecretVault::open_in(&root);
        vault.put(&AccountId::from("frame-loop"), &Secret::password("kept"));
        {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread();
            assert_eq!(
                vault.commit().unwrap(),
                Saved::Pending,
                "the frame loop never writes the vault itself"
            );
            vault.put(
                &AccountId::from("edited-meanwhile"),
                &Secret::password("too"),
            );
        }
        // A caller that may wait takes the write under way, then writes
        // the edit made meanwhile.
        vault.try_save().unwrap();
        let reopened = SecretVault::open_in(&root);
        assert_eq!(
            reopened.get(&AccountId::from("frame-loop")),
            Some(Secret::password("kept"))
        );
        assert_eq!(
            reopened.get(&AccountId::from("edited-meanwhile")),
            Some(Secret::password("too"))
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sealed_vault_round_trips_and_never_exposes_the_secret() {
        {
            let key = VaultKey::from_bytes([7u8; KEY_LEN]);
            let plaintext = b"secret vault bytes";
            let blob = seal(&key, plaintext).expect("seal");
            // The sealed blob is not the plaintext.
            assert!(!blob.windows(plaintext.len()).any(|w| w == plaintext));
            assert!(blob.starts_with(b"TSEAL"));
            assert_eq!(open(&key, &blob).unwrap().as_slice(), plaintext);
            // A different key can't open it.
            assert!(open(&VaultKey::from_bytes([9u8; KEY_LEN]), &blob).is_none());
            let mut tampered = blob;
            let last = tampered.len() - 1;
            tampered[last] ^= 0x01;
            assert!(open(&key, &tampered).is_none());
        }

        {
            let root = temp_root();
            let id = AccountId::from("ada@example.com");
            {
                let mut v = SecretVault::open_in(&root);
                #[cfg(not(windows))]
                assert_eq!(v.custody(), SecretCustody::LocalFile);
                #[cfg(windows)]
                assert_eq!(v.custody(), SecretCustody::WindowsDpapi);
                v.put(&id, &Secret::password("hunter2"));
                v.save();
            }
            // A fresh open decrypts the persisted secret.
            let v = SecretVault::open_in(&root);
            assert_eq!(v.get(&id).unwrap().password, "hunter2");
            std::fs::remove_dir_all(&root).ok();
        }

        {
            let root = temp_root();
            let mut v = SecretVault::open_in(&root);
            v.put(&AccountId::from("a"), &Secret::password("s3cr3t-value"));
            v.save();
            let enc = crate::vault_dir_in(&root).join("vault.enc");
            let raw = std::fs::read(&enc).unwrap();
            assert!(
                !raw.windows(b"s3cr3t-value".len())
                    .any(|w| w == b"s3cr3t-value"),
                "password must not be recoverable from the sealed file"
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }
}
