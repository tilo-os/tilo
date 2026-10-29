//! Plans' and Transforms' resolved profiles and keys, reused between model
//! turns while what they were read from is unchanged.

use super::*;

/// How long a resolved profile and key are reused at most. Minted tokens are
/// also bounded by their expiry minus the refresh margin. Changes to
/// `settings.json` or the vault, and keys set or removed through this
/// gateway, take effect at once; this bounds what no file records, chiefly
/// the keyring holding the vault's key: once it is locked, a Plan within
/// this long still uses the key read while it was open, and a later one
/// consults (and asks to unlock) it again, as every Plan did before.
pub(super) const PROFILE_REUSE: Duration = Duration::from_secs(60);

/// Plans' and Transforms' resolved profiles and keys, kept between model
/// turns so a turn does not re-read and re-validate `settings.json`, ask the
/// keyring for the vault key and decrypt every account secret before its
/// POST. Memory only; a key is wiped when its entry is dropped.
pub(super) static PLAN_PROFILES: ProfileCache = ProfileCache::new();

/// One file as a reading of it found it. An atomic replacement changes the
/// inode, an edit in place the length or times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    /// `Ok(None)` for a file that is not there, `Err` when that cannot be told.
    fn of(path: &Path) -> Result<Option<Self>, ()> {
        use std::os::unix::fs::MetadataExt as _;
        match fs::metadata(path) {
            Ok(meta) => Ok(Some(Self {
                device: meta.dev(),
                inode: meta.ino(),
                length: meta.len(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        }
    }
}

/// The settings and the vault, as a reading of both found them.
pub(super) type SourceStamp = (settings_store::SettingsStamp, Option<FileStamp>);

/// What a resolved profile is read from: the settings `ProviderConfig::load`
/// reads and the credential store it decrypts.
pub(super) struct ProfileSources {
    vault: PathBuf,
}

impl ProfileSources {
    /// `None` where the vault is not a file under the config root, so
    /// nothing is kept.
    pub(super) fn current() -> Option<Self> {
        let root = accounts::config_root()?;
        Some(Self {
            vault: accounts::vault_dir_in(&root).join("vault.enc"),
        })
    }

    /// `None` when the vault cannot be told, or there are no settings.
    pub(super) fn stamp(&self) -> Option<SourceStamp> {
        let settings = settings_store::stamp()?;
        let vault = FileStamp::of(&self.vault).ok()?;
        Some((settings, vault))
    }
}

pub(super) struct ProfileCache {
    state: Mutex<CacheState>,
    /// How many times a profile was read rather than reused.
    reads: std::sync::atomic::AtomicU64,
}

struct CacheState {
    /// Advanced by [`ProfileCache::forget`]: a reading begun before is not kept.
    generation: u64,
    entries: Vec<CachedProfile>,
}

struct CachedProfile {
    profile_id: String,
    model_id: String,
    stamp: SourceStamp,
    read_at: Instant,
    until: Option<Instant>,
    resolved: ResolvedProfile,
}

impl ProfileCache {
    const fn new() -> Self {
        Self {
            state: Mutex::new(CacheState {
                generation: 0,
                entries: Vec::new(),
            }),
            reads: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock_recover()
    }

    /// The profile and key for `(profile id, model id)`: the kept ones while
    /// `stamp`, taken just before this call, is the one they were read
    /// under and they are younger than [`PROFILE_REUSE`]; else `read`'s,
    /// kept when stamped and nothing was forgotten meanwhile. A failed read
    /// is never kept.
    pub(super) fn resolve(
        &self,
        (profile_id, model_id): (&str, &str),
        stamp: Option<SourceStamp>,
        now: Instant,
        read: impl FnOnce() -> Result<(ResolvedProfile, Option<Instant>), ModelError>,
    ) -> Result<ResolvedProfile, ModelError> {
        let same =
            |entry: &CachedProfile| entry.profile_id == profile_id && entry.model_id == model_id;
        let generation = {
            let mut state = self.lock();
            state.entries.retain(|entry| fresh(entry, now));
            if let Some(entry) = state
                .entries
                .iter()
                .find(|entry| same(entry) && Some(entry.stamp) == stamp)
            {
                return Ok(entry.resolved.clone());
            }
            // A stale reading of this pair holds a key nothing will use.
            state.entries.retain(|entry| !same(entry));
            state.generation
        };
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (resolved, until) = read()?;
        if let Some(stamp) = stamp.filter(|_| until.is_none_or(|until| now < until)) {
            let mut state = self.lock();
            if state.generation == generation {
                state.entries.retain(|entry| !same(entry));
                state.entries.push(CachedProfile {
                    profile_id: profile_id.to_owned(),
                    model_id: model_id.to_owned(),
                    stamp,
                    read_at: now,
                    until,
                    resolved: resolved.clone(),
                });
            }
        }
        Ok(resolved)
    }

    /// Drop what is kept for `profile_id`, and keep no reading in progress:
    /// its key was just set or removed.
    pub(super) fn forget(&self, profile_id: &str) {
        let mut state = self.lock();
        state.generation = state.generation.wrapping_add(1);
        state.entries.retain(|entry| entry.profile_id != profile_id);
    }

    /// Drop every entry past [`PROFILE_REUSE`], so an idle gateway does not
    /// hold a key in memory longer than a busy one reuses it.
    pub(super) fn sweep(&self, now: Instant) {
        self.lock().entries.retain(|entry| fresh(entry, now));
    }
}

fn fresh(entry: &CachedProfile, now: Instant) -> bool {
    now.saturating_duration_since(entry.read_at) < PROFILE_REUSE
        && entry.until.is_none_or(|until| now < until)
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod profile_tests;
