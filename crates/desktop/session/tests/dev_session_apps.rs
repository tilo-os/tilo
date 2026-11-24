//! A development session's tilolang apps are installed packages.
//! `tilolang run` builds each app of `apps/tl` into a signed `.tapp` and installs it into
//! the session's own app store; the session then reads each app from its store row, the way it
//! reads a device's installed apps, and the manifest it reads there is what the launcher sees.
//! Nothing here loads a package into the session from a test's own copy: what runs is what the
//! store holds, which is what a person's device would hold.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

/// A development session's app store, as `tilolang run` leaves it: every app of `apps/tl` built
/// into a signed `.tapp` and installed, in a home of its own, with the store named for the
/// session the way the environment names it.
///
/// Building the apps is what costs here (about five seconds of this unoptimised binary), and
/// everything below only reads what the install left: so the store is installed once, and the
/// three readings of it are the three parts of one test instead of three installs.
struct DevSession {
    /// The home the store is in, with the environment naming it, kept for the test's length.
    _home: tilo_test_env::TestHome,
    /// The session's app store.
    store: std::path::PathBuf,
    /// The apps the install answered, by package id.
    installed: Vec<String>,
}

impl DevSession {
    fn installed() -> Self {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let store = home.data().join("tilo-apps");
        let runtime = home.state().join("tilolang");
        std::fs::create_dir_all(&runtime).expect("a directory for the build");
        let installed = crate::common::dev_apps::install(&store, &runtime);
        home.env().set("TILO_APPS_DATA_DIR", &store);
        Self {
            _home: home,
            store,
            installed,
        }
    }
}

/// What a development session's install leaves, read three ways from the one store it installed:
/// every part reads the store and none writes it, so a later part sees exactly what the first
/// one did.
#[test]
fn a_dev_sessions_apps_are_installed_packages_it_seeds_from() {
    let session = DevSession::installed();
    every_dev_session_app_is_an_installed_package(&session);
    a_dev_session_installs_the_apps_and_not_the_parts_they_are_made_with(&session);
    the_installed_apps_scenarios_fill_the_sessions_records(&session);
}

/// Every app of the development session is a package the session's store holds, built from
/// `apps/tl` and installed with its signed manifest.
fn every_dev_session_app_is_an_installed_package(session: &DevSession) {
    // What is read below is what the install answered: the session's apps and its shell, so the
    // readings are of six packages and never of none.
    for app in tilo_session::seeded_apps::APPS
        .into_iter()
        .chain(["so.tilo.shell"])
    {
        assert!(
            session.installed.iter().any(|installed| installed == app),
            "{app} is an app the install answered: {:?}",
            session.installed
        );
    }
    assert_eq!(
        session.installed.len(),
        6,
        "the install answered the session's apps and nothing besides: {:?}",
        session.installed
    );
    for app in &session.installed {
        let app = app.as_str();
        let found = app_catalogue::apps::read_one_from_env(app)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("{app} is installed in the session's store"));
        assert_eq!(
            found.app_id.as_str(),
            app,
            "the store's row is the app's own id"
        );
        assert!(
            found
                .payload_root
                .join(tilolang_tilo::bundle::WASM_FILE)
                .is_file(),
            "{app} is installed as a tilolang package: {}",
            found.payload_root.display()
        );
        assert!(
            found.manifest_path.is_file(),
            "{app} has the manifest its install signed"
        );
    }
}

/// A development session installs the apps a person opens, and not the parts they are made
/// with: Discussion is a package Tasks and Notes are built with, so it rides in their payloads
/// and the comments on a task or a note are kept in that app's own record, as a package an app
/// uses that is not installed keeps its records in it. Installed as an app of its own it would
/// be a second record the threads are read from, and an app a person could open.
fn a_dev_session_installs_the_apps_and_not_the_parts_they_are_made_with(session: &DevSession) {
    let installed = &session.installed;
    let catalog = package_store::Catalog::at(&session.store, None);
    let apps: Vec<String> = app_catalogue::apps::read(&catalog)
        .expect("the store's apps read")
        .into_iter()
        .map(|app| app.app_id.to_string())
        .collect();
    for app in installed {
        assert!(apps.contains(app), "{app} is installed: {apps:?}");
    }
    assert!(
        !apps.contains(&"so.tilo.lang.discussion".to_owned()),
        "Discussion is no app of the session's: {apps:?}"
    );
    // The part itself rides in the apps made with it.
    for app in ["so.tilo.tasks", "so.tilo.notes"] {
        let found = app_catalogue::apps::read_one_from_env(app)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("{app} is installed"));
        assert!(
            found
                .payload_root
                .join(tilolang_tilo::bundle::IMPORTS_DIR)
                .join("so.tilo.lang.discussion.wasm")
                .is_file(),
            "{app} carries the package it is made with: {}",
            found.payload_root.display()
        );
    }
}

/// Every kind the resources `resources` publish, at any depth.
fn kinds_of(
    resources: &[tilo_actions::ResourceSpec],
    kinds: &mut std::collections::BTreeSet<String>,
) {
    for resource in resources {
        kinds.insert(resource.kind.clone());
        kinds_of(&resource.children, kinds);
    }
}

/// Each installed app's first scenario fills the session's records: the seeded `YouTube` client
/// holds the channels its subscriptions feed shows, and Notes its notebook, so what those apps draw
/// and what a search's answer lands beside are there. Nothing here seeds after the fact: the
/// scenario is the package's own, run once as a development session runs it.
fn the_installed_apps_scenarios_fill_the_sessions_records(session: &DevSession) {
    let apps = crate::common::seeded_apps_in(&session.store);
    let mut kinds = std::collections::BTreeSet::new();
    kinds_of(&crate::common::published(&apps), &mut kinds);
    for kind in [
        "tl.so.tilo.tube.Channel",
        "tl.so.tilo.tube.Video",
        "tl.so.tilo.notes.Note",
    ] {
        assert!(kinds.contains(kind), "{kind} is seeded: {kinds:?}");
    }
}
