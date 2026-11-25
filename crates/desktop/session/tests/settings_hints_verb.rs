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
