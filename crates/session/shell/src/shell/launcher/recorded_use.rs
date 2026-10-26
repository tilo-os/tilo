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

/// Record, on a worker, that the person opened `app`. The worker, when one
/// was started: what must not run before the use is written joins it.
pub(super) fn record(app: &InstalledApp) -> Option<std::thread::JoinHandle<()>> {
    // The store the launch resolved from, not whichever one the worker
    // would find later.
    let store = PackageStore::from_env().ok()?;
    if !app.payload_root.starts_with(store.layout().root()) || !due(&app.package_id, Instant::now())
    {
        return None;
    }
    let (package_id, payload_root) = (app.package_id.clone(), app.payload_root.clone());
    let spawned = std::thread::Builder::new()
        .name("tilo-app-used".into())
        .spawn(move || {
            record_in(&store, &package_id, &payload_root);
        });
    spawned
        .inspect_err(|error| tracing::debug!(%error, "could not record an app's use"))
        .ok()
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

    /// An app behind this system is updated before it opens, and the size
    /// sweep that follows the update evicts the app opened least recently.
    /// The update keeps the app's last use, so the open must be recorded
    /// before the update runs, or the app being opened is the one evicted.
    #[test]
    #[serial_test::serial(process_env)]
    fn opening_an_app_that_must_update_first_records_the_use_before_the_update() {
        use std::os::unix::fs::PermissionsExt as _;
        const BEHIND: &str = "so.tilo.shell-recorded-use-behind";
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let store_root = root.join("store");
        let (payload_root, manifest_path) = crate::shell::test_support::install_signed_package_with(
            &store_root,
            BEHIND,
            "0.1.0",
            &[("probe", b"behind")],
            "[sandbox.services]\ncollab = \"optional@1\"\n",
        );
        let store = PackageStore::at(&store_root);
        store.update_row(BEHIND, |row| row.last_used = 10).unwrap();
        let interfaces = root.join("system-interfaces.toml");
        std::fs::write(&interfaces, "[interfaces]\ncollab = 2\n").unwrap();
        // The update keeps the registry as it found it, then fails with
        // the app gone, so what follows the update never starts it.
        let seen = root.join("registry-the-update-saw.toml");
        let update = root.join("app");
        let registry = store.layout().registry();
        std::fs::write(
            &update,
            format!(
                "#!/bin/sh\ncp '{0}' '{1}'\nrm '{0}'\nexit 1\n",
                registry.display(),
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&update, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", &store_root)
            .set("TILO_SYSTEM_INTERFACES", &interfaces)
            .set("TILO_APP_BIN", &update);
        let app = InstalledApp {
            package_id: BEHIND.into(),
            display_name: BEHIND.into(),
            localized_names: std::collections::BTreeMap::default(),
            keywords: Vec::new(),
            category: None,
            version: "0.1.0".into(),
            manifest_path,
            payload_root,
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        };
        assert!(matches!(
            crate::shell::app_versions::fit(&app.manifest_path),
            crate::shell::app_versions::Fit::AppBehind(_)
        ));

        super::super::execute_installed(
            app,
            crate::shell::state::RoomId::new(),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        tilo_test_env::wait_until("the update to run", || {
            !crate::shell::app_versions::updating(BEHIND) && seen.exists()
        });
        let registry = package_store::Registry::load(&seen).unwrap();
        let last_used = registry.find(BEHIND).unwrap().last_used;
        env.remove("TILO_APPS_DATA_DIR");
        super::super::invalidate_app_cache();
        assert!(
            last_used > 10,
            "the open was recorded before the update ran: {last_used}"
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
