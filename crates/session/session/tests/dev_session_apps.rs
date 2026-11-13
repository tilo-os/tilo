//! A development session's tilolang apps are installed packages, and the session plans their
//! connectors from what it installed.
//! `tilolang run` builds each app of `apps/tl` into a signed `.tapp` and installs it into
//! the session's own app store; the session then reads each app from its store row, the way it
//! reads a device's installed apps, and the manifest it reads there is what the launcher and the
//! connector planning see (`tilo_session::apps::Apps::plan_connector`). So the `YouTube`
//! client's connector is planned from its installed manifest, and Mail's from its own: their
//! origins are the apps' declarations, never something a test or a seed path supplies.
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

use tilo_app_launch::SessionEnvironment;
use tilo_service::Service as _;
use tilo_session::apps::Apps;

/// Every app of the development session is a package the session's store holds, built from
/// `apps/tl` and installed with its signed manifest.
#[test]
fn every_dev_session_app_is_an_installed_package() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    let runtime = home.state().join("tilolang");
    std::fs::create_dir_all(&runtime).expect("a directory for the build");
    let installed = crate::common::dev_apps::install(&store, &runtime);
    home.env().set("TILO_APPS_DATA_DIR", &store);
    for app in installed {
        let found = app_catalogue::apps::read_one_from_env(&app)
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
#[test]
fn a_dev_session_installs_the_apps_and_not_the_parts_they_are_made_with() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    let runtime = home.state().join("tilolang");
    std::fs::create_dir_all(&runtime).expect("a directory for the build");
    let installed = crate::common::dev_apps::install(&store, &runtime);
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let catalog = package_store::Catalog::at(&store, None);
    let apps: Vec<String> = app_catalogue::apps::read(&catalog)
        .expect("the store's apps read")
        .into_iter()
        .map(|app| app.app_id.to_string())
        .collect();
    for app in &installed {
        assert!(apps.contains(app), "{app} is installed: {apps:?}");
    }
    assert!(
        !apps.contains(&"so.tilo.lang.discussion".to_owned()),
        "Discussion is no app of the session's: {apps:?}"
    );
    // The part itself rides in the apps made with it.
    for app in ["so.tilo.lang.tasks", "so.tilo.lang.notes"] {
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

/// The session plans each app's connector from its installed manifest: the `YouTube` client's
/// `youtube` connector and Mail's `imap` one are the apps' own declarations, and their launch is
/// the app's own with the origin named (`--tilolang-connector <origin>`) and nothing a window
/// has — no display, no render node, nothing open — which the launcher refuses beside it.
#[test]
fn the_installed_apps_connectors_are_planned_from_their_manifests() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    let runtime = home.state().join("tilolang");
    std::fs::create_dir_all(&runtime).expect("a directory for the build");
    crate::common::dev_apps::install(&store, &runtime);
    home.env().set("TILO_APPS_DATA_DIR", &store);

    // The launcher a connector's launch is planned through: what it would start is read from the
    // plan, so a stand-in that starts nothing is the launcher here.
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "exit 0\n")
        .expect("a launcher to plan through");
    // A session that shows windows: a connector's plan must leave its display out all the same.
    let environment = SessionEnvironment {
        wayland_socket: Some(home.state().join("wayland-0")),
        ..SessionEnvironment::from_process()
    };
    let apps = Apps::with_launcher(environment, None, Some(launcher.path().to_owned()));
    let workspace = apps.workspace_named("workspace-1");
    for (app, origin) in [
        ("so.tilo.lang.tube", "youtube"),
        ("so.tilo.lang.mail", "imap"),
    ] {
        let planned = apps
            .plan_connector(app, origin, &workspace)
            .unwrap_or_else(|why| {
                panic!("{app}'s {origin} connector is planned: {why}");
            });
        assert_eq!(planned.app, app);
        let named = planned
            .args
            .iter()
            .position(|arg| arg == "--tilolang-connector")
            .unwrap_or_else(|| panic!("{app}'s launch is a connector: {:?}", planned.args));
        assert_eq!(
            planned.args.get(named + 1).map(String::as_str),
            Some(origin),
            "{app}'s launch is its connector for {origin}: {:?}",
            planned.args
        );
        for window_only in [
            "--wayland-socket",
            "--render-node",
            "--open",
            "--open-uri",
            "--open-as-visitor",
        ] {
            assert!(
                !planned.args.iter().any(|arg| arg == window_only),
                "{app}'s connector launch is no window, so it has no {window_only}: {:?}",
                planned.args
            );
        }
        for display in ["WAYLAND_DISPLAY", "DISPLAY"] {
            assert!(
                !planned.env.contains_key(display),
                "{app}'s connector reaches no compositor: {display}"
            );
        }
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
#[test]
fn the_installed_apps_scenarios_fill_the_sessions_records() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    let scratch = home.state().join("tilolang");
    std::fs::create_dir_all(&scratch).expect("a directory for the build");
    crate::common::dev_apps::install(&store, &scratch);
    home.env().set("TILO_APPS_DATA_DIR", &store);

    let apps = crate::common::seeded_apps_in(&store);
    let mut kinds = std::collections::BTreeSet::new();
    kinds_of(&apps.resources(), &mut kinds);
    for kind in [
        "tl.so.tilo.lang.tube.Channel",
        "tl.so.tilo.lang.tube.Video",
        "tl.so.tilo.lang.notes.Note",
    ] {
        assert!(kinds.contains(kind), "{kind} is seeded: {kinds:?}");
    }
}
