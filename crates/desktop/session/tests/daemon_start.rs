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

/// The person's listing names each action once: every action the device's dispatch lists the
/// person stands at one address, offered by the one provider that answers it, by words that say
/// what it does and that no other row reads. The settings are the settings provider's, the
/// session's services are its services provider's, nearby send is the device's, and the workspace
/// activity the shell shows is the seat's.
#[test]
fn the_persons_listing_names_each_action_once() {
    use std::collections::BTreeMap;

    /// The provider the session's services are served under on the device's dispatch.
    const SERVICES: &str = "session.services";

    /// What one row reads as: its action's words for the resource it acts on, as a person's list
    /// and `tilo do` name a row.
    fn words(row: &tilo_actions::remote::CatalogueRow) -> String {
        let Some(action) = &row.action else {
            return String::new();
        };
        let place = row.resource.label.trim();
        let place = if place.is_empty() {
            action.resource_label.trim()
        } else {
            place
        };
        format!("{} on {place}", action.action_label)
    }

    /// What one row stands at, and where: `resource#action` at the provider that offers it, by
    /// the words it reads as.
    fn shown(row: &tilo_actions::remote::CatalogueRow) -> String {
        let Some(action) = &row.action else {
            return String::new();
        };
        format!(
            "{}#{} at {}: {:?}",
            action.address.resource,
            action.address.action,
            row.provider,
            words(row)
        )
    }

    let mut fixture = crate::common::linked::Builder::without_an_app()
        .service(Box::new(settings_sections::service::SettingsService::new()))
        .start();
    // Every provider the device's dispatch lists is served as the session starts: what the
    // person reads is whole once a row of each stands in it.
    let wanted = [
        tilo_actions::remote::settings_resources::PROVIDER,
        SERVICES,
        tilo_actions::remote::session_actions::PROVIDER,
        tilo_actions::remote::workspace_activity::PROVIDER,
        tilo_runtime::device::nearby::PROVIDER,
    ];
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    let rows = loop {
        let rows = fixture.catalogue("", None).unwrap_or_default();
        if wanted
            .iter()
            .all(|provider| rows.iter().any(|row| row.provider.as_str() == *provider))
        {
            break rows;
        }
        if std::time::Instant::now() >= deadline {
            let mut listed: Vec<String> = rows.iter().map(shown).collect();
            listed.sort();
            panic!(
                "the person's listing is served by {wanted:?}; it lists:\n{}",
                listed.join("\n")
            );
        }
        fixture.pass(Duration::from_millis(100));
    };
    // Each address stands once, from one provider; each row reads as its own words; and the
    // settings are the settings provider's, as they were before the rest was held to this.
    let mut by_address: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut by_words: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in &rows {
        let Some(action) = &row.action else {
            continue;
        };
        let address = format!("{}#{}", action.address.resource, action.address.action);
        by_address
            .entry(address.clone())
            .or_default()
            .push(row.provider.clone());
        by_words.entry(words(row)).or_default().push(shown(row));
    }
    let twice: Vec<String> = by_address
        .iter()
        .filter(|(_, providers)| providers.len() > 1)
        .map(|(address, providers)| format!("{address}: {providers:?}"))
        .collect();
    assert!(
        twice.is_empty(),
        "each address is listed once:\n{}",
        twice.join("\n")
    );
    let elsewhere: Vec<String> = by_address
        .iter()
        .filter(|(address, providers)| {
            address.starts_with("settings/")
                && providers
                    .iter()
                    .any(|provider| provider != tilo_actions::remote::settings_resources::PROVIDER)
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
        "each row reads as its own:\n{}",
        alike.join("\n")
    );
    // Each provider that answers a group the session's listing once doubled still offers it:
    // the one-row rule above must not be kept by offering none.
    for (resource, provider) in [
        ("session:invite", SERVICES),
        (
            "session:nearby-send",
            tilo_runtime::device::nearby::PROVIDER,
        ),
        (
            "workspace.activity",
            tilo_actions::remote::workspace_activity::PROVIDER,
        ),
    ] {
        assert!(
            by_address.keys().any(|address| {
                address.starts_with(&format!("{resource}#"))
                    && by_address[address] == [provider.to_owned()]
            }),
            "{resource} is offered by {provider}: {:?}",
            by_address
                .iter()
                .filter(|(address, _)| address.starts_with(resource))
                .map(|(address, providers)| format!("{address}: {providers:?}"))
                .collect::<Vec<_>>()
        );
    }
}

/// The person's listing offers the shell's own actions: the verbs the session publishes under
/// `so.tilo.session` on `desktop:chrome` (a new strip among them) are listed beside every app's,
/// so a shortcut, a script or an agent can name one and the daemon knows where it runs.
#[test]
fn the_persons_listing_offers_the_shells_own_actions() {
    let mut fixture = crate::common::linked::Builder::without_an_app().start();
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        let rows = fixture.catalogue("", None).unwrap_or_default();
        let offered = |rows: &[tilo_actions::remote::CatalogueRow]| {
            rows.iter().any(|row| {
                row.app_id == "so.tilo.session"
                    && row.action.as_ref().is_some_and(|action| {
                        action.address.resource == "desktop:chrome"
                            && action.address.action == "new-strip"
                    })
            })
        };
        if offered(&rows) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            let session = rows
                .iter()
                .filter(|row| row.app_id == "so.tilo.session")
                .filter_map(|row| row.action.as_ref())
                .map(|action| format!("{}#{}", action.address.resource, action.address.action))
                .collect::<Vec<_>>();
            panic!(
                "the person's listing never offered so.tilo.session desktop:chrome#new-strip; \
                 the session's rows it lists: {session:?}"
            );
        }
        fixture.pass(Duration::from_millis(100));
    }
}
