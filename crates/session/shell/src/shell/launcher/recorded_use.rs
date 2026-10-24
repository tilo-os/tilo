//! Opening a downloaded app records the use in the app store, so the size
//! sweep that evicts unused copies evicts the app the person opened least
//! recently, not the one downloaded first.
//!
//! Only apps in the person's own store are recorded; the image's apps are
//! never evicted. The write runs on a worker, and an app is recorded at most
//! once an interval per session: eviction orders apps by days, and every
//! registry write makes the next app listing re-read every manifest.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use package_store::PackageStore;

use super::InstalledApp;

/// How long one recorded use stands for every later open of the same app.
const INTERVAL: Duration = Duration::from_secs(60 * 60);

/// When each package's use was last recorded this session.
static RECORDED: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record, on a worker, that the person opened `app`.
pub(super) fn record(app: &InstalledApp) {
    // The store the launch resolved from, not whichever one the worker
    // would find later.
    let Ok(store) = PackageStore::from_env() else {
        return;
    };
    if !app.payload_root.starts_with(store.layout().root()) || !due(&app.package_id, Instant::now())
    {
        return;
    }
    let (package_id, payload_root) = (app.package_id.clone(), app.payload_root.clone());
    let spawned = std::thread::Builder::new()
        .name("tilo-app-used".into())
        .spawn(move || record_in(&store, &package_id, &payload_root));
    if let Err(error) = spawned {
        tracing::debug!(%error, "could not record an app's use");
    }
}

/// Whether `package_id`'s use is due to be recorded at `now`, noting it as
/// recorded when it is.
fn due(package_id: &str, now: Instant) -> bool {
    let mut recorded = super::lock(&RECORDED);
    if recorded
        .get(package_id)
        .is_some_and(|at| now.saturating_duration_since(*at) < INTERVAL)
    {
        return false;
    }
    recorded.insert(package_id.to_owned(), now);
    true
}

/// Record the use of `package_id` in `store` when its payload is the
/// store's own. Whether it was recorded.
fn record_in(store: &PackageStore, package_id: &str, payload_root: &std::path::Path) -> bool {
    if !payload_root.starts_with(store.layout().root()) {
        return false;
    }
    match store.mark_used(package_id) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(package_id, %error, "could not record an app's use");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The person downloaded the daily app first and opens it every day; an
    /// app tried once came later. The sweep must evict the one tried once.
    #[test]
    fn opening_a_downloaded_app_records_its_use_for_eviction() {
        const DAILY: &str = "so.tilo.shell-recorded-use-daily";
        const TRIED: &str = "so.tilo.shell-recorded-use-tried";
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let (daily_payload, _) = crate::shell::test_support::install_signed_package(
            &root,
            DAILY,
            "0.1.0",
            &[("probe", b"daily")],
        );
        crate::shell::test_support::install_signed_package(
            &root,
            TRIED,
            "0.1.0",
            &[("probe", b"tried")],
        );
        let store = PackageStore::at(&root);
        store.update_row(DAILY, |row| row.last_used = 10).unwrap();
        store.update_row(TRIED, |row| row.last_used = 20).unwrap();

        assert!(record_in(&store, DAILY, &daily_payload));

        let registry = store.registry().unwrap();
        let daily = registry.find(DAILY).unwrap().last_used;
        let tried = registry.find(TRIED).unwrap().last_used;
        assert!(
            daily > tried,
            "opening it made it the most recently used: {daily} after {tried}"
        );
    }

    #[test]
    fn an_app_from_the_image_is_not_recorded_in_the_persons_store() {
        const SYSTEM: &str = "so.tilo.shell-recorded-use-system";
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let store = PackageStore::at(root.join("user"));
        let image = root.join("image/payloads").join(SYSTEM);

        assert!(!record_in(&store, SYSTEM, &image));
        assert!(
            !store.layout().registry().exists(),
            "nothing was written to the person's store"
        );
    }

    #[test]
    fn an_app_opened_again_soon_is_recorded_once() {
        const APP: &str = "so.tilo.shell-recorded-use-again";
        let start = Instant::now();
        assert!(due(APP, start));
        assert!(!due(APP, start + Duration::from_secs(60)));
        assert!(due(APP, start + INTERVAL));
    }
}
