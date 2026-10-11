//! Local registry of identities available to this login session.
//!
//! Private identity selection is deliberately not replicated. The registry
//! maps a durable room id to the local identity record this device presents
//! there; workspace ownership remains a shared cryptographic principal.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Serialize};

const REGISTRY_SCHEMA_VERSION: u32 = 1;
const REGISTRY_FILE: &str = "registry.json";
const MAX_IMPORTED_IDENTITY_BYTES: u64 = 1024 * 1024;
#[cfg(test)]
const LEGACY_PRIMARY_FILE: &str = "primary.json";

/// Public metadata for one locally available signing identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityProfile {
    /// Lowercase Ed25519 public-key hex. Also the stable registry key.
    pub id: String,
    /// User-selected label. Empty records receive a fingerprint label.
    pub display_name: String,
    /// Absolute path to the private identity record. Never replicated.
    pub record_path: PathBuf,
    /// The person principal represented by this identity.
    pub user_public_key: [u8; 32],
}

impl IdentityProfile {
    /// Capability-model person id for this identity.
    #[must_use]
    pub fn principal_id(&self) -> String {
        match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(self.user_public_key),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            _ => self.id.clone(),
        }
    }
}

/// Versioned local identity index plus per-workspace selections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityRegistry {
    pub schema_version: u32,
    /// Identity used for new and unbound workspaces.
    pub default_identity: Option<String>,
    pub identities: BTreeMap<String, IdentityProfile>,
    /// Durable room-id string to [`IdentityProfile::id`].
    pub workspace_identities: BTreeMap<String, String>,
}

impl Default for IdentityRegistry {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            default_identity: None,
            identities: BTreeMap::new(),
            workspace_identities: BTreeMap::new(),
        }
    }
}

impl IdentityRegistry {
    /// Register a validated record already stored at `record_path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the record cannot be loaded from the provided path or if
    /// the record disappears immediately after registration (should not occur).
    pub fn register_record(&mut self, record_path: &Path) -> io::Result<&IdentityProfile> {
        let record = runtime_identity::load_from_path(record_path.to_path_buf())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let public_key = record.group_public_key();
        let id = hex::encode(public_key);
        let display_name = if record.display_name().trim().is_empty() {
            format!("Identity {}", short_fingerprint(&id))
        } else {
            record.display_name().trim().to_string()
        };
        let profile = IdentityProfile {
            id: id.clone(),
            display_name,
            record_path: absolute_path(record_path)?,
            user_public_key: public_key,
        };
        self.identities.insert(id.clone(), profile);
        if self.default_identity.is_none() {
            self.default_identity = Some(id.clone());
        }
        self.identities
            .get(&id)
            .ok_or_else(|| io::Error::other("identity disappeared immediately after registration"))
    }

    /// Import an existing record into the managed identity directory.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or loading the source record fails, if creating
    /// the identity directory fails, or if the destination exists with a different key.
    pub fn import_record(
        &mut self,
        source: &Path,
        identity_dir: &Path,
    ) -> io::Result<&IdentityProfile> {
        let bytes = read_import_record(source)?;
        let record = runtime_identity::load_from_bytes(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let id = hex::encode(record.group_public_key());
        // Runtime store ids are capped at 64 characters and must begin with a
        // letter. Keep 220 fingerprint bits; the full key remains the registry
        // id and is rechecked before accepting an existing destination.
        let store_id = format!("identity-{}", id.get(..55).unwrap_or(&id));
        fs::create_dir_all(identity_dir)?;
        set_private_dir_mode(identity_dir)?;
        if let Some(existing) = runtime_identity::list_identities(identity_dir)?
            .into_iter()
            .find(|summary| summary.public_key == record.group_public_key())
        {
            runtime_identity::secure_identity(identity_dir, &existing.id)?;
            return self.register_record(&identity_dir.join(format!("{}.json", existing.id)));
        }
        let destination = identity_dir.join(format!("{store_id}.json"));
        if destination.exists() {
            let existing = runtime_identity::load_from_path(destination.clone())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            if existing.group_public_key() != record.group_public_key() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "identity id is already used by another record",
                ));
            }
        } else {
            runtime_identity::import_identity(identity_dir, &store_id, &bytes)?;
        }
        self.register_record(&destination)
    }

    /// Select an identity for a durable workspace room.
    ///
    /// # Errors
    ///
    /// Returns an error if the `identity_id` is not registered.
    pub fn bind_workspace(&mut self, room: impl Into<String>, identity_id: &str) -> io::Result<()> {
        if !self.identities.contains_key(identity_id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("identity {identity_id} is not registered"),
            ));
        }
        self.workspace_identities
            .insert(room.into(), identity_id.to_string());
        Ok(())
    }

    /// Identity selected for `room`, falling back to the default.
    pub fn selected_for_workspace(&self, room: &str) -> Option<&IdentityProfile> {
        self.workspace_identities
            .get(room)
            .and_then(|id| self.identities.get(id))
            .or_else(|| {
                self.default_identity
                    .as_ref()
                    .and_then(|id| self.identities.get(id))
            })
    }

    /// Change the default identity after validating it exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the `identity_id` is not registered.
    pub fn set_default(&mut self, identity_id: &str) -> io::Result<()> {
        if !self.identities.contains_key(identity_id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("identity {identity_id} is not registered"),
            ));
        }
        self.default_identity = Some(identity_id.to_string());
        Ok(())
    }
}

fn read_import_record(source: &Path) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(source)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_IMPORTED_IDENTITY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity import must be a regular file no larger than 1 MiB",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    (&mut file)
        .take(MAX_IMPORTED_IDENTITY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORTED_IDENTITY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity import exceeds the 1 MiB limit",
        ));
    }
    Ok(bytes)
}

/// Identity of an identity directory's contents, as far as rename-based
/// writers can change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DirStamp {
    dev: u64,
    ino: u64,
    modified: std::time::SystemTime,
}

impl DirStamp {
    #[cfg(unix)]
    pub(crate) fn of(dir: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(dir).ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            modified: meta.modified().ok()?,
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn of(_dir: &Path) -> Option<Self> {
        None
    }

    /// Old enough that a later write cannot share its timestamp.
    pub(crate) fn settled(self) -> bool {
        std::time::SystemTime::now()
            .duration_since(self.modified)
            .is_ok_and(|age| age >= std::time::Duration::from_secs(2))
    }
}

/// What [`IdentityRegistryStore::load_reconciled_prompt`] answers from.
#[derive(Clone)]
struct PromptSnapshot {
    dir: PathBuf,
    stamp: Option<DirStamp>,
    registry: std::sync::Arc<IdentityRegistry>,
}

/// One snapshot per identity directory.
static PROMPT_SNAPSHOT: std::sync::Mutex<Vec<PromptSnapshot>> = std::sync::Mutex::new(Vec::new());
/// The directories a background reconcile is running for.
static PROMPT_REFRESHING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Counts the registry saves, so a refresh that began before one does not
/// replace what the writer's next query read.
static PROMPT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn stop_refreshing(dir: &Path) {
    if let Ok(mut running) = PROMPT_REFRESHING.lock() {
        running.retain(|candidate| candidate != dir);
    }
}

fn forget_prompt_snapshot(dir: &Path) {
    PROMPT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    if let Ok(mut slots) = PROMPT_SNAPSHOT.lock() {
        slots.retain(|held| held.dir != dir);
    }
}

#[cfg(test)]
thread_local! {
    /// Reconciles run by this thread, so a test can tell a call that read the
    /// disk itself from one that was answered from memory.
    static RECONCILES_ON_THIS_THREAD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Disk owner for the registry.
#[derive(Clone, Debug)]
pub struct IdentityRegistryStore {
    identity_dir: PathBuf,
    registry_path: PathBuf,
}

impl IdentityRegistryStore {
    /// Open the identity registry rooted at the environment's identity
    /// directory (`XDG_CONFIG_HOME` or `HOME`).
    ///
    /// # Errors
    ///
    /// Returns an error if neither `XDG_CONFIG_HOME` nor `HOME` is set.
    pub fn from_env() -> io::Result<Self> {
        Ok(Self::at(resolve_identity_dir()?))
    }

    #[must_use]
    pub fn at(identity_dir: impl Into<PathBuf>) -> Self {
        let identity_dir = identity_dir.into();
        let registry_path = identity_dir.join(REGISTRY_FILE);
        Self {
            identity_dir,
            registry_path,
        }
    }

    #[must_use]
    pub fn identity_dir(&self) -> &Path {
        &self.identity_dir
    }

    #[must_use]
    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    /// Load the identity registry from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the registry file fails, if the JSON is invalid,
    /// or if the schema version is unsupported.
    pub fn load(&self) -> io::Result<IdentityRegistry> {
        if !self.registry_path.exists() {
            return Ok(IdentityRegistry::default());
        }
        let bytes = fs::read(&self.registry_path)?;
        let registry: IdentityRegistry = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if registry.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported identity registry schema {}",
                    registry.schema_version
                ),
            ));
        }
        Ok(registry)
    }

    /// Save the identity registry to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if creating the identity directory, setting permissions,
    /// serializing the registry, or writing the file fails.
    pub fn save(&self, registry: &IdentityRegistry) -> io::Result<()> {
        fs::create_dir_all(&self.identity_dir)?;
        set_private_dir_mode(&self.identity_dir)?;
        let bytes = serde_json::to_vec_pretty(registry)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let written =
            tilo_files::native::atomic::write_with_mode(&self.registry_path, &bytes, 0o600);
        // A change made here is read back by the writer's next query, not one
        // refresh later.
        forget_prompt_snapshot(&self.identity_dir);
        // The identity a workspace selected decides whose fleet admits.
        crate::policy_change::announce();
        written
    }

    /// Load the registry after registering every identity record in the
    /// directory and adopting the runtime's active record as the default.
    /// Provisioning, onboarding and the Users pane write records and the
    /// active marker through `runtime_identity`, not through this registry.
    /// Idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if loading the registry, listing identities, loading a record,
    /// setting permissions, or saving the registry fails.
    pub fn load_reconciled(&self) -> io::Result<IdentityRegistry> {
        fs::create_dir_all(&self.identity_dir)?;
        set_private_dir_mode(&self.identity_dir)?;
        let (registry, changed) = self.reconcile()?;
        if changed {
            self.save(&registry)?;
        }
        Ok(registry)
    }

    /// The registry with every record in the directory registered and the
    /// runtime's active record as its default, and whether that differs from
    /// the saved one. Reads only.
    fn reconcile(&self) -> io::Result<(IdentityRegistry, bool)> {
        #[cfg(test)]
        RECONCILES_ON_THIS_THREAD.with(|count| count.set(count.get() + 1));
        let mut registry = self.load()?;
        let mut changed = false;
        for summary in runtime_identity::list_identities(&self.identity_dir)? {
            let id = hex::encode(summary.public_key);
            let path = self.identity_dir.join(format!("{}.json", summary.id));
            let profile = IdentityProfile {
                id: id.clone(),
                display_name: if summary.display_name.trim().is_empty() {
                    format!("Identity {}", short_fingerprint(&id))
                } else {
                    summary.display_name.trim().to_string()
                },
                record_path: absolute_path(&path)?,
                user_public_key: summary.public_key,
            };
            if registry.identities.get(&id) != Some(&profile) {
                registry.identities.insert(id.clone(), profile);
                changed = true;
            }
            if summary.active && registry.default_identity.as_deref() != Some(id.as_str()) {
                registry.default_identity = Some(id);
                changed = true;
            }
        }
        if registry.default_identity.is_none()
            && let Some(id) = registry.identities.keys().next().cloned()
        {
            registry.default_identity = Some(id);
            changed = true;
        }
        Ok((registry, changed))
    }

    /// [`Self::load_reconciled`] for read-only queries that run on every
    /// painted frame and app launch (which identity a room presents). It
    /// never writes the identity directory: the reconciled registry is
    /// returned, not saved. Every writer in the identity directory replaces
    /// files by rename, so an unchanged directory (same inode and
    /// modification time) holds the registry the last reconcile produced. A
    /// directory changed within the last two seconds is reconciled again on
    /// each call: a coarse filesystem clock can give a write that lands right
    /// after a reconcile the same time.
    ///
    /// # Errors
    ///
    /// Returns an error if loading the registry, listing identities or
    /// resolving a record path fails.
    pub fn load_reconciled_cached(&self) -> io::Result<IdentityRegistry> {
        static CACHE: std::sync::Mutex<Option<(PathBuf, DirStamp, IdentityRegistry)>> =
            std::sync::Mutex::new(None);
        let before = DirStamp::of(&self.identity_dir);
        if let Some(stamp) = before
            && let Ok(cache) = CACHE.lock()
            && let Some((dir, cached_stamp, registry)) = cache.as_ref()
            && *dir == self.identity_dir
            && *cached_stamp == stamp
        {
            return Ok(registry.clone());
        }
        let (registry, _) = self.reconcile()?;
        if let Some(stamp) = DirStamp::of(&self.identity_dir)
            && before == Some(stamp)
            && stamp.settled()
            && let Ok(mut cache) = CACHE.lock()
        {
            *cache = Some((self.identity_dir.clone(), stamp, registry.clone()));
        }
        Ok(registry)
    }

    /// [`Self::load_reconciled_cached`] for a thread that must not wait on the
    /// disk: the compositor's input and frame loop, which asks which identity
    /// a room presents on every keystroke and frame.
    ///
    /// The registry the last reconcile produced is returned at once. When the
    /// identity directory has changed since, or changed too recently to be
    /// trusted ([`DirStamp::settled`]), one background thread reconciles it
    /// again and the next call sees the result. Reconciling lists and parses
    /// every identity record, which took hundreds of milliseconds on a busy
    /// disk and held the loop for all of them. Only the first call, with
    /// nothing to return yet, reads inline. A registry this process saves is
    /// read back inline, so a writer never sees its own change late.
    ///
    /// # Errors
    ///
    /// Returns an error if the first read fails.
    pub fn load_reconciled_prompt(&self) -> io::Result<std::sync::Arc<IdentityRegistry>> {
        let current = DirStamp::of(&self.identity_dir);
        let held = PROMPT_SNAPSHOT.lock().ok().and_then(|slots| {
            slots
                .iter()
                .find(|held| held.dir == self.identity_dir)
                .cloned()
        });
        if let Some(held) = held {
            if current != held.stamp || !current.is_some_and(DirStamp::settled) {
                self.refresh_prompt_snapshot_in_background();
            }
            return Ok(held.registry);
        }
        let registry = std::sync::Arc::new(self.reconcile()?.0);
        self.keep_prompt_snapshot(current, &registry);
        Ok(registry)
    }

    fn keep_prompt_snapshot(
        &self,
        stamp: Option<DirStamp>,
        registry: &std::sync::Arc<IdentityRegistry>,
    ) {
        if let Ok(mut slots) = PROMPT_SNAPSHOT.lock() {
            slots.retain(|held| held.dir != self.identity_dir);
            slots.push(PromptSnapshot {
                dir: self.identity_dir.clone(),
                stamp,
                registry: std::sync::Arc::clone(registry),
            });
        }
    }

    /// Reconcile on a worker, one at a time.
    fn refresh_prompt_snapshot_in_background(&self) {
        use std::sync::atomic::Ordering;
        let Ok(mut running) = PROMPT_REFRESHING.lock() else {
            return;
        };
        if running.contains(&self.identity_dir) {
            return;
        }
        running.push(self.identity_dir.clone());
        drop(running);
        let store = self.clone();
        let spawned = std::thread::Builder::new()
            .name("identity-refresh".to_owned())
            .spawn(move || {
                // The stamp read first: a write landing during the reconcile
                // differs from it and is reconciled again.
                let generation = PROMPT_GENERATION.load(Ordering::Acquire);
                let stamp = DirStamp::of(&store.identity_dir);
                if let Ok((registry, _)) = store.reconcile()
                    && PROMPT_GENERATION.load(Ordering::Acquire) == generation
                {
                    store.keep_prompt_snapshot(stamp, &std::sync::Arc::new(registry));
                }
                stop_refreshing(&store.identity_dir);
            });
        if spawned.is_err() {
            stop_refreshing(&self.identity_dir);
        }
    }

    /// Import a new identity record and return its profile.
    ///
    /// # Errors
    ///
    /// Returns an error if migration, importing the record, or saving the registry fails.
    pub fn import(&self, source: &Path) -> io::Result<IdentityProfile> {
        let mut registry = self.load_reconciled()?;
        let profile = registry.import_record(source, &self.identity_dir)?.clone();
        self.save(&registry)?;
        Ok(profile)
    }

    /// Restore an encrypted portable backup into the managed identity store.
    ///
    /// Decryption remains inside `runtime-identity`; this registry only sees
    /// the public fingerprint and the final secured metadata path. Re-importing
    /// a backup already present on this login is idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if backup verification, migration, import, registration,
    /// or registry persistence fails.
    pub fn import_portable(&self, source: &Path, passphrase: &[u8]) -> io::Result<IdentityProfile> {
        let info = runtime_identity::verify_portable_backup(source, passphrase)?;
        let id = hex::encode(info.group_public_key);
        let mut registry = self.load_reconciled()?;
        if let Some(existing) = registry.identities.get(&id) {
            return Ok(existing.clone());
        }

        // Runtime ids are capped at 64 characters and must start with a
        // letter. The full fingerprint remains the registry's stable id.
        let runtime_id = format!("identity-{}", id.get(..55).unwrap_or(&id));
        let destination = runtime_identity::import_portable_backup_matching(
            source,
            passphrase,
            &self.identity_dir,
            &runtime_id,
            info.group_public_key,
        )?;
        let profile = registry.register_record(&destination)?.clone();
        self.save(&registry)?;
        Ok(profile)
    }

    /// Bind an identity to a workspace in the registry.
    ///
    /// # Errors
    ///
    /// Returns an error if migration, binding the workspace, or saving the registry fails.
    pub fn bind_workspace(&self, room: &str, identity_id: &str) -> io::Result<()> {
        let mut registry = self.load_reconciled()?;
        registry.bind_workspace(room, identity_id)?;
        self.save(&registry)
    }
}

/// Resolve the selected private record for a room. Registry failure is an
/// error rather than a silent fallback to another person's identity.
///
/// # Errors
///
/// Returns an error if loading the registry or migration fails.
pub fn selected_identity_path(room: &str) -> io::Result<Option<PathBuf>> {
    let store = IdentityRegistryStore::from_env()?;
    let registry = store.load_reconciled_cached()?;
    Ok(registry
        .selected_for_workspace(room)
        .map(|profile| profile.record_path.clone()))
}

/// Resolve the selected person principal for a room.
///
/// # Errors
///
/// Returns an error if loading the registry or migration fails.
pub fn selected_principal_id(room: &str) -> io::Result<Option<String>> {
    let store = IdentityRegistryStore::from_env()?;
    let registry = store.load_reconciled_cached()?;
    Ok(registry
        .selected_for_workspace(room)
        .map(IdentityProfile::principal_id))
}

/// [`selected_principal_id`] for the compositor's input and frame loop: it
/// never waits for the disk after the first call
/// ([`IdentityRegistryStore::load_reconciled_prompt`]).
///
/// # Errors
///
/// Returns an error if the identity directory cannot be resolved or the first
/// read fails.
pub fn selected_principal_id_prompt(room: &str) -> io::Result<Option<String>> {
    let store = IdentityRegistryStore::from_env()?;
    let registry = store.load_reconciled_prompt()?;
    Ok(registry
        .selected_for_workspace(room)
        .map(IdentityProfile::principal_id))
}

/// Public profile selected for a room, for shell chrome/settings.
///
/// # Errors
///
/// Returns an error if loading the registry or migration fails.
pub fn selected_identity_profile(room: &str) -> io::Result<Option<IdentityProfile>> {
    let store = IdentityRegistryStore::from_env()?;
    let registry = store.load_reconciled_cached()?;
    Ok(registry.selected_for_workspace(room).cloned())
}

/// Resolve the identity directory from environment variables.
///
/// # Errors
///
/// Returns an error if neither `XDG_CONFIG_HOME` nor `HOME` is set.
fn resolve_identity_dir() -> io::Result<PathBuf> {
    runtime_identity::identity_dir_from_env()
}

/// Convert a relative path to absolute, or return unchanged if already absolute.
///
/// # Errors
///
/// Returns an error if getting the current directory fails.
fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

/// Set directory permissions to 0o700 on Unix systems.
///
/// # Errors
///
/// Returns an error if setting permissions fails.
fn set_private_dir_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn short_fingerprint(hex: &str) -> String {
    hex.get(..8).unwrap_or(hex).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use threshold_identity::ThresholdIdentity;
    use tilo_identity_ceremony::IdentityRecord;

    fn record(name: &str, seed: u8) -> IdentityRecord {
        let identity = ThresholdIdentity::from_single_device_seed(&[seed; 32]);
        IdentityRecord::from_threshold_identity(
            &identity,
            name,
            [10, 20, 30],
            std::time::SystemTime::UNIX_EPOCH,
        )
        .expect("record")
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = tilo_clock::try_unix_now_ns().expect("clock");
        let path = std::env::temp_dir().join(format!(
            "tilo-identity-registry-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn write_record(path: &Path, record: &IdentityRecord) {
        fs::write(path, record.to_json_bytes().expect("encode")).expect("write");
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private record");
    }

    /// The identity service answers every launch from the cached read; it
    /// reconciles in memory and leaves saving to provisioning.
    #[test]
    fn the_per_launch_read_never_writes_the_identity_directory() {
        let dir = temp_dir("read-only");
        write_record(&dir.join(LEGACY_PRIMARY_FILE), &record("Alice", 8));
        #[cfg(unix)]
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("mode");
        let entries = || {
            let mut names: Vec<_> = fs::read_dir(&dir)
                .expect("list")
                .map(|entry| entry.expect("entry").file_name())
                .collect();
            names.sort();
            let meta = fs::metadata(&dir).expect("dir");
            (names, meta.modified().expect("mtime"), meta.permissions())
        };
        let before = entries();
        let store = IdentityRegistryStore::at(&dir);
        let registry = store.load_reconciled_cached().expect("read");
        let selected = registry.selected_for_workspace("room-a").expect("default");
        assert_eq!(selected.display_name, "Alice");
        assert_eq!(entries(), before);
        assert!(!store.registry_path().exists());
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_changed_identity_directory_is_reconciled_off_the_calling_thread() {
        let dir = temp_dir("prompt");
        write_record(&dir.join("alice.json"), &record("Alice", 31));
        let store = IdentityRegistryStore::at(&dir);
        let reconciles = || RECONCILES_ON_THIS_THREAD.with(std::cell::Cell::get);
        let first = store.load_reconciled_prompt().expect("first read");
        assert_eq!(reconciles(), 1, "the first call has nothing to return yet");
        assert_eq!(first.identities.len(), 1);
        // Another process adds an identity: the directory's stamp moves.
        write_record(&dir.join("bob.json"), &record("Bob", 32));
        let answered = store.load_reconciled_prompt().expect("stale read");
        assert_eq!(
            reconciles(),
            1,
            "the loop's thread never reads the disk again"
        );
        assert_eq!(answered.identities.len(), 1, "it answers from memory");
        // The background reconcile lands; a later call sees both.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let now = store.load_reconciled_prompt().expect("read");
            if now.identities.len() == 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the refresh never landed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(reconciles(), 1, "and still not on this thread");
        // A registry this process saves is read back by its writer at once.
        let mut registry = (*store.load_reconciled_prompt().expect("read")).clone();
        let bob = registry
            .identities
            .values()
            .find(|profile| profile.record_path.ends_with("bob.json"))
            .expect("bob")
            .id
            .clone();
        registry.bind_workspace("room-p", &bob).expect("bind");
        store.save(&registry).expect("save");
        let bound = store.load_reconciled_prompt().expect("after save");
        assert_eq!(
            bound
                .selected_for_workspace("room-p")
                .map(|profile| profile.id.as_str()),
            Some(bob.as_str())
        );
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn identity_registry_migrates_selects_and_bounds_legacy_imports() {
        // Phase `migrates_legacy_primary_and_selects_it_by_default`.
        {
            let dir = temp_dir("legacy");
            write_record(&dir.join(LEGACY_PRIMARY_FILE), &record("Alice", 7));
            let store = IdentityRegistryStore::at(&dir);
            let registry = store.load_reconciled().expect("migrate");
            let selected = registry.selected_for_workspace("room-a").expect("default");
            assert_eq!(selected.display_name, "Alice");
            assert_eq!(selected.record_path, dir.join(LEGACY_PRIMARY_FILE));
            assert!(store.registry_path().exists());
            fs::remove_dir_all(dir).expect("cleanup");
        }
        // Phase `one_identity_can_be_selected_per_workspace`.
        {
            let dir = temp_dir("binding");
            let alice_path = dir.join("alice-source.json");
            let bob_path = dir.join("bob-source.json");
            write_record(&alice_path, &record("Alice", 11));
            write_record(&bob_path, &record("Bob", 12));
            let store = IdentityRegistryStore::at(&dir);
            let alice = store.import(&alice_path).expect("import alice");
            let bob = store.import(&bob_path).expect("import bob");
            store
                .bind_workspace("room-a", &alice.id)
                .expect("bind alice");
            store.bind_workspace("room-b", &bob.id).expect("bind bob");
            let registry = store.load().expect("load");
            assert_eq!(
                registry
                    .selected_for_workspace("room-a")
                    .map(|profile| profile.id.as_str()),
                Some(alice.id.as_str())
            );
            assert_eq!(
                registry
                    .selected_for_workspace("room-b")
                    .map(|profile| profile.id.as_str()),
                Some(bob.id.as_str())
            );
            fs::remove_dir_all(dir).expect("cleanup");
        }
        // Phase `unknown_identity_cannot_be_bound`.
        {
            let mut registry = IdentityRegistry::default();
            let error = registry
                .bind_workspace("room-a", "missing")
                .expect_err("must reject unknown identity");
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
        }
        // Phase `workspace_selector_cycles_registered_identities`.
        {
            let dir = temp_dir("cycle");
            let alice_path = dir.join("alice-source.json");
            let bob_path = dir.join("bob-source.json");
            write_record(&alice_path, &record("Alice", 21));
            write_record(&bob_path, &record("Bob", 22));
            let store = IdentityRegistryStore::at(&dir);
            let alice = store.import(&alice_path).expect("alice");
            let bob = store.import(&bob_path).expect("bob");
            store
                .bind_workspace("room-cycle", &alice.id)
                .expect("bind alice");

            let mut registry = (*store.load_reconciled_prompt().expect("read")).clone();
            let ids: Vec<_> = registry.identities.keys().cloned().collect();
            let current = registry
                .selected_for_workspace("room-cycle")
                .expect("current")
                .id
                .clone();
            let next = ids
                .iter()
                .cycle()
                .skip_while(|id| **id != current)
                .nth(1)
                .expect("next")
                .clone();
            registry
                .bind_workspace("room-cycle", &next)
                .expect("cycle bind");
            assert_eq!(
                registry
                    .selected_for_workspace("room-cycle")
                    .map(|profile| profile.id.as_str()),
                Some(next.as_str())
            );
            assert!(next == alice.id || next == bob.id);
            fs::remove_dir_all(dir).expect("cleanup");
        }
        // Phase `legacy_import_rejects_oversized_records_before_parsing`.
        {
            let dir = temp_dir("oversized-import");
            let source = dir.join("oversized.json");
            let file = fs::File::create(&source).expect("source");
            file.set_len(MAX_IMPORTED_IDENTITY_BYTES + 1)
                .expect("sparse oversized record");
            let store = IdentityRegistryStore::at(dir.join("managed"));
            let error = store.import(&source).expect_err("oversized import");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            fs::remove_dir_all(dir).expect("cleanup");
        }
    }

    #[cfg(unix)]
    #[test]
    fn legacy_import_refuses_symlink_sources() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("symlink-import");
        let source = dir.join("source.json");
        write_record(&source, &record("Alice", 41));
        let link = dir.join("identity.json");
        symlink(&source, &link).expect("source symlink");
        let store = IdentityRegistryStore::at(dir.join("managed"));
        assert!(store.import(&link).is_err());
        fs::remove_dir_all(dir).expect("cleanup");
    }
}
