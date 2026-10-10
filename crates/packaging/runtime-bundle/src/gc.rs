//! Collecting the per-user runtime bundles nothing needs any more.
//!
//! Every runtime build a device installs lands in its own
//! `<root>/<version>/` ([`crate::bundle_root`]), and the runtime is rebuilt
//! with every release, so without collection each app update that moves to
//! a new build leaves the previous ~200 MB bundle behind.
//!
//! Beside the bundles the root holds the collector's state, every name of
//! which starts with a dot and none of which is ever a bundle:
//!
//! ```text
//! <root>/
//!   <version>/          a bundle (selected when its bundle.toml exists)
//!   .gc.lock            installs hold it shared, a collection exclusively
//!   .leases/<version>   unix seconds of the bundle's last install
//!   .pins/<version>     kept until unpinned
//!   .trash/<version>.<pid>.<nanos>/
//!                       a bundle on its way out
//!   .install-<pid>/     an install's staging
//! ```
//!
//! A bundle stays while an installed package names it, while it is pinned,
//! for a grace period after it was installed (an app's first open installs
//! the runtime and then the app, in two processes), and while a running app
//! has it mounted. Everything else is collected.
//!
//! Collection is crash-safe because a bundle leaves the version namespace in
//! one `rename(2)` before any of its files is deleted: a directory in
//! `.trash/` is never found by a version lookup (it lies one level deeper,
//! under a name no version has a `bundle.toml` at), so a collection stopped
//! halfway leaves either the whole bundle or none of it selectable. The next
//! collection empties the trash. Nothing outside the root is touched and no
//! symbolic link is followed. Bundle directories are never written to, so a
//! bundle sealed with fs-verity stays sealed until it is gone.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use advisory_lock::{FileLock, LockMode, TryLockError};

/// The lock installs share and a collection holds alone.
pub const GC_LOCK_FILE: &str = ".gc.lock";
/// Where a bundle goes before it is deleted.
pub const TRASH_DIR: &str = ".trash";
/// When each bundle was last installed.
pub const LEASES_DIR: &str = ".leases";
/// The bundles kept until unpinned.
pub const PINS_DIR: &str = ".pins";
/// The name prefix of an install's staging directory.
pub const STAGING_PREFIX: &str = ".install-";
/// How long a freshly installed bundle stays without anything naming it:
/// long enough for the app install that follows a runtime install.
pub const DEFAULT_GRACE_SECS: u64 = 60 * 60;

/// Why a bundle was kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// An installed package names it.
    Referenced,
    /// It is pinned.
    Pinned,
    /// It was installed within the grace period.
    Leased,
    /// A running app has it mounted.
    InUse,
}

/// What one collection did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Collected {
    /// Another install or collection held the lock; nothing was looked at.
    pub busy: bool,
    /// Bundles removed (or, on a dry run, that would be), with their bytes.
    pub removed: Vec<(String, u64)>,
    /// Bundles kept, and why.
    pub kept: Vec<(String, Kept)>,
    /// Bytes of earlier collections' trash and of abandoned install staging
    /// removed this time.
    pub swept_bytes: u64,
}

impl Collected {
    /// Every byte this collection freed.
    #[must_use]
    pub fn freed_bytes(&self) -> u64 {
        self.removed
            .iter()
            .fold(self.swept_bytes, |sum, (_, bytes)| {
                sum.saturating_add(*bytes)
            })
    }
}

/// Settings for one [`BundleRoot::collect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// How long a lease keeps a bundle; see [`DEFAULT_GRACE_SECS`].
    pub grace_secs: u64,
    /// Unix seconds now.
    pub now: u64,
    /// Report what would be removed, removing nothing.
    pub dry_run: bool,
}

/// Whether a running app holds the bundle at a path, which carries a
/// version. Taken fresh by [`InUse`] for each look.
pub type Holds = Box<dyn Fn(&Path, &str) -> bool>;

/// A fresh look at what running apps hold.
pub type InUse<'a> = &'a dyn Fn() -> Holds;

/// The bundle versions installed packages name, read under the lock.
pub type References<'a> = &'a dyn Fn() -> io::Result<BTreeSet<String>>;

/// A directory of per-user runtime bundles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleRoot {
    root: PathBuf,
}

impl BundleRoot {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root the environment names; see [`crate::bundle_root`].
    ///
    /// # Errors
    ///
    /// See [`crate::bundle_root`].
    pub fn from_env() -> Result<Self, crate::BundleError> {
        crate::bundle_root().map(Self::new)
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The bundle directory of `version`.
    pub fn bundle(&self, version: &str) -> PathBuf {
        self.root.join(version)
    }

    /// Hold off collection while installing: any number of installs share
    /// this lock, and a collection skips while any holds it.
    ///
    /// # Errors
    ///
    /// Returns the error from creating the root or locking.
    pub fn install_lock(&self) -> io::Result<FileLock> {
        fs::create_dir_all(&self.root)?;
        FileLock::open(&self.root.join(GC_LOCK_FILE), LockMode::Shared)
    }

    /// Record that `version` was installed (or found installed) at `now`,
    /// so it survives the grace period whatever names it.
    ///
    /// # Errors
    ///
    /// Returns the write error.
    pub fn lease(&self, version: &str, now: u64) -> io::Result<()> {
        let path = self.state_file(LEASES_DIR, version)?;
        write_atomically(&path, now.to_string().as_bytes())
    }

    /// Keep `version` until [`Self::unpin`].
    ///
    /// # Errors
    ///
    /// Returns the write error.
    pub fn pin(&self, version: &str) -> io::Result<()> {
        let path = self.state_file(PINS_DIR, version)?;
        write_atomically(&path, b"")
    }

    /// Stop keeping `version` for its pin alone.
    ///
    /// # Errors
    ///
    /// Returns a removal error other than the pin being absent.
    pub fn unpin(&self, version: &str) -> io::Result<()> {
        remove_file_if_present(&self.state_file(PINS_DIR, version)?)
    }

    #[must_use]
    pub fn is_pinned(&self, version: &str) -> bool {
        self.state_file(PINS_DIR, version)
            .is_ok_and(|path| path.is_file())
    }

    /// Unix seconds of `version`'s last install, if recorded.
    #[must_use]
    pub fn leased_at(&self, version: &str) -> Option<u64> {
        let path = self.state_file(LEASES_DIR, version).ok()?;
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// Take `version`'s bundle out of the version namespace in one rename,
    /// so no lookup finds it from here on, and return where it went. `None`
    /// when there is no such bundle. The caller deletes it with
    /// [`Self::purge`] once nothing holds it.
    ///
    /// # Errors
    ///
    /// Returns a version that names no plain directory component, or the
    /// error from creating the trash or renaming.
    pub fn retire(&self, version: &str) -> io::Result<Option<PathBuf>> {
        check_version(version)?;
        let bundle = self.bundle(version);
        match fs::symlink_metadata(&bundle) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                // A link or a file where a bundle belongs: never follow it.
                remove_file_if_present(&bundle)?;
                return Ok(None);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let trash = self.trash_dir()?;
        let nanos = tilo_clock::unix_now_ns() % 1_000_000_000;
        let target = trash.join(format!("{version}.{}.{nanos}", std::process::id()));
        fs::rename(&bundle, &target)?;
        Ok(Some(target))
    }

    /// Delete a bundle [`Self::retire`] moved to the trash, unless `holds`
    /// says a running app has it mounted; then it stays in the trash, which
    /// no lookup reads, until a later collection finds it free. Returns
    /// whether it was deleted.
    ///
    /// # Errors
    ///
    /// Returns a path outside this root's trash, or the removal error.
    pub fn purge(&self, trashed: &Path, holds: &Holds) -> io::Result<bool> {
        let trash = self.root.join(TRASH_DIR);
        let Some(name) = trashed
            .strip_prefix(&trash)
            .ok()
            .filter(|rest| rest.components().count() == 1)
            .and_then(Path::to_str)
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not in {}", trashed.display(), trash.display()),
            ));
        };
        if holds(trashed, trashed_version(name)) {
            return Ok(false);
        }
        remove_entry(trashed)?;
        Ok(true)
    }

    /// Remove every bundle nothing keeps; see the module documentation for
    /// what keeps one. `references` and `in_use` are read only once the
    /// collection holds the lock, and `in_use` is read again after the
    /// renames, so a launch that mounted a bundle in between keeps it.
    ///
    /// # Errors
    ///
    /// Returns a failure to read the root, the references, or to move or
    /// delete a bundle. A bundle that failed to delete stays in the trash,
    /// where nothing selects it, for the next collection.
    pub fn collect(
        &self,
        references: References<'_>,
        in_use: InUse<'_>,
        options: Options,
    ) -> io::Result<Collected> {
        let mut out = Collected::default();
        if !self.root.is_dir() {
            return Ok(out);
        }
        let _lock = match FileLock::try_open(&self.root.join(GC_LOCK_FILE), LockMode::Exclusive) {
            Ok(lock) => lock,
            Err(TryLockError::WouldBlock) => {
                out.busy = true;
                return Ok(out);
            }
            Err(TryLockError::Error(error)) => return Err(error),
        };
        let named = references()?;
        let holds = in_use();
        out.swept_bytes = self.sweep_leftovers(&holds, options.dry_run)?;
        let mut leaving = Vec::new();
        for (version, path) in self.bundles()? {
            let kept = if named.contains(&version) {
                Some(Kept::Referenced)
            } else if self.is_pinned(&version) {
                Some(Kept::Pinned)
            } else if self
                .leased_at(&version)
                .is_some_and(|at| options.now.saturating_sub(at) < options.grace_secs)
            {
                Some(Kept::Leased)
            } else if holds(&path, &version) {
                Some(Kept::InUse)
            } else {
                None
            };
            match kept {
                Some(why) => out.kept.push((version, why)),
                None => leaving.push((version, tree_size(&path))),
            }
        }
        if options.dry_run {
            out.removed = leaving;
            return Ok(out);
        }
        let mut retired = Vec::new();
        for (version, bytes) in leaving {
            if let Some(trashed) = self.retire(&version)? {
                retired.push((version, bytes, trashed));
            }
        }
        // A launch may have mounted a bundle between the first look and its
        // rename: its mount follows the directory into the trash.
        let holds = in_use();
        for (version, bytes, trashed) in retired {
            if self.purge(&trashed, &holds)? {
                remove_file_if_present(&self.state_file(LEASES_DIR, &version)?)?;
                out.removed.push((version, bytes));
            } else {
                if !self.bundle(&version).exists() {
                    fs::rename(&trashed, self.bundle(&version))?;
                }
                out.kept.push((version, Kept::InUse));
            }
        }
        self.forget_stale_leases(&options)?;
        Ok(out)
    }

    /// Every bundle directory: plain directories directly under the root
    /// whose name is a version. State, trash and staging start with a dot.
    fn bundles(&self) -> io::Result<Vec<(String, PathBuf)>> {
        let mut found = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') || check_version(&name).is_err() {
                continue;
            }
            found.push((name, entry.path()));
        }
        found.sort();
        Ok(found)
    }

    /// Empty the trash of earlier collections and remove install staging
    /// left by an install that died: while the collection holds the lock no
    /// install runs, so every staging directory is abandoned. Returns the
    /// bytes removed.
    fn sweep_leftovers(&self, holds: &Holds, dry_run: bool) -> io::Result<u64> {
        let mut bytes = 0u64;
        let trash = self.root.join(TRASH_DIR);
        if fs::symlink_metadata(&trash).is_ok_and(|meta| meta.is_dir()) {
            for entry in fs::read_dir(&trash)? {
                let path = entry?.path();
                let size = tree_size(&path);
                if dry_run || self.purge(&path, holds)? {
                    bytes = bytes.saturating_add(size);
                }
            }
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(STAGING_PREFIX))
            {
                continue;
            }
            let path = entry.path();
            bytes = bytes.saturating_add(tree_size(&path));
            if !dry_run {
                remove_entry(&path)?;
            }
        }
        Ok(bytes)
    }

    /// Drop the lease of every bundle that is gone once it has expired.
    fn forget_stale_leases(&self, options: &Options) -> io::Result<()> {
        let leases = self.root.join(LEASES_DIR);
        let Ok(entries) = fs::read_dir(&leases) else {
            return Ok(());
        };
        for entry in entries {
            let entry = entry?;
            let Some(version) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let expired = self
                .leased_at(&version)
                .is_none_or(|at| options.now.saturating_sub(at) >= options.grace_secs);
            if expired && !self.bundle(&version).exists() {
                remove_file_if_present(&entry.path())?;
            }
        }
        Ok(())
    }

    fn trash_dir(&self) -> io::Result<PathBuf> {
        let trash = self.root.join(TRASH_DIR);
        match fs::symlink_metadata(&trash) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                // Never move a bundle through a link out of the root.
                remove_file_if_present(&trash)?;
                fs::create_dir(&trash)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&trash)?,
            Err(error) => return Err(error),
        }
        Ok(trash)
    }

    /// `<root>/<dir>/<version>`, its directory created.
    fn state_file(&self, dir: &str, version: &str) -> io::Result<PathBuf> {
        check_version(version)?;
        let dir = self.root.join(dir);
        if !fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
            remove_file_if_present(&dir)?;
            fs::create_dir_all(&dir)?;
        }
        Ok(dir.join(version))
    }
}

/// The version a trash entry `<version>.<pid>.<nanos>` was.
fn trashed_version(name: &str) -> &str {
    name.rsplitn(3, '.').nth(2).unwrap_or(name)
}

/// A version names one plain path component under the root, as the
/// launcher requires of a manifest's runtime.
fn check_version(version: &str) -> io::Result<()> {
    let plain = !version.is_empty()
        && version.len() <= 64
        && version != "."
        && version != ".."
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'));
    if plain {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{version:?} is not a runtime version"),
        ))
    }
}

/// Remove a directory tree, or a file or link, without following links.
fn remove_entry(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Replace `path` in one rename. The temporary name starts with a dot, so a
/// leftover one reads as an expired lease of no bundle and is dropped.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let nanos = tilo_clock::unix_now_ns() % 1_000_000_000;
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.{}.{nanos}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Bytes of the regular files under `path`, links not followed.
fn tree_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&next) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&next) {
                stack.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn options() -> Options {
        Options {
            grace_secs: DEFAULT_GRACE_SECS,
            now: NOW,
            dry_run: false,
        }
    }

    /// A bundle of `bytes` bytes at `version`, as an install leaves it.
    fn bundle(root: &BundleRoot, version: &str, bytes: usize) {
        let dir = root.bundle(version);
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(
            dir.join(crate::BUNDLE_MANIFEST_FILENAME),
            b"version = \"x\"\n",
        )
        .unwrap();
        fs::write(dir.join("lib/libruntime_dylib.so"), vec![7u8; bytes]).unwrap();
    }

    fn refs(names: &[&str]) -> impl Fn() -> io::Result<BTreeSet<String>> {
        let set: BTreeSet<String> = names.iter().map(|name| (*name).to_string()).collect();
        move || Ok(set.clone())
    }

    fn nothing_held() -> Holds {
        Box::new(|_: &Path, _: &str| false)
    }

    /// Whether a version lookup — the launcher's rule: `<root>/<v>/bundle.toml`
    /// exists — would select `version`.
    fn selectable(root: &BundleRoot, version: &str) -> bool {
        root.bundle(version)
            .join(crate::BUNDLE_MANIFEST_FILENAME)
            .exists()
    }

    #[test]
    fn unreferenced_versions_are_reclaimed_and_named_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let root = BundleRoot::new(dir.path());
        for version in [
            "2026.07+native.aa",
            "2026.07+native.bb",
            "2026.07+native.cc",
        ] {
            bundle(&root, version, 1000);
        }
        let collected = root
            .collect(&refs(&["2026.07+native.cc"]), &nothing_held, options())
            .unwrap();
        assert_eq!(
            collected.removed,
            vec![
                ("2026.07+native.aa".to_string(), 1014),
                ("2026.07+native.bb".to_string(), 1014)
            ]
        );
        assert_eq!(
            collected.kept,
            vec![("2026.07+native.cc".to_string(), Kept::Referenced)]
        );
        assert!(!root.bundle("2026.07+native.aa").exists());
        assert!(selectable(&root, "2026.07+native.cc"));
        assert_eq!(collected.freed_bytes(), 2028);
        assert!(
            fs::read_dir(dir.path().join(TRASH_DIR))
                .unwrap()
                .next()
                .is_none(),
            "the trash is empty afterwards"
        );
    }

    #[test]
    fn pinned_leased_and_mounted_bundles_survive() {
        let dir = tempfile::tempdir().unwrap();
        let root = BundleRoot::new(dir.path());
        for version in ["pinned", "fresh", "stale-lease", "mounted", "unused"] {
            bundle(&root, version, 10);
        }
        root.pin("pinned").unwrap();
        root.lease("fresh", NOW.saturating_sub(60)).unwrap();
        root.lease("stale-lease", NOW.saturating_sub(DEFAULT_GRACE_SECS))
            .unwrap();
        let mounted = root.bundle("mounted");
        let in_use = move || -> Holds {
            let mounted = mounted.clone();
            Box::new(move |path: &Path, _: &str| path == mounted)
        };
        let collected = root.collect(&refs(&[]), &in_use, options()).unwrap();
        let removed: Vec<&str> = collected.removed.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(removed, ["stale-lease", "unused"]);
        assert_eq!(
            collected.kept,
            vec![
                ("fresh".to_string(), Kept::Leased),
                ("mounted".to_string(), Kept::InUse),
                ("pinned".to_string(), Kept::Pinned),
            ]
        );
        assert!(
            root.leased_at("stale-lease").is_none(),
            "its lease went with it"
        );
        root.unpin("pinned").unwrap();
        let collected = root.collect(&refs(&[]), &nothing_held, options()).unwrap();
        let removed: Vec<&str> = collected.removed.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(removed, ["mounted", "pinned"]);
    }

    #[test]
    fn a_launch_that_mounts_during_the_collection_keeps_its_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let root = BundleRoot::new(dir.path());
        bundle(&root, "v1", 10);
        // The first look sees nothing; by the second, a sandbox has mounted
        // the bundle, and its mount followed the directory into the trash.
        let looked = std::cell::Cell::new(false);
        let in_use = || -> Holds {
            if looked.replace(true) {
                Box::new(|path: &Path, _: &str| path.to_string_lossy().contains("/.trash/v1."))
            } else {
                nothing_held()
            }
        };
        let collected = root.collect(&refs(&[]), &in_use, options()).unwrap();
        assert!(collected.removed.is_empty());
        assert_eq!(collected.kept, vec![("v1".to_string(), Kept::InUse)]);
        assert!(
            selectable(&root, "v1"),
            "it is back where the launcher finds it"
        );
    }

    #[test]
    fn a_crash_mid_collection_never_leaves_a_selectable_partial_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let root = BundleRoot::new(dir.path());
        bundle(&root, "v1", 4096);
        bundle(&root, "v2", 10);
        // The collection renamed v1 out and was killed while deleting it:
        // its manifest is still there, its toolkit already gone.
        let trashed = root.retire("v1").unwrap().unwrap();
        fs::remove_file(trashed.join("lib/libruntime_dylib.so")).unwrap();
        assert!(trashed.join(crate::BUNDLE_MANIFEST_FILENAME).exists());
        assert!(
            !selectable(&root, "v1"),
            "a lookup never finds the half-deleted bundle"
        );
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(crate::RUNTIME_DIR_ENV, dir.path());
        assert_eq!(crate::resolve_bundle_path("v1"), None);
        assert!(crate::resolve_bundle_path("v2").is_some());
        // The trash itself is not a version either.
        assert_eq!(crate::resolve_bundle_path(TRASH_DIR), None);
        drop(env);
        // The next collection finishes the job.
        let collected = root
            .collect(&refs(&["v2"]), &nothing_held, options())
            .unwrap();
        assert!(collected.removed.is_empty());
        assert!(collected.swept_bytes > 0);
        assert!(!trashed.exists());
        assert!(selectable(&root, "v2"));
    }

    #[test]
    fn a_held_lock_skips_the_collection() {
        let dir = tempfile::tempdir().unwrap();
        let root = BundleRoot::new(dir.path());
        bundle(&root, "v1", 10);
        let install = root.install_lock().unwrap();
        let collected = root.collect(&refs(&[]), &nothing_held, options()).unwrap();
        assert!(collected.busy);
        assert!(selectable(&root, "v1"));
        drop(install);
        assert!(
            !root
                .collect(&refs(&[]), &nothing_held, options())
                .unwrap()
                .busy
        );
        assert!(!root.bundle("v1").exists());
    }

    #[test]
    fn abandoned_staging_is_swept_and_a_dry_run_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = BundleRoot::new(dir.path());
        bundle(&root, "v1", 100);
        fs::create_dir_all(dir.path().join(".install-4242/tree")).unwrap();
        fs::write(dir.path().join(".install-4242/tree/blob"), vec![1u8; 50]).unwrap();
        let dry = Options {
            dry_run: true,
            ..options()
        };
        let collected = root.collect(&refs(&[]), &nothing_held, dry).unwrap();
        assert_eq!(collected.removed, vec![("v1".to_string(), 114)]);
        assert_eq!(collected.swept_bytes, 50);
        assert!(selectable(&root, "v1"));
        assert!(dir.path().join(".install-4242").exists());
        let collected = root.collect(&refs(&[]), &nothing_held, options()).unwrap();
        assert_eq!(collected.freed_bytes(), 164);
        assert!(!dir.path().join(".install-4242").exists());
    }

    #[cfg(unix)]
    #[test]
    fn nothing_outside_the_root_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"precious").unwrap();
        let root = BundleRoot::new(dir.path().join("root"));
        bundle(&root, "v1", 10);
        // A version name and the trash that are links out of the root.
        std::os::unix::fs::symlink(outside.path(), root.bundle("v-link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(TRASH_DIR)).unwrap();
        let collected = root.collect(&refs(&[]), &nothing_held, options()).unwrap();
        assert_eq!(collected.removed, vec![("v1".to_string(), 24)]);
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"precious");
        assert!(root.retire("../escape").is_err());
        assert!(root.purge(outside.path(), &nothing_held()).is_err());
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"precious");
    }

    #[test]
    fn retiring_and_purging_a_named_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let root = BundleRoot::new(dir.path());
        bundle(&root, "2026.07+native.ab", 10);
        let trashed = root.retire("2026.07+native.ab").unwrap().unwrap();
        assert_eq!(
            trashed_version(trashed.file_name().unwrap().to_str().unwrap()),
            "2026.07+native.ab"
        );
        assert!(
            !root
                .purge(&trashed, &(Box::new(|_: &Path, _: &str| true) as Holds))
                .unwrap()
        );
        assert!(trashed.exists(), "a held bundle stays in the trash");
        assert!(root.purge(&trashed, &nothing_held()).unwrap());
        assert!(!trashed.exists());
        assert_eq!(root.retire("2026.07+native.ab").unwrap(), None);
    }

    /// N runtime updates, each moving the app to a new build: the bytes the
    /// bundle root holds with and without collection.
    #[test]
    fn disk_after_runtime_updates() {
        // A runtime bundle is ~205 MB on x86_64; a 1:1000 scale keeps the
        // test fast and the ratio exact.
        const UPDATES: usize = 10;
        const BUNDLE_BYTES: usize = 205_000;
        let kept = tempfile::tempdir().unwrap();
        let collected = tempfile::tempdir().unwrap();
        let (kept, collected) = (
            BundleRoot::new(kept.path()),
            BundleRoot::new(collected.path()),
        );
        for update in 0..UPDATES {
            // Each update installs the build the updated app names; the
            // collection after it runs once the install's grace has passed.
            let version = format!("2026.07+native.{update:02}");
            for root in [&kept, &collected] {
                bundle(root, &version, BUNDLE_BYTES);
                root.lease(&version, NOW).unwrap();
            }
            let later = Options {
                now: NOW.saturating_add(DEFAULT_GRACE_SECS),
                ..options()
            };
            collected
                .collect(&refs(&[&version]), &nothing_held, later)
                .unwrap();
        }
        let without = tree_size(kept.path());
        let with = tree_size(collected.path());
        println!(
            "RUNTIME-GC updates={UPDATES} bundle_bytes={BUNDLE_BYTES} without_gc={without} \
             with_gc={with}"
        );
        assert_eq!(kept.bundles().unwrap().len(), UPDATES);
        assert_eq!(
            collected.bundles().unwrap().len(),
            1,
            "only the build in use"
        );
        assert!(with.saturating_mul(9) < without, "{with} of {without}");
    }
}
