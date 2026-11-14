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
    seated_with(staged_settings(scenario))
}

/// `settings` shown through a seat, as the shell lends it.
fn seated_with(settings: Settings) -> (HeadlessApp<WorkspaceSettingsApp>, WorkspaceSettingsSeat) {
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
    for text in ["Studio", "Owner", "Invite people", "Devices"] {
        assert!(says(&mut owner, text), "{text:?} is shown");
    }
    assert!(enabled(&mut owner, "Create invitation"));
    assert!(enabled(&mut owner, "Leave"));
    assert!(!enabled(&mut owner, "Move earlier"), "it is first already");

    let mut viewer = staged("general-viewer");
    assert!(says(&mut viewer, "Only admins can rename the workspace."));
    assert!(!enabled(&mut viewer, "Create invitation"));
}

#[test]
fn one_device_has_no_order_to_choose_between() {
    let mut settings = staged_settings("general");
    settings.general.device_rows.truncate(1);
    settings.general.move_labels = ["Move earlier".to_owned(), "Move later".to_owned()];
    let (mut app, _seat) = seated_with(settings);

    assert!(!says(&mut app, "Move earlier"));
    assert!(!says(&mut app, "Move later"));
    assert!(
        says(&mut app, "Laptop (this device)"),
        "the one device's row is still shown"
    );
}

#[test]
fn general_controls_ask_the_shell_by_what_they_name() {
    let (mut app, seat) = seated("general");
    press(&mut app, "ws-general-device-tablet");
    press(&mut app, "ws-general-move-down");
    press(&mut app, "ws-general-invite");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::ToggleDevice {
                device: "tablet".to_owned()
            },
            WorkspaceSettingsEffect::Move { step: 1 },
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
    assert_eq!(
        seat.presentation().phase(),
        tilo_sheets::Phase::Leaving,
        "the card leaves the screen; the shell hears it is gone once it has"
    );
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
    let mut member = staged("people-member");
    assert!(says(
        &mut member,
        "Only admins can change roles or remove people."
    ));
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
    for text in [
        "3 agents",
        "Claude",
        "Kept by you",
        "Kept by Ada",
        "Model: Sonnet from Anthropic",
        "Reviewer",
        "Encrypted backup",
        "Bots",
        "Bots run small programs here on their own",
        "Standup",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    let mut empty = staged("agents-empty");
    assert!(says(&mut empty, "No agents yet"));
}

#[test]
fn agent_and_bot_controls_name_what_they_act_on() {
    let (mut app, seat) = seated("agents");
    press(&mut app, "ws-agents-review-claude");
    // One button opens the form; a press with no name in it asks nothing.
    press(&mut app, "ws-agents-new");
    press(&mut app, "ws-agents-create");
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::ReviewAgent {
            agent: "claude".to_owned()
        }],
        "only the review was asked"
    );
    app.harness()
        .perform_named(
            "ws-agents-name",
            &A11yAction::SetValue("Analyst".to_owned()),
        )
        .unwrap();
    app.settle();
    assert!(
        says(&mut app, "Analyst"),
        "the form holds the name typed into it"
    );
    assert!(says(
        &mut app,
        "Its colour comes from its name until you pick one."
    ));
    press(&mut app, "ws-agents-create");
    press(&mut app, "ws-bots-add");
    assert_eq!(
        seat.take_asked(),
        [
            WorkspaceSettingsEffect::AddAgent {
                name: "Analyst".to_owned(),
                model: String::new(),
                colour: String::new(),
            },
            WorkspaceSettingsEffect::AddBot,
        ]
    );
}

/// Remove takes back a bot a member asked for, and the tab offers no Remove
/// for one the workspace's host runs from its own choice.
#[test]
fn a_requested_bot_is_removed_here_and_a_hosts_bot_is_not() {
    let (mut app, seat) = seated("agents");
    let removes = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.label() == Some("Remove"))
        .count();
    // One per agent, and one for the only bot a member asked for.
    assert_eq!(
        removes, 4,
        "only the bot a member asked for has a bot's Remove"
    );
    press(&mut app, "ws-bots-remove-so.tilo.bots.standup");
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::RemoveBot {
            bot: "so.tilo.bots.standup".to_owned()
        }]
    );
}

#[test]
fn an_agent_review_shows_its_answers_model_and_exceptions() {
    let mut app = staged("agent-review");
    for text in [
        "Can read and change, asks before acting outside",
        "Notes",
        "Calendar",
        "Ask first",
        "Set here",
        "Sonnet from Anthropic",
        "Notes: Delete for good",
        "Remove",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
}

/// Every app shows its answers as choices, with no text to open them first.
#[test]
fn every_app_offers_each_answer_as_a_choice() {
    let mut app = staged("agent-review");
    for name in [
        "Notes Reading",
        "Notes Changes",
        "Notes Outside effects",
        "Calendar Reading",
        "Calendar Changes",
        "Calendar Outside effects",
    ] {
        assert!(
            app.harness().a11y_node_named(name).is_some(),
            "{name:?} is offered without opening anything"
        );
    }
    assert!(
        !says(&mut app, "Reading:"),
        "no answer is listed as text beside its control"
    );
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
    press(&mut app, "ws-agent-reset-so.tilo.notes");
    press(
        &mut app,
        "ws-agent-stop-so.tilo.notes/notes.notebook/delete-outside",
    );
    press(&mut app, "ws-agent-use-");
    press(&mut app, "ws-agent-retire");
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
            WorkspaceSettingsEffect::RemoveAgent {
                agent: "claude".to_owned(),
            },
        ]
    );
}

/// What an agent spends and may spend reads on its row; its keeper sets the
/// day from its review, where a typed amount is taken by Set and by Enter.
#[test]
fn an_agents_day_is_read_on_its_row_and_set_in_its_review() {
    let (mut app, _) = seated("agents");
    assert!(
        says(&mut app, "$0.42 of $5 today"),
        "the row counts the day on its line under the name, under who keeps it"
    );
    assert!(
        says(&mut app, "$1.20 today"),
        "an unmetered agent says what it spent on that line too"
    );

    let (mut app, seat) = seated("agent-review");
    assert!(
        says(&mut app, "$5 a day"),
        "the review says the day's limit"
    );
    assert!(says(&mut app, "$0.42 today"), "and what it spent of it");
    // The field takes the room and Set keeps its own at the row's right end,
    // both on the one line.
    let drawn = app.harness().targets();
    let field = drawn["ws-agent-limit-field"];
    let button = drawn["ws-agent-limit-set"];
    assert!(
        field.width() >= button.width() * 3.0,
        "the field grows beside Set: field {field:?}, button {button:?}"
    );
    assert!(
        (field.center().y - button.center().y).abs() <= 1.0,
        "the field and Set share a line: field {field:?}, button {button:?}"
    );
    assert!(
        button.x0 >= field.x1 && button.x0 - field.x1 <= 16.0,
        "Set sits at the field's right end: field {field:?}, button {button:?}"
    );
    app.harness()
        .perform_named(
            "ws-agent-limit-field",
            &A11yAction::SetValue("12.50".to_owned()),
        )
        .unwrap();
    app.settle();
    press(&mut app, "ws-agent-limit-set");
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::SetAgentSpendLimit {
            agent: "claude".to_owned(),
            dollars: "12.50".to_owned(),
        }]
    );

    // Enter in the field takes it too, and an empty field asks for no limit.
    app.harness()
        .perform_named("ws-agent-limit-field", &A11yAction::SetValue(String::new()))
        .unwrap();
    app.settle();
    app.harness()
        .perform_named("ws-agent-limit-field", &A11yAction::Focus)
        .unwrap();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::SetAgentSpendLimit {
            agent: "claude".to_owned(),
            dollars: String::new(),
        }]
    );
}

/// What an agent may spend a day is its keeper's to set, as its model is: a
/// review someone else opens says who sets it and offers no field.
#[test]
fn a_review_offers_the_day_to_the_agents_keeper_alone() {
    let mut settings = staged_settings("agent-review");
    let limit = settings
        .agents
        .review
        .as_mut()
        .expect("a review")
        .limit
        .as_mut()
        .expect("the agent's day");
    limit.can_choose = false;
    limit.note = "Only Ada sets what it may spend a day.".to_owned();
    let seat = WorkspaceSettingsSeat::default();
    seat.show(settings);
    seat.presentation().open(false);
    let mut app =
        HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", seat).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();

    assert!(says(&mut app, "Only Ada sets what it may spend a day."));
    assert!(
        app.harness()
            .perform_named("ws-agent-limit-field", &A11yAction::Focus)
            .is_err(),
        "someone who is not keeper is offered no field"
    );
}

/// The model an agent plans with is its keeper's to choose: a review someone
/// else opens says who chooses and offers no Use.
#[test]
fn a_review_offers_the_model_to_the_agents_keeper_alone() {
    let mut settings = staged_settings("agent-review");
    let models = settings
        .agents
        .review
        .as_mut()
        .expect("a review")
        .models
        .as_mut()
        .expect("the agent's models");
    models.can_choose = false;
    models.note = Some("Only Ada chooses the model it runs on".to_owned());
    let seat = WorkspaceSettingsSeat::default();
    seat.show(settings);
    seat.presentation().open(false);
    let mut app =
        HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", seat).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();

    assert!(
        says(&mut app, "Only Ada chooses the model it runs on"),
        "the review says who chooses"
    );
    let uses = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.label() == Some("Use"))
        .count();
    assert_eq!(uses, 0, "no model is offered to someone who is not keeper");
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

#[test]
fn deleting_the_workspace_asks_first_and_keep_changes_nothing() {
    let (mut app, seat) = seated("general");
    press(&mut app, "ws-general-delete");
    assert!(says(&mut app, "Delete Design studio?"));
    assert!(says(&mut app, "Everything in this workspace goes with it"));
    assert!(
        seat.take_asked().is_empty(),
        "nothing is asked of the shell yet"
    );
    app.harness().key("Escape");
    app.settle();
    assert!(seat.take_asked().is_empty(), "keeping changes nothing");
    assert!(says(&mut app, "At a glance"), "the tab is back");
    assert!(seat.presentation().is_open());
}

#[test]
fn a_confirmed_delete_asks_the_shell_for_the_workspace() {
    let (mut app, seat) = seated("general");
    press(&mut app, "ws-general-delete");
    press(&mut app, "ws-settings-decision.primary");
    assert_eq!(
        seat.take_asked(),
        [WorkspaceSettingsEffect::DeleteWorkspace]
    );
}

#[test]
fn the_delete_question_renders_for_captures() {
    let mut app = staged("general-deleting");
    assert!(says(&mut app, "Delete Design studio?"));
    assert!(says(&mut app, "Keep"));
}
