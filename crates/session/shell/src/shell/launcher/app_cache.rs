//! The installed-app list a launch reads, memoised: the registry's signed
//! manifests, refreshed off the compositor loop, expired by a watch of the
//! registries it was read from, and never older than an install that
//! completed.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{InstalledApp, LaunchError, lock, read_installed_apps};

/// Fallback only: the registry is watched with inotify, so a new install
/// shows up on the next `installed_apps()` without waiting. This bound
/// catches a missed watch (NFS, the watch limit, a swapped `TILO_APPS_DATA_DIR`).
pub(super) const APP_CACHE_TTL: Duration = Duration::from_secs(30);

/// Last enumeration and when it was memoised; `None` marks it stale, so it
/// is only the baseline the next read compares against.
pub(super) static APP_CACHE: Mutex<Option<(Vec<InstalledApp>, Option<Instant>)>> = Mutex::new(None);
pub(super) static APP_CACHE_REVISION: AtomicU64 = AtomicU64::new(0);
/// Bumped when a watch (or an explicit invalidate) makes the memoised
/// snapshot untrustworthy. Distinct from [`APP_CACHE_REVISION`]: an
/// invalidation alone must not look like a new generation of apps.
pub(super) static APP_CACHE_EPOCH: AtomicU64 = AtomicU64::new(0);
/// The registry roots watched, and a watch of each that could be armed.
pub(super) static APP_CACHE_WATCH: Mutex<
    Option<(Vec<PathBuf>, Vec<tilo_files::native::ChangeWatch>)>,
> = Mutex::new(None);
/// The registries the memoised snapshot was read from, as they were just before the read.
pub(super) static APP_CACHE_STAMP: Mutex<Option<Vec<Option<RegistryStamp>>>> = Mutex::new(None);

/// Which version of a registry file is on disk. An install replaces the registry by one
/// rename, its commit point, so a completed install always shows as another file here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct RegistryStamp {
    device: u64,
    inode: u64,
    modified: Option<std::time::SystemTime>,
    len: u64,
}

pub(super) fn registry_stamps() -> Vec<Option<RegistryStamp>> {
    use std::os::unix::fs::MetadataExt as _;
    app_cache_roots()
        .iter()
        .map(|root| {
            let metadata =
                std::fs::metadata(root.join(package_store::layout::REGISTRY_FILE)).ok()?;
            Some(RegistryStamp {
                device: metadata.dev(),
                inode: metadata.ino(),
                modified: metadata.modified().ok(),
                len: metadata.len(),
            })
        })
        .collect()
}

/// Pair records with their cache revision while holding the cache lock.
///
/// A memoised snapshot answers only while the registries are the ones it was read from: the
/// watch that expires it runs on its own thread, and an app launched right after an install
/// completed must start the installed package, not the one before it.
pub(super) fn installed_apps_snapshot() -> Result<(u64, Vec<InstalledApp>), LaunchError> {
    let stamps = registry_stamps();
    {
        let cache = lock(&APP_CACHE);
        if let Some((apps, Some(at))) = cache.as_ref()
            && at.elapsed() < APP_CACHE_TTL
            && lock(&APP_CACHE_STAMP).as_ref() == Some(&stamps)
        {
            return Ok((APP_CACHE_REVISION.load(Ordering::Acquire), apps.clone()));
        }
    }
    let epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
    let apps = read_installed_apps()?;
    *lock(&APP_CACHE_STAMP) = Some(stamps);
    let generation = record_app_read(epoch, &apps);
    ensure_app_cache_watch();
    Ok((generation, apps))
}

/// [`installed_apps`](super::installed_apps) for the compositor loop: the last enumeration, fresh
/// or stale, and never a registry read, which scans the registry and parses
/// every manifest. One past its TTL or invalidated by the registry watch is
/// refreshed on a worker and served until the refresh lands; before the
/// first enumeration there is none.
pub(crate) fn installed_apps_without_waiting() -> Vec<InstalledApp> {
    let (apps, fresh) = {
        let cache = lock(&APP_CACHE);
        cache.as_ref().map_or_else(
            || (Vec::new(), false),
            |(apps, at)| {
                (
                    apps.clone(),
                    at.is_some_and(|at| at.elapsed() < APP_CACHE_TTL),
                )
            },
        )
    };
    if !fresh {
        refresh_installed_apps_in_background();
    }
    apps
}

/// Re-read the registry on a worker, one refresh at a time.
pub(super) fn refresh_installed_apps_in_background() {
    static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tilo-apps-refresh".into())
        .spawn(|| {
            if let Err(error) = installed_apps_snapshot() {
                tracing::debug!(%error, "installed apps refresh failed; the last list stays");
            }
            REFRESHING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        REFRESHING.store(false, Ordering::Release);
    }
}

/// Store a registry read that started at `read_epoch` as the new baseline.
pub(super) fn record_app_read(read_epoch: u64, apps: &[InstalledApp]) -> u64 {
    let mut cache = lock(&APP_CACHE);
    // An invalidation (install, removal, watch event, repointed registry)
    // that raced this read means the read may predate it: keep it only as
    // a stale baseline so the next call re-reads instead of hiding the
    // change for a whole TTL.
    let fresh = (APP_CACHE_EPOCH.load(Ordering::Acquire) == read_epoch).then(Instant::now);
    let changed = cache.as_ref().is_none_or(|(cached, _)| cached != apps);
    // The TTL controls disk freshness, not search identity. Advancing this
    // generation for identical records invalidates both native and host app
    // indexes, making a just-discovered action disappear before invocation.
    if changed {
        APP_CACHE_REVISION.fetch_add(1, Ordering::Relaxed);
    }
    *cache = Some((apps.to_vec(), fresh));
    let generation = if fresh.is_some() {
        APP_CACHE_REVISION.load(Ordering::Acquire)
    } else {
        0
    };
    drop(cache);
    generation
}

/// Drop the memoised enumeration and read the registry again, so what the
/// compositor loop is served ([`installed_apps_without_waiting`]) already
/// holds the change. Used on a worker after an install/remove and by tests
/// that repoint `TILO_APPS_DATA_DIR`; never on the compositor loop.
pub fn invalidate_app_cache() {
    // Bump the epoch under the lock so a read finishing right after cannot
    // memoise itself as fresh.
    let mut cache = lock(&APP_CACHE);
    APP_CACHE_EPOCH.fetch_add(1, Ordering::Release);
    APP_CACHE_REVISION.fetch_add(1, Ordering::Relaxed);
    *cache = None;
    drop(cache);
    *lock(&APP_CACHE_STAMP) = None;
    load_installed_apps();
}

/// Read the installed apps into what the compositor loop is served. At
/// startup, before the loop turns, so its first windows are named by their
/// apps; never on the loop.
pub(crate) fn load_installed_apps() {
    if let Err(error) = installed_apps_snapshot() {
        tracing::warn!(%error, "installed apps unreadable");
    }
}

/// Cheap installed-store invalidation token for session-owned services.
/// A changed token means callers must reread the canonical cached catalogue;
/// it is not an authorization or a package identity.
pub(crate) fn installed_apps_epoch() -> u64 {
    APP_CACHE_EPOCH.load(Ordering::Acquire)
}

pub(super) fn expire_watched_app_cache() {
    let mut cache = lock(&APP_CACHE);
    APP_CACHE_EPOCH.fetch_add(1, Ordering::Release);
    if let Some((_, at)) = cache.as_mut() {
        *at = None;
    }
}

/// The watch of the registries `roots` saw a change: expire the memoised
/// enumeration and read it again, so the compositor loop is served the
/// change without a read of its own. Only while the cache is still watched
/// for `roots`: a watch it has moved on from (the registry was repointed,
/// or the watch replaced or forgotten) still delivers what was queued for
/// it, on its own thread and at its own time, and that is news about
/// registries the cache no longer reads. Checked under the watch's lock, so
/// once a watch is replaced none of its reports expires or records anything.
pub(super) fn registry_changed(roots: &[PathBuf]) {
    let watched = lock(&APP_CACHE_WATCH);
    if watched.as_ref().is_none_or(|(held, _)| held != roots) {
        return;
    }
    expire_watched_app_cache();
    drop(watched);
    // A read that starts after this epoch was taken reads a registry at least
    // as new as the change just reported.
    let epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
    let stamps = registry_stamps();
    let apps = match read_installed_apps() {
        Ok(apps) => apps,
        Err(error) => {
            tracing::debug!(%error, "changed app registry unreadable; the last list stays stale");
            return;
        }
    };
    let watched = lock(&APP_CACHE_WATCH);
    if watched.as_ref().is_none_or(|(held, _)| held != roots) {
        return;
    }
    *lock(&APP_CACHE_STAMP) = Some(stamps);
    record_app_read(epoch, &apps);
    drop(watched);
}

/// Forget the memoised enumeration and the registry watch without reading
/// again: a test that reads its fixture registry itself, with no watch over
/// it to report its own writes at a time of the watch's choosing.
#[cfg(test)]
pub(super) fn forget_app_cache() {
    *lock(&APP_CACHE_WATCH) = None;
    let mut cache = lock(&APP_CACHE);
    APP_CACHE_EPOCH.fetch_add(1, Ordering::Release);
    *cache = None;
    drop(cache);
    *lock(&APP_CACHE_STAMP) = None;
}

pub(super) fn app_cache_roots() -> Vec<PathBuf> {
    [
        package_store::layout::default_user_root(),
        package_store::layout::system_data_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Watch the registry roots, package folders included, once per set of roots. A change is
/// read on the watch's thread, so the list the compositor loop is served holds it without a
/// read of its own. Opens and reads are not changes, so filling the cache does not expire it.
pub(super) fn ensure_app_cache_watch() {
    let roots = app_cache_roots();
    if roots.is_empty() {
        return;
    }
    let mut watched = lock(&APP_CACHE_WATCH);
    if watched.as_ref().is_some_and(|(held, _)| *held == roots) {
        return;
    }
    let armed = roots
        .iter()
        .filter_map(|root| {
            let watching = roots.clone();
            tilo_files::native::watch_tree_changes(
                root,
                |_| true,
                move || registry_changed(&watching),
            )
            .map_err(|error| {
                tracing::info!(%error, path = %root.display(), "app registry path is not watched");
            })
            .ok()
        })
        .collect();
    *watched = Some((roots, armed));
}
