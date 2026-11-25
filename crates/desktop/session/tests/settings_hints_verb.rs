//! The shell's control hints write the person's record through the section's own verb: the call
//! its `on learn(hint)` handler makes (`settings.learn_control_hint(settings.control_hints,
//! hint: hint)`) reaches the session's API as `settings/control-hints#learn-control-hint`, as a
//! press in any of the shell's views does, and the hint is recorded as learned.

use std::time::Duration;

use serde_json::{Map, json};

const CONTROL_HINTS: &str = "settings/control-hints";

/// A session keeping the person's settings, stepped until it offers them.
fn settings_session(root: &std::path::Path) -> tilo_session::Session {
    let mut session = crate::common::headless(tilo_session::Options {
        services: vec![Box::new(settings_sections::service::SettingsService::new())],
        ..crate::common::api_options(root)
    });
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a pass");
    }
    session
}

/// A session as the shell of a device daemon of the test's own, keeping the person's settings:
/// the shell the person uses on their computer, its asks reaching the session's API.
fn settings_session_on_daemon(root: &std::path::Path) -> tilo_session::Session {
    let mut session = crate::common::on_its_daemon_with(
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |_device| tilo_session::Options {
            services: vec![Box::new(settings_sections::service::SettingsService::new())],
            collab: false,
            // As `dist/dev/nested-session.sh` starts a session beside another desktop, which is
            // how a walk runs one.
            machine: tilo_session::Machine::Window,
            // The socket the launcher hands its apps: with one, the session serves its seat's
            // providers to its device's daemon.
            app_socket: tilo_session::AppSocket::At(root.join("app_wire.sock")),
            ..tilo_session::Options::default()
        },
    );
    session.step(Duration::from_millis(1)).expect("a pass");
    session
}

/// The hint the shell says began counts as shown when the shell of a session on its device's
/// daemon asks its API, as it does a moment after the person sees it: a hint that is shown but
/// never counted shows forever.
#[test]
fn the_shells_hint_ask_reaches_the_section_on_its_daemon() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = settings_session_on_daemon(root.path());
    let read = crate::common::wait_for(
        &mut session,
        "the settings the session publishes",
        |session| session.read(CONTROL_HINTS)?.get("learned").cloned(),
    );
    // A setting the person changed first, as the walk's own first step changes the theme: the
    // service publishes its sections again after every change.
    session
        .invoke(
            "settings/appearance",
            "set-dark-mode",
            &serde_json::Map::from_iter([("on".to_owned(), json!(false))]),
        )
        .expect("the appearance changes");
    let resources = session.resources();
    let settings: Vec<(&str, Vec<&str>)> = resources
        .iter()
        .filter(|resource| resource.id.as_str().starts_with("settings/"))
        .map(|resource| {
            (
                resource.id.as_str(),
                resource
                    .actions
                    .iter()
                    .map(|spec| spec.id.as_str())
                    .collect(),
            )
        })
        .collect();
    let owners: Vec<&tilo_actions::ResourceSpec> = resources
        .iter()
        .filter(|resource| resource.id.as_str() == CONTROL_HINTS)
        .collect();
    assert_eq!(
        owners.len(),
        1,
        "one surface offers {CONTROL_HINTS}: {settings:?}"
    );
    let published = owners[0];
    assert!(
        published
            .actions
            .iter()
            .any(|spec| spec.id == "note-control-hint-shown"),
        "{CONTROL_HINTS} publishes the verb the shell asks: {:?}",
        published
            .actions
            .iter()
            .map(|spec| spec.id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(read["shown"].get("open-from-search"), None);
    // The person sits at the desktop a while: the shell's hints begin to show, and the widget
    // that shows one publishes the section it writes.
    for _ in 0..300 {
        session.step(Duration::from_millis(50)).expect("a pass");
    }
    session
        .shell_link()
        .expect("the shell runs")
        .ask(tilo_session::shell::part::Ask {
            resource: CONTROL_HINTS.to_owned(),
            action: "note-control-hint-shown".to_owned(),
            arguments: std::collections::BTreeMap::from([(
                "hint".to_owned(),
                json!("open-from-search"),
            )]),
            from: None,
        });
    let shown = crate::common::wait_for(&mut session, "the hint to count as shown", |session| {
        session
            .read(CONTROL_HINTS)?
            .get("learned")?
            .get("shown")?
            .get("open-from-search")
            .cloned()
    });
    assert!(
        session.shell_refusals().is_empty(),
        "the session ran what the shell asked: {:?}",
        session.shell_refusals()
    );
    assert_eq!(
        shown,
        json!(1),
        "the hint the person was shown counts as shown"
    );
}

#[test]
fn the_shells_hint_call_is_the_sections_own_verb() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = settings_session(root.path());
    let published = session
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == CONTROL_HINTS)
        .expect("the session publishes the control hints section");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(
        actions.contains(&"learn-control-hint"),
        "{CONTROL_HINTS} publishes the verb the shell's hint call names: {actions:?}"
    );
    let before = session
        .read(CONTROL_HINTS)
        .expect("the control hints section reads");
    assert_eq!(before["learned"]["known"], json!([]));
    session
        .invoke(
            CONTROL_HINTS,
            "learn-control-hint",
            &Map::from_iter([("hint".to_owned(), json!("open-from-search"))]),
        )
        .expect("the shell's hint call runs");
    session.settle_services();
    assert_eq!(
        session.read(CONTROL_HINTS).expect("published")["learned"]["known"],
        json!(["open-from-search"]),
        "the hint is recorded as learned"
    );
}
