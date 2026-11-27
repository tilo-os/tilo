//! A session started as the shell of its device's daemon, its apps' wire up: the seat's providers
//! are served on the shell link while the session's own loop reads and writes on it, and the
//! session gets through its start and its passes and answers. A read the loop waits for must never
//! wait on something only the loop itself, or a task nobody polls, can answer.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_session::{AppSocket, Options};

#[test]
fn a_session_on_its_daemon_serving_the_seat_starts_and_answers() {
    let root = crate::common::private_home();
    let socket = root.path().join("app_wire.sock");
    let (started, answered) = std::sync::mpsc::channel();
    // The session runs on a thread of its own: a hang in its start or its loop fails this test
    // with what it waited for, where it would otherwise stop the runner.
    std::thread::spawn(move || {
        let mut session = crate::common::on_its_daemon(
            &[OutputConfig::new("HEADLESS-1", 1280, 800)],
            Options {
                app_socket: AppSocket::At(socket),
                collab: false,
                ..Options::default()
            },
        );
        for _ in 0..3 {
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        let workspaces = session.handle().read(|tree| tree.workspaces.len());
        started
            .send(workspaces)
            .expect("the test waits for the session");
        // The session ends with the test's process: it is not dropped here, so a teardown of its
        // own never reads as the start's answer.
        std::mem::forget(session);
    });
    let workspaces = tilo_test_env::recv(
        "the session on its daemon to start and take its first passes",
        &answered,
    );
    assert!(
        workspaces >= 1,
        "the session shows the person's workspaces it read through the daemon"
    );
}

/// The shell offers an app's actions in the workspace in front, as its search and launcher read
/// them from the device's dispatch: the session asks the dispatch in each workspace it shows, so
/// an app the person opened there is listed with its actions.
#[test]
fn the_shell_lists_an_opened_apps_actions_in_its_workspace() {
    use tilo_session::AppActions as _;
    let mut fixture =
        crate::common::linked::Builder::of(crate::common::linked::DEFAULT_APP).start();
    let apps = std::rc::Rc::clone(&fixture.apps);
    let workspace = fixture.workspace.clone();
    let listed = crate::common::wait_for(
        &mut fixture.session,
        "the app's actions in the shell's listing",
        |session| {
            session.step(Duration::from_millis(5)).expect("a frame");
            apps.published(&workspace)
                .into_iter()
                .find(|published| published.app == crate::common::linked::DEFAULT_APP)
        },
    );
    assert_eq!(
        listed.workspace, workspace,
        "listed in the workspace in front"
    );
    assert!(!listed.actions.is_empty(), "the app's actions are listed");
}

/// Settings' sections are served by the session on the device's dispatch, which lists them to the
/// person in the workspace in front, as it lists any app's action: the appearance's switch is
/// what a script's `tilo do set-dark-mode` and the app walks find there.
#[test]
fn the_persons_listing_offers_the_settings_the_session_serves() {
    let mut fixture = crate::common::linked::Builder::without_an_app()
        .service(Box::new(settings_sections::service::SettingsService::new()))
        .start();
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        let rows = fixture.catalogue("dark", None).unwrap_or_default();
        if rows.iter().any(|row| {
            row.action.as_ref().is_some_and(|action| {
                action.address.resource == "settings/appearance"
                    && action.address.action == "set-dark-mode"
            })
        }) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            let listed = |rows: Vec<tilo_actions::remote::CatalogueRow>| {
                rows.into_iter()
                    .filter_map(|row| {
                        let action = row.action?;
                        Some(format!(
                            "{} {}#{}",
                            row.provider, action.address.resource, action.address.action
                        ))
                    })
                    .collect::<Vec<_>>()
            };
            let everything = fixture.catalogue("", None).map(listed);
            let settings = fixture
                .catalogue("", Some(tilo_actions::remote::settings_resources::PROVIDER))
                .map(listed);
            panic!(
                "the person's listing never offered settings/appearance#set-dark-mode; \
                 everything it lists: {everything:?}; what the settings provider lists: {settings:?}"
            );
        }
        fixture.pass(Duration::from_millis(100));
    }
}

/// The person's listing names each setting once: the settings' sections, the person's and the
/// device's, are the settings provider's to publish, at the addresses it answers on, by words
/// that say what each does. No second copy stands beside them, and no two settings read alike.
#[test]
fn the_persons_listing_names_each_setting_once() {
    use std::collections::BTreeMap;

    /// Whether `row` offers an action of a section of the settings.
    fn setting(row: &tilo_actions::remote::CatalogueRow) -> bool {
        row.action.as_ref().is_some_and(|action| {
            settings_sections::service::keeps(action.address.resource.as_str())
        })
    }

    let mut fixture = crate::common::linked::Builder::without_an_app()
        .service(Box::new(settings_sections::service::SettingsService::new()))
        .start();
    // The settings provider is served on the device's dispatch beside the session's services:
    // what the person reads is whole once a section of the settings is listed.
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    let rows = loop {
        let rows = fixture.catalogue("", None).unwrap_or_default();
        if rows.iter().any(setting) {
            break rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the person's listing carries the settings the session serves: {:?}",
            fixture.catalogue("", None).map(|rows| rows.len())
        );
        fixture.pass(Duration::from_millis(100));
    };
    // Each setting's address stands once, at the settings provider's rows, and each reading of a
    // section's words names one setting: two rows a person reads alike are a copy or a muddle.
    let mut by_address: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut by_words: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows.iter().filter(|row| setting(row)) {
        let Some(action) = &row.action else {
            continue;
        };
        let address = format!("{}#{}", action.address.resource, action.address.action);
        by_address
            .entry(address.clone())
            .or_default()
            .push(row.provider.as_str());
        by_words
            .entry(format!(
                "{} on {}",
                action.action_label, action.resource_kind
            ))
            .or_default()
            .push(address);
    }
    assert!(
        by_address
            .keys()
            .any(|address| address.starts_with("settings/")),
        "the listing carries the person's settings: {:?}",
        by_address.keys().collect::<Vec<_>>()
    );
    let twice: Vec<String> = by_address
        .iter()
        .filter(|(_, providers)| providers.len() > 1)
        .map(|(address, providers)| format!("{address}: {providers:?}"))
        .collect();
    assert!(
        twice.is_empty(),
        "each setting is listed once:\n{}",
        twice.join("\n")
    );
    let elsewhere: Vec<String> = by_address
        .iter()
        .filter(|(_, providers)| {
            providers
                .iter()
                .any(|provider| *provider != tilo_actions::remote::settings_resources::PROVIDER)
        })
        .map(|(address, providers)| format!("{address}: {providers:?}"))
        .collect();
    assert!(
        elsewhere.is_empty(),
        "each setting is the settings provider's:\n{}",
        elsewhere.join("\n")
    );
    let alike: Vec<String> = by_words
        .iter()
        .filter(|(_, addresses)| addresses.len() > 1)
        .map(|(words, addresses)| format!("{words}: {addresses:?}"))
        .collect();
    assert!(
        alike.is_empty(),
        "each setting reads as its own:\n{}",
        alike.join("\n")
    );
}
