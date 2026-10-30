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

#[test]
fn the_people_tab_lists_everyone_with_what_the_viewer_may_change() {
    let mut owner = staged("people");
    for text in [
        "3 members",
        "Bob Hellauer",
        "Ada Lovelace-Byron",
        "2 extra permissions",
    ] {
        assert!(says(&mut owner, text), "{text:?} is shown");
    }
    assert!(enabled(&mut owner, "Transfer ownership"));

    let mut member = staged("people-member");
    assert!(says(
        &mut member,
        "Only admins can change roles or remove people."
    ));
    assert!(!says(&mut member, "Transfer ownership"));
}

#[test]
fn removing_someone_asks_first_and_keep_changes_nothing() {
    let (mut app, seat) = seated("people");
    press(&mut app, "ws-people-remove-ada");
    assert!(says(&mut app, "Remove this person?"));
    assert!(says(&mut app, "Ada Lovelace-Byron"));
    assert!(
        seat.take_asked().is_empty(),
        "nothing is asked of the shell yet"
    );
    app.harness().key("Escape");
    app.settle();
    assert!(seat.take_asked().is_empty(), "keeping changes nothing");
    assert!(says(&mut app, "3 members"), "the tab is back");
    assert!(seat.presentation().is_open());
}

#[test]
fn a_confirmed_removal_asks_the_shell_for_that_member() {
    let (mut app, seat) = seated("people");
    press(&mut app, "ws-people-remove-bo");
    press(&mut app, "ws-settings-decision.primary");
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::RemoveMember {
            member: "bo".to_owned()
        }]
    );
}

#[test]
fn the_question_leads_to_the_activity_instead() {
    let (mut app, seat) = seated("people");
    press(&mut app, "ws-people-remove-bo");
    press(&mut app, "ws-settings-decision.tertiary");
    assert_eq!(seat.take_asked(), [WorkspaceSettingsEffect::ReviewActivity]);
}

#[test]
fn the_removal_question_renders_for_captures() {
    let mut app = staged("people-removing");
    assert!(says(&mut app, "Remove this person?"));
    assert!(says(&mut app, "Keep access"));
}

#[test]
fn an_agent_changing_a_role_goes_through_the_door() {
    let (mut app, seat) = seated("people");
    app.effect(&WorkspaceSettingsEffect::SetRole {
        member: "bo".to_owned(),
        role: "editor".to_owned(),
    })
    .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::SetRole {
            member: "bo".to_owned(),
            role: "editor".to_owned(),
        }]
    );
}

#[test]
fn switching_tabs_asks_the_shell() {
    let (mut app, seat) = seated("general");
    app.effect(&WorkspaceSettingsEffect::ShowTab {
        tab: "people".to_owned(),
    })
    .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::ShowTab {
            tab: "people".to_owned()
        }]
    );
}

#[test]
fn the_agents_tab_lists_agents_and_bots_apart() {
    let mut app = staged("agents");
    for text in ["2 agents", "claude", "Encrypted backup", "Bots", "Standup"] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    let mut empty = staged("agents-empty");
    assert!(says(&mut empty, "No agents yet"));
}

#[test]
fn agent_and_bot_controls_name_what_they_act_on() {
    let (mut app, seat) = seated("agents");
    press(&mut app, "ws-agents-review-claude");
    press(&mut app, "ws-agents-add");
    press(&mut app, "ws-bots-add");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::ReviewAgent {
                agent: "claude".to_owned()
            },
            WorkspaceSettingsEffect::AddAgent,
            WorkspaceSettingsEffect::AddBot,
        ]
    );
}

#[test]
fn an_agent_review_shows_its_answers_model_and_exceptions() {
    let mut app = staged("agent-review");
    for text in [
        "Notes",
        "Calendar",
        "Ask first",
        "Sonnet (Anthropic)",
        "Notes: Delete for good",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
}

#[test]
fn a_closed_app_lists_each_answer_as_its_own_fact() {
    // Notes is open in this scenario; Calendar is the closed one.
    let mut app = staged("agent-review");
    for text in [
        "Reading: Allow",
        "Changes: Allow",
        "Outside effects: Ask first",
    ] {
        assert!(
            app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some(text)),
            "{text:?} is a fact of its own"
        );
    }
    assert!(!says(&mut app, " · "), "no answers are joined with a glyph");
}

#[test]
fn in_a_review_escape_goes_back_to_the_agents() {
    let (mut app, seat) = seated("agent-review");
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [WorkspaceSettingsEffect::EndReview]);
    assert!(seat.presentation().is_open(), "the sheet stays up");
}

#[test]
fn a_review_asks_for_answers_models_and_resets_by_key() {
    let (mut app, seat) = seated("agent-review");
    press(&mut app, "ws-agent-app-so.tilo.notes");
    press(&mut app, "ws-agent-reset-so.tilo.notes");
    press(
        &mut app,
        "ws-agent-stop-so.tilo.notes/notes.notebook/delete-outside",
    );
    press(&mut app, "ws-agent-models");
    press(&mut app, "ws-agent-use-");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::ResetAgentApp {
                agent: "claude".to_owned(),
                scope: "so.tilo.notes".to_owned(),
            },
            WorkspaceSettingsEffect::SetAgentAnswer {
                agent: "claude".to_owned(),
                scope: "so.tilo.notes/notes.notebook/delete".to_owned(),
                kind: "outside".to_owned(),
                answer: String::new(),
            },
            WorkspaceSettingsEffect::SetAgentModel {
                agent: "claude".to_owned(),
                model: String::new(),
            },
        ]
    );
}

#[test]
fn an_agent_is_answered_through_the_door_too() {
    let (mut app, seat) = seated("agent-review");
    app.effect(&WorkspaceSettingsEffect::SetAgentAnswer {
        agent: "claude".to_owned(),
        scope: "so.tilo.calendar".to_owned(),
        kind: "change".to_owned(),
        answer: "deny".to_owned(),
    })
    .unwrap();
    app.settle();
    assert_eq!(seat.take_asked().len(), 1);
}

#[test]
fn the_permissions_tab_groups_what_is_given_by_who_holds_it() {
    let mut app = staged("permissions");
    for text in [
        "Give a permission",
        "Apps",
        "Notes",
        "People",
        "Create roles, given by you",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    assert!(enabled(&mut app, "Grant"));
}

#[test]
fn granting_names_who_what_and_for_how_long() {
    let (mut app, seat) = seated("permissions");
    press(&mut app, "ws-grant");
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::Grant {
            subject: "member:ada".to_owned(),
            permission: "camera".to_owned(),
            expiry: "session".to_owned(),
        }]
    );
}

#[test]
fn revoking_asks_first() {
    let (mut app, seat) = seated("permissions");
    press(&mut app, "ws-permission-revoke-grant-2");
    assert!(says(&mut app, "Revoke this permission?"));
    assert!(seat.take_asked().is_empty());
    press(&mut app, "ws-settings-decision.primary");
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::Revoke {
            grant: "grant-2".to_owned()
        }]
    );
}

#[test]
fn the_revoke_question_renders_for_captures() {
    let mut app = staged("permissions-revoking");
    assert!(says(&mut app, "Keep permission"));
}

#[test]
fn the_apps_tab_shows_both_choices_for_each_kind() {
    let mut app = staged("apps");
    for text in [".md files", "Notes", "mailto: links", "Mail"] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    assert!(says(&mut staged("apps-empty"), "No apps open files yet"));
}

#[test]
fn each_choice_moves_on_by_the_kind_it_is_for() {
    let (mut app, seat) = seated("apps");
    press(&mut app, "ws-apps-workspace-ext:md");
    press(&mut app, "ws-apps-yours-scheme:mailto");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::NextWorkspaceDefault {
                resource: "ext:md".to_owned()
            },
            WorkspaceSettingsEffect::NextPersonalDefault {
                resource: "scheme:mailto".to_owned()
            },
        ]
    );
}

#[test]
fn the_emoji_tab_lists_each_emoji_with_who_added_it() {
    let mut app = staged("emoji");
    for text in ["2 emoji", ":party-parrot:", "Added by Ada Lovelace-Byron"] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    assert!(enabled(&mut app, "Add emoji"));
    let mut refused = staged("emoji-refused");
    assert!(says(&mut refused, "cat.png was not added"));
    assert!(says(&mut refused, "Editors can add emoji."));
    assert!(!enabled(&mut refused, "Add emoji"));
}

#[test]
fn emoji_controls_ask_by_name() {
    let (mut app, seat) = seated("emoji");
    press(&mut app, "ws-emoji-add");
    press(&mut app, "ws-emoji-remove-party-parrot");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::AddEmoji,
            WorkspaceSettingsEffect::RemoveEmoji {
                name: "party-parrot".to_owned()
            },
        ]
    );
}

#[test]
fn only_the_emoji_the_person_may_remove_offer_remove() {
    let mut app = staged("emoji");
    assert!(says(&mut app, "Remove :party-parrot:"));
    assert!(!says(&mut app, "Remove :shipit:"));
}
