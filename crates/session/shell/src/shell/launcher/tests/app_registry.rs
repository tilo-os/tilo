//! The installed-app list a launch reads: the registry's signed manifests,
//! memoised, refreshed off the compositor loop and when the registry changes,
//! and never older than an install that completed.

use super::*;

/// The compositor loop asks for installed apps while building tiles and
/// naming apps in Activity. It is served the last enumeration, stale or
/// not, and never reads the registry itself: a stale list is refreshed
/// on a worker and the refresh lands for a later ask.
#[test]
#[serial_test::serial(process_env)]
fn the_loop_is_served_the_last_app_list_while_a_worker_refreshes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", tmp.path());
    invalidate_app_cache();
    let initial = installed_apps().unwrap();
    assert_eq!(initial[0].version, "1.2.3");

    let manifest = payload.join("manifest.toml");
    let changed_manifest = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("1.2.3", "1.2.4");
    std::fs::write(&manifest, changed_manifest).unwrap();
    lock(&APP_CACHE).as_mut().unwrap().1 = None;

    let served = installed_apps_without_waiting();
    assert_eq!(
        served, initial,
        "the stale list is served; the registry was not read here"
    );
    let refreshed = tilo_test_env::wait_for("the refreshed app list", || {
        let served = installed_apps_without_waiting();
        served
            .first()
            .is_some_and(|app| app.version == "1.2.4")
            .then_some(served)
    });
    env.remove("TILO_APPS_DATA_DIR");
    invalidate_app_cache();
    assert_eq!(refreshed.len(), 1);
}

/// An install that completed is what the next launch starts, whether or not the registry
/// watch has expired the memoised list yet: the list answers only while the registry is the
/// file it was read from.
#[test]
#[serial_test::serial(process_env)]
fn a_launch_right_after_an_install_reads_the_installed_package() {
    let tmp = tempfile::tempdir().unwrap();
    install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", tmp.path());
    invalidate_app_cache();
    let before = registry_stamps();
    assert_eq!(installed_apps().unwrap().len(), 1);

    // The install's commit: a new registry, renamed into place.
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    install_fixture(&other, "dev.tilo.calculator", "gui");
    install_fixture(&other, "dev.tilo.notes", "gui");
    std::fs::rename(
        other.join("registry.toml"),
        tmp.path().join("registry.toml"),
    )
    .unwrap();
    assert_ne!(
        registry_stamps(),
        before,
        "the replaced registry is another file"
    );
    let after = installed_apps().unwrap();
    env.remove("TILO_APPS_DATA_DIR");
    invalidate_app_cache();

    assert_eq!(
        after.len(),
        2,
        "the memoised list did not answer for the new registry"
    );
}

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn app_registry_enumeration_reads_signed_manifests_and_keeps_generations() {
    fn unchanged_app_cache_refresh_preserves_search_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        let (snapshot_generation, initial_gui) = gui_apps_snapshot();
        let initial_generation = gui_apps_generation();
        assert_eq!(
            snapshot_generation, initial_generation,
            "populating a cold cache must not invalidate its own snapshot"
        );
        assert_eq!(initial_gui.len(), 1);
        let initial = installed_apps().unwrap();
        let expire = || {
            lock(&APP_CACHE).as_mut().unwrap().1 = None;
        };
        expire();
        let unchanged = installed_apps().unwrap();
        let unchanged_generation = gui_apps_generation();

        let manifest = payload.join("manifest.toml");
        let changed_manifest = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("1.2.3", "1.2.4");
        std::fs::write(&manifest, changed_manifest).unwrap();
        expire();
        let changed = installed_apps().unwrap();
        let changed_generation = gui_apps_generation();

        std::fs::write(tmp.path().join("registry.toml"), "").unwrap();
        expire();
        let removed = installed_apps().unwrap();
        let removed_generation = gui_apps_generation();
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();

        assert_eq!(unchanged, initial);
        assert_eq!(
            unchanged_generation, initial_generation,
            "an unchanged TTL refresh must not invalidate retained search results"
        );
        assert_eq!(changed[0].version, "1.2.4");
        assert_ne!(changed_generation, unchanged_generation);
        assert!(removed.is_empty());
        assert_ne!(removed_generation, changed_generation);
    }

    fn raced_app_cache_refresh_keeps_its_read_as_the_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        // No watch over the fixture: this scenario writes its manifest, and a
        // watch would report that write on its own thread whenever it likes,
        // contesting the re-read below, which this scenario says is not.
        // (`invalidate_app_cache` reads the registry, and that arms one.) A
        // watch an earlier scenario left is forgotten too: its reports no
        // longer touch the cache (`registry_changed`).
        forget_app_cache();
        // Seed and re-read through `record_app_read` only: `installed_apps`
        // arms the real watch.
        record_app_read(
            APP_CACHE_EPOCH.load(Ordering::Acquire),
            &read_installed_apps().unwrap(),
        );
        let manifest = payload.join("manifest.toml");
        let changed_manifest = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("1.2.3", "1.2.4");
        std::fs::write(&manifest, changed_manifest).unwrap();
        let before_race = gui_apps_generation();
        // A watch event lands while this refresh is reading the registry.
        let read_epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
        let raced = read_installed_apps().unwrap();
        expire_watched_app_cache();
        let raced_snapshot_generation = record_app_read(read_epoch, &raced);
        assert_eq!(raced_snapshot_generation, 0, "a raced read stays stale");
        let raced_generation = gui_apps_generation();
        let stale = lock(&APP_CACHE).as_ref().unwrap().1.is_none();
        let reread_epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
        let reread = read_installed_apps().unwrap();
        record_app_read(reread_epoch, &reread);
        let reread_generation = gui_apps_generation();
        let fresh = lock(&APP_CACHE).as_ref().unwrap().1.is_some();
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();

        assert_eq!(raced[0].version, "1.2.4");
        assert_ne!(raced_generation, before_race, "the raced read saw a change");
        assert!(stale, "a raced read must not be memoised as fresh");
        assert_eq!(reread, raced);
        assert_eq!(
            reread_generation, raced_generation,
            "an identical re-read after a raced refresh keeps the generation"
        );
        assert!(fresh, "the uncontested re-read is memoised");
    }

    fn enumeration_reads_the_registry_and_flags_gui_apps() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "so.tilo.hello", "gui");
        install_fixture(tmp.path(), "so.tilo.notes-cli", "cli");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        invalidate_app_cache();
        let apps = read_installed_apps().expect("registry readable");
        let offered = gui_apps(); // what the palette actually offers
        env.remove("TILO_APPS_DATA_DIR");
        invalidate_app_cache();

        assert_eq!(apps.len(), 2, "both packages enumerate: {apps:?}");
        let hello = apps
            .iter()
            .find(|a| a.package_id == "so.tilo.hello")
            .expect("hello present");
        assert!(hello.has_gui, "gui entrypoint must be flagged as GUI");
        assert_eq!(hello.display_name, "Hello");
        assert_eq!(hello.version, "1.2.3");
        let cli = apps
            .iter()
            .find(|a| a.package_id == "so.tilo.notes-cli")
            .expect("cli package present");
        assert!(!cli.has_gui, "a cli-only package is not a GUI app");

        // Enumerating it is fine; offering it to launch is not — a
        // headless package has no window to show.
        let offered_ids: Vec<&str> = offered.iter().map(|a| a.package_id.as_str()).collect();
        assert_eq!(
            offered_ids,
            vec!["so.tilo.hello"],
            "only the GUI package is launchable from the palette",
        );
    }

    fn installed_continuity_lookup_reads_signed_manifest_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = install_fixture(tmp.path(), "so.tilo.browser", "gui");
        let manifest_path = payload.join("manifest.toml");
        let mut manifest = std::fs::OpenOptions::new()
            .append(true)
            .open(&manifest_path)
            .unwrap();
        writeln!(
            manifest,
            "\n[continuity]\nrequired = [\"presentation\", \"semantic\"]\nsemantic-schema = \"web-location-v1\""
        )
        .unwrap();

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        let required = installed_continuity_contract("so.tilo.browser");
        let legacy = installed_continuity_contract("so.tilo.not-installed");
        env.remove("TILO_APPS_DATA_DIR");

        assert_eq!(
            required,
            InstalledContinuityRequirements::Required(launcher::manifest::ContinuityContract {
                required: vec![
                    launcher::manifest::ContinuityFacet::Presentation,
                    launcher::manifest::ContinuityFacet::Semantic,
                ],
                semantic_schema: Some(launcher::manifest::ContinuitySemanticSchema::WebLocationV1,),
            })
        );
        assert_eq!(legacy, InstalledContinuityRequirements::NotInstalled);
    }

    fn display_name_title_cases_the_trailing_component() {
        assert_eq!(display_name_for_package_id("so.tilo.hello"), "Hello");
        assert_eq!(
            display_name_for_package_id("so.tilo.text-editor"),
            "Text Editor"
        );
        assert_eq!(display_name_for_package_id("Notes"), "Notes");
        // A degenerate id must not produce an empty (unclickable) label.
        assert!(!display_name_for_package_id("so.tilo.").is_empty());
    }

    fn managed_entry_registry_generation_changes_only_with_its_snapshot() {
        let first = set_windows_compat_entry_ids(vec!["windows-one".into()]);
        let unchanged = set_windows_compat_entry_ids(vec!["windows-one".into()]);
        let second = set_windows_compat_entry_ids(vec!["windows-two".into()]);
        assert_eq!(unchanged, first);
        assert!(second > first);
    }

    raced_app_cache_refresh_keeps_its_read_as_the_baseline();
    unchanged_app_cache_refresh_preserves_search_generation();
    enumeration_reads_the_registry_and_flags_gui_apps();
    installed_continuity_lookup_reads_signed_manifest_contract();
    display_name_title_cases_the_trailing_component();
    managed_entry_registry_generation_changes_only_with_its_snapshot();
}

#[test]
#[serial_test::serial(process_env)]
fn app_registry_watch_invalidates_without_waiting_for_ttl() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", tmp.path());
    invalidate_app_cache();
    let initial = installed_apps().unwrap();
    assert_eq!(initial[0].version, "1.2.3");
    let manifest = payload.join("manifest.toml");
    let changed = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("1.2.3", "1.2.4");
    std::fs::write(&manifest, changed).unwrap();
    tilo_test_env::wait_until("inotify invalidates the app cache", || {
        installed_apps().is_ok_and(|apps| apps.first().is_some_and(|app| app.version == "1.2.4"))
    });
    env.remove("TILO_APPS_DATA_DIR");
    invalidate_app_cache();
}

/// A watch reports a change on its own thread, at its own time, and keeps
/// delivering what was queued for it after the cache moved on. A report from
/// a watch of other registries than the ones the cache is watched for (the
/// registry repointed, the watch replaced or forgotten) expires nothing; one
/// from the cache's own watch does.
#[test]
#[serial_test::serial(process_env)]
fn only_the_cache_s_own_watch_expires_it() {
    let tmp = tempfile::tempdir().unwrap();
    install_fixture(tmp.path(), "dev.tilo.calculator", "gui");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", tmp.path());
    invalidate_app_cache();
    let fresh = || {
        lock(&APP_CACHE)
            .as_ref()
            .is_some_and(|(_, at)| at.is_some())
    };
    let held = lock(&APP_CACHE_WATCH)
        .as_ref()
        .map(|(roots, _)| roots.clone())
        .expect("reading the registry watches it");
    assert!(fresh(), "the read is memoised");

    let elsewhere = tempfile::tempdir().unwrap();
    registry_changed(&[elsewhere.path().to_path_buf()]);
    assert!(fresh(), "a watch the cache moved on from expires nothing");

    let epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
    registry_changed(&held);
    assert_ne!(
        APP_CACHE_EPOCH.load(Ordering::Acquire),
        epoch,
        "the cache's own watch expires it"
    );

    forget_app_cache();
    registry_changed(&held);
    assert!(
        lock(&APP_CACHE).is_none(),
        "nor does a forgotten one record a read"
    );
    env.remove("TILO_APPS_DATA_DIR");
    invalidate_app_cache();
}
