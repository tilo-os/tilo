//! Workspace settings as a person, a screen reader and an agent meet them:
//! what each tab shows, what its controls ask of the shell, and what the
//! keys do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::Seat as _;
use tilo_sheets::workspace_settings::{
    Settings, WorkspaceSettingsApp, WorkspaceSettingsEffect, WorkspaceSettingsSeat, scenario,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 900);

fn staged(scenario: &str) -> HeadlessApp<WorkspaceSettingsApp> {
    HeadlessApp::<WorkspaceSettingsApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn says(app: &mut HeadlessApp<WorkspaceSettingsApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

/// Whether the control named `name` takes input.
fn enabled(app: &mut HeadlessApp<WorkspaceSettingsApp>, name: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .is_some_and(|(_, node)| !node.is_disabled())
}

/// The settings a scenario stages, shown through a seat as the shell lends
/// it.
fn seated(scenario: &str) -> (HeadlessApp<WorkspaceSettingsApp>, WorkspaceSettingsSeat) {
    let settings = staged_settings(scenario);
    let seat = WorkspaceSettingsSeat::default();
    seat.show(settings);
    seat.presentation().open(false);
    let mut app =
        HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn staged_settings(name: &str) -> Settings {
    scenario(Some(name))
}

fn press(app: &mut HeadlessApp<WorkspaceSettingsApp>, id: &str) {
    app.harness()
        .perform_named(id, &A11yAction::Activate)
        .unwrap();
    app.settle();
}

#[test]
fn the_general_tab_shows_the_workspace_and_what_the_person_may_change() {
    let mut owner = staged("general");
    for text in ["Studio", "Owner", "Invite people", "Devices", "Old project"] {
        assert!(says(&mut owner, text), "{text:?} is shown");
    }
    assert!(enabled(&mut owner, "Create invitation"));
    assert!(enabled(&mut owner, "Delete…"));
    assert!(!enabled(&mut owner, "Move earlier"), "it is first already");

    let mut viewer = staged("general-viewer");
    assert!(says(&mut viewer, "Only admins can rename the workspace."));
    assert!(!enabled(&mut viewer, "Create invitation"));
    assert!(!says(&mut viewer, "Old project"));
}

#[test]
fn general_controls_ask_the_shell_by_what_they_name() {
    let (mut app, seat) = seated("general");
    press(&mut app, "ws-general-device-tablet");
    press(&mut app, "ws-general-move-down");
    press(&mut app, "ws-general-public");
    press(&mut app, "ws-general-restore-old-project");
    press(&mut app, "ws-general-invite");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::ToggleDevice {
                device: "tablet".to_owned()
            },
            WorkspaceSettingsEffect::Move { step: 1 },
            WorkspaceSettingsEffect::SetPublic { public: true },
            WorkspaceSettingsEffect::Restore {
                workspace: "old-project".to_owned()
            },
            WorkspaceSettingsEffect::Invite,
        ]
    );
}

#[test]
fn a_typed_name_is_saved_with_enter_and_taken_back_with_escape() {
    let (mut app, seat) = seated("general");
    app.harness()
        .perform_named(
            "ws-general-name",
            &A11yAction::SetValue("Atelier".to_owned()),
        )
        .unwrap();
    app.settle();
    assert_eq!(seat.typed_name(), "Atelier");
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.typed_name(), "Studio", "Escape takes the name back");
    assert!(seat.take_asked().is_empty(), "and closes nothing");
    assert!(seat.presentation().is_open());

    app.harness()
        .perform_named(
            "ws-general-name",
            &A11yAction::SetValue("Atelier".to_owned()),
        )
        .unwrap();
    app.settle();
    app.harness()
        .perform_named("ws-general-name", &A11yAction::Focus)
        .unwrap();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::Rename {
            name: "Atelier".to_owned()
        }]
    );
}

#[test]
fn closing_saves_a_typed_name_first() {
    let (mut app, seat) = seated("general");
    app.harness()
        .perform_named(
            "ws-general-name",
            &A11yAction::SetValue("Atelier".to_owned()),
        )
        .unwrap();
    app.settle();
    press(&mut app, "ws-settings.secondary");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::Rename {
                name: "Atelier".to_owned()
            },
            WorkspaceSettingsEffect::Close,
        ]
    );
    assert!(!seat.presentation().is_drawn());
}

#[test]
fn escape_with_nothing_typed_closes() {
    let (mut app, seat) = seated("general");
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [WorkspaceSettingsEffect::Close]);
}

#[test]
fn a_viewer_cannot_rename() {
    let (mut app, seat) = seated("general-viewer");
    app.harness()
        .perform_named("ws-general-name", &A11yAction::SetValue("Mine".to_owned()))
        .ok();
    app.settle();
    press(&mut app, "ws-settings.secondary");
    assert_eq!(seat.take_asked(), [WorkspaceSettingsEffect::Close]);
}

#[test]
fn a_name_the_person_has_not_touched_follows_the_workspace() {
    let (mut app, seat) = seated("general");
    let mut renamed = seat.settings();
    renamed.name = "Atelier".to_owned();
    seat.show(renamed);
    app.settle();
    assert_eq!(seat.typed_name(), "Atelier");
    assert!(says(&mut app, "Atelier"));
}

#[test]
fn an_agent_changes_the_workspace_through_the_same_door() {
    let (mut app, seat) = seated("general");
    app.effect(&WorkspaceSettingsEffect::Move { step: -1 })
        .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::Move { step: -1 }]
    );
}
