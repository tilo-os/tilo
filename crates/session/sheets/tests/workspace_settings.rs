//! Workspace settings as a person, a screen reader and an agent meet them:
//! what each tab shows, what its controls ask of the shell, and what the
//! keys do.

use tilo_sheets::SheetState as _;
use tilo_sheets::workspace_settings::{
    Settings, WorkspaceSettingsApp, WorkspaceSettingsEffect, WorkspaceSettingsState, scenario,
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

/// The settings a scenario stages, shown through a state as the shell lends
/// it.
fn mounted(scenario: &str) -> (HeadlessApp<WorkspaceSettingsApp>, WorkspaceSettingsState) {
    mounted_with(staged_settings(scenario))
}

/// `settings` shown through a state, as the shell lends it.
fn mounted_with(settings: Settings) -> (HeadlessApp<WorkspaceSettingsApp>, WorkspaceSettingsState) {
    let state = WorkspaceSettingsState::default();
    state.show(settings);
    state.presentation().open(false);
    let mut app =
        HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", state.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, state)
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

tilo_test_env::scenario! {
    /// The General tab of workspace settings, and the question before the workspace is deleted.
    fn the_general_tab_shows_renames_and_deletes_the_workspace;

    phase the_general_tab_shows_the_workspace_and_what_the_person_may_change;
    {
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

    phase one_device_has_no_order_to_choose_between;
    {
        let mut settings = staged_settings("general");
        settings.general.device_rows.truncate(1);
        settings.general.move_labels = ["Move earlier".to_owned(), "Move later".to_owned()];
        let (mut app, _state) = mounted_with(settings);

        assert!(!says(&mut app, "Move earlier"));
        assert!(!says(&mut app, "Move later"));
        assert!(
            says(&mut app, "Laptop (this device)"),
            "the one device's row is still shown"
        );
    }

    phase general_controls_ask_the_shell_by_what_they_name;
    {
        let (mut app, state) = mounted("general");
        press(&mut app, "ws-general-device-tablet");
        press(&mut app, "ws-general-move-down");
        press(&mut app, "ws-general-invite");
        assert_eq!(
            state.take_asked(),
            [
                WorkspaceSettingsEffect::ToggleDevice {
                    device: "tablet".to_owned()
                },
                WorkspaceSettingsEffect::Move { step: 1 },
                WorkspaceSettingsEffect::Invite,
            ]
        );
    }

    phase a_typed_name_is_saved_with_enter_and_taken_back_with_escape;
    {
        let (mut app, state) = mounted("general");
        app.harness()
            .perform_named(
                "ws-general-name",
                &A11yAction::SetValue("Atelier".to_owned()),
            )
            .unwrap();
        app.settle();
        assert_eq!(state.typed_name(), "Atelier");
        app.harness().key("Escape");
        app.settle();
        assert_eq!(state.typed_name(), "Studio", "Escape takes the name back");
        assert!(state.take_asked().is_empty(), "and closes nothing");
        assert!(state.presentation().is_open());

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
            state.take_asked(),
            [WorkspaceSettingsEffect::Rename {
                name: "Atelier".to_owned()
            }]
        );
    }

    phase closing_saves_a_typed_name_first;
    {
        let (mut app, state) = mounted("general");
        app.harness()
            .perform_named(
                "ws-general-name",
                &A11yAction::SetValue("Atelier".to_owned()),
            )
            .unwrap();
        app.settle();
        press(&mut app, "ws-settings.secondary");
        assert_eq!(
            state.take_asked(),
            [
                WorkspaceSettingsEffect::Rename {
                    name: "Atelier".to_owned()
                },
                WorkspaceSettingsEffect::Close,
            ]
        );
        assert_eq!(
            state.presentation().phase(),
            tilo_sheets::Phase::Leaving,
            "the card leaves the screen; the shell hears it is gone once it has"
        );
    }

    phase escape_with_nothing_typed_closes;
    {
        let (mut app, state) = mounted("general");
        app.harness().key("Escape");
        app.settle();
        assert_eq!(state.take_asked(), [WorkspaceSettingsEffect::Close]);
    }

    phase a_viewer_cannot_rename;
    {
        let (mut app, state) = mounted("general-viewer");
        app.harness()
            .perform_named("ws-general-name", &A11yAction::SetValue("Mine".to_owned()))
            .ok();
        app.settle();
        press(&mut app, "ws-settings.secondary");
        assert_eq!(state.take_asked(), [WorkspaceSettingsEffect::Close]);
    }

    phase a_name_the_person_has_not_touched_follows_the_workspace;
    {
        let (mut app, state) = mounted("general");
        let mut renamed = state.settings();
        renamed.name = "Atelier".to_owned();
        state.show(renamed);
        app.settle();
        assert_eq!(state.typed_name(), "Atelier");
        assert!(says(&mut app, "Atelier"));
    }

    phase an_agent_changes_the_workspace_through_the_same_api;
    {
        let (mut app, state) = mounted("general");
        app.effect(&WorkspaceSettingsEffect::Move { step: -1 })
            .unwrap();
        app.settle();
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::Move { step: -1 }]
        );
    }

    phase switching_tabs_asks_the_shell;
    {
        let (mut app, state) = mounted("general");
        app.effect(&WorkspaceSettingsEffect::ShowTab {
            tab: "people".to_owned(),
        })
        .unwrap();
        app.settle();
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::ShowTab {
                tab: "people".to_owned()
            }]
        );
    }

    phase deleting_the_workspace_asks_first_and_keep_changes_nothing;
    {
        let (mut app, state) = mounted("general");
        press(&mut app, "ws-general-delete");
        assert!(says(&mut app, "Delete Design studio?"));
        assert!(says(&mut app, "Everything in this workspace goes with it"));
        assert!(
            state.take_asked().is_empty(),
            "nothing is asked of the shell yet"
        );
        app.harness().key("Escape");
        app.settle();
        assert!(state.take_asked().is_empty(), "keeping changes nothing");
        assert!(says(&mut app, "At a glance"), "the tab is back");
        assert!(state.presentation().is_open());
    }

    phase a_confirmed_delete_asks_the_shell_for_the_workspace;
    {
        let (mut app, state) = mounted("general");
        press(&mut app, "ws-general-delete");
        press(&mut app, "ws-settings-decision.primary");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::DeleteWorkspace]
        );
    }

    phase the_delete_question_renders_for_captures;
    {
        let mut app = staged("general-deleting");
        assert!(says(&mut app, "Delete Design studio?"));
        assert!(says(&mut app, "Keep"));
    }
}

tilo_test_env::scenario! {
    /// The servers keeping the workspace, which its owners alone choose.
    fn the_general_tab_lists_the_servers_keeping_the_workspace;

    phase an_owner_sees_the_servers_keeping_the_workspace;
    {
        let mut owner = staged("general");
        // Each server by its own name, the one the page is connected to saying so once.
        for text in ["Servers", "eu1.tilo.sh", "You're connected here", "Ada's laptop"] {
            assert!(says(&mut owner, text), "{text:?} is shown");
        }
        assert!(!says(&mut owner, "This server"), "no server reads by a stand-in name twice");
        // Only a server other than the one the page is connected to is let go here.
        assert_eq!(
            owner
                .harness()
                .a11y_nodes()
                .iter()
                .filter(|(_, node)| node.label() == Some("Remove"))
                .count(),
            1,
            "one remove button, for the server that is not the one it was made on"
        );

        let mut viewer = staged("general-viewer");
        assert!(!says(&mut viewer, "Servers"), "a viewer sees no servers");
    }

    phase removing_a_server_asks_the_shell_for_it;
    {
        let (mut app, state) = mounted("general");
        press(&mut app, "ws-general-server-remove-8d4e0a11");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::RemoveServer {
                server: "8d4e0a11".to_owned()
            }]
        );
    }

    phase a_typed_site_is_asked_for_by_add_and_by_enter;
    {
        let (mut app, state) = mounted("general");
        app.harness()
            .perform_named(
                "ws-general-server-host",
                &A11yAction::SetValue("{\"origin\":\"https://tilo.sh\"}".to_owned()),
            )
            .unwrap();
        app.settle();
        press(&mut app, "ws-general-server-add");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::AddServer {
                host: "{\"origin\":\"https://tilo.sh\"}".to_owned()
            }]
        );

        app.harness()
            .perform_named(
                "ws-general-server-host",
                &A11yAction::SetValue("{\"origin\":\"https://eu1.tilo.sh\"}".to_owned()),
            )
            .unwrap();
        app.settle();
        app.harness()
            .perform_named("ws-general-server-host", &A11yAction::Focus)
            .unwrap();
        app.harness().key("Enter");
        app.settle();
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::AddServer {
                host: "{\"origin\":\"https://eu1.tilo.sh\"}".to_owned()
            }]
        );
    }
}

tilo_test_env::scenario! {
    /// The People tab of workspace settings.
    fn the_people_tab_lists_members_and_asks_before_removing_one;

    phase the_people_tab_lists_everyone_with_what_the_viewer_may_change;
    {
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




    phase the_holders_of_the_workspace_say_what_each_can_read;
    {
        let mut app = staged("people");
        for text in [
            "Where it is kept",
            "Kitchen NAS",
            "Reads the workspace",
            "tilo.sh",
            "Reads the workspace, and so can the cloud provider",
            "Backup box",
            "Keeps an encrypted copy and reads none of it",
        ] {
            assert!(says(&mut app, text), "{text:?} is shown");
        }
        assert!(
            !says(&mut app, "Keeps an encrypted copy and reads none of it, and so can the cloud provider"),
            "a store reads nothing, whatever its machine"
        );

        // A workspace held by a store, a server of the person's own and a
        // keeper on a rented machine: three ways of holding it, in a person's
        // words.
        let mut settings = staged_settings("people");
        settings.people.holders = vec![
            tilo_sheets::workspace_settings::Holder {
                name: "Kitchen NAS".to_owned(),
                can_read: "Reads the workspace".to_owned(),
            },
            tilo_sheets::workspace_settings::Holder {
                name: "tilo.sh".to_owned(),
                can_read: "Reads the workspace, and so can the cloud provider".to_owned(),
            },
            tilo_sheets::workspace_settings::Holder {
                name: "Backup box".to_owned(),
                can_read: "Keeps an encrypted copy and reads none of it".to_owned(),
            },
        ];
        let (mut app, _state) = mounted_with(settings);
        assert!(says(&mut app, "Kitchen NAS"));
        assert!(says(
            &mut app,
            "Reads the workspace, and so can the cloud provider"
        ));
        assert!(says(
            &mut app,
            "Keeps an encrypted copy and reads none of it"
        ));
    }

    phase removing_someone_asks_first_and_keep_changes_nothing;
    {
        let (mut app, state) = mounted("people");
        press(&mut app, "ws-people-remove-ada");
        assert!(says(&mut app, "Remove this person?"));
        assert!(says(&mut app, "Ada Lovelace-Byron"));
        assert!(
            state.take_asked().is_empty(),
            "nothing is asked of the shell yet"
        );
        app.harness().key("Escape");
        app.settle();
        assert!(state.take_asked().is_empty(), "keeping changes nothing");
        assert!(says(&mut app, "3 members"), "the tab is back");
        assert!(state.presentation().is_open());
    }

    phase a_confirmed_removal_asks_the_shell_for_that_member;
    {
        let (mut app, state) = mounted("people");
        press(&mut app, "ws-people-remove-bo");
        press(&mut app, "ws-settings-decision.primary");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::RemoveMember {
                member: "bo".to_owned()
            }]
        );
    }

    phase the_question_leads_to_the_activity_instead;
    {
        let (mut app, state) = mounted("people");
        press(&mut app, "ws-people-remove-bo");
        press(&mut app, "ws-settings-decision.tertiary");
        assert_eq!(state.take_asked(), [WorkspaceSettingsEffect::ReviewActivity]);
    }

    phase the_removal_question_renders_for_captures;
    {
        let mut app = staged("people-removing");
        assert!(says(&mut app, "Remove this person?"));
        assert!(says(&mut app, "Keep access"));
    }

    phase an_agent_changing_a_role_goes_through_the_api;
    {
        let (mut app, state) = mounted("people");
        app.effect(&WorkspaceSettingsEffect::SetRole {
            member: "bo".to_owned(),
            role: "editor".to_owned(),
        })
        .unwrap();
        app.settle();
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::SetRole {
                member: "bo".to_owned(),
                role: "editor".to_owned(),
            }]
        );
    }
}

tilo_test_env::scenario! {
    /// The Agents tab of workspace settings, and the review of one agent.
    fn the_agents_tab_lists_agents_and_bots_and_reviews_an_agent;

    phase the_agents_tab_lists_agents_and_bots_apart;
    {
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

    phase agent_and_bot_controls_name_what_they_act_on;
    {
        let (mut app, state) = mounted("agents");
        press(&mut app, "ws-agents-review-claude");
        // One button opens the form; a press with no name in it asks nothing.
        press(&mut app, "ws-agents-new");
        press(&mut app, "ws-agents-create");
        assert_eq!(
            state.take_asked(),
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
        press(&mut app, "ws-agents-create");
        press(&mut app, "ws-bots-add");
        assert_eq!(
            state.take_asked(),
            [
                WorkspaceSettingsEffect::AddAgent {
                    name: "Analyst".to_owned(),
                    model: String::new(),
                },
                WorkspaceSettingsEffect::AddBot,
            ]
        );
    }

    phase choosing_an_answer_far_down_keeps_the_sheet_where_it_was;
    // A choice made far down a long review rebuilds the sheet with the new answer: it stays where
    // the person was, rather than jumping back to the top.
    {
        let mut settings = staged_settings("agent-review");
        let review = settings.agents.review.as_mut().unwrap();
        let first = review.apps[0].clone();
        review.apps = (0..12)
            .map(|at| {
                let mut app = first.clone();
                app.scope = format!("app-{at}");
                app.label = format!("App {at}");
                app
            })
            .collect();
        let (mut app, state) = mounted_with(settings.clone());
        let row = "ws-agent-answer-app-9-change";
        let at = |app: &mut HeadlessApp<WorkspaceSettingsApp>| {
            app.harness().targets().get(row).map(|bounds| bounds.y0)
        };
        let top = at(&mut app).expect("the row is laid out");
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(640.0, 450.0));
        for _ in 0..6 {
            app.harness().scroll(0.0, -3.0);
        }
        app.settle();
        let scrolled = at(&mut app).expect("still laid out");
        assert!(scrolled < top - 100.0, "the review scrolls: {top} to {scrolled}");
        // The shell shows the answer chosen: the same page with one answer changed.
        let review = settings.agents.review.as_mut().unwrap();
        review.apps[9].kinds[1].answer = 2;
        state.show(settings);
        app.settle();
        let after = at(&mut app).expect("still laid out");
        assert!(
            (after - scrolled).abs() < 1.0,
            "the sheet stays where it was: {scrolled} then {after}"
        );
    }

    phase a_requested_bot_is_removed_here_and_a_hosts_bot_is_not;
    // Remove takes back a bot a member asked for, and the tab offers no Remove
    // for one the workspace's host runs from its own choice.
    {
        let (mut app, state) = mounted("agents");
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
            state.take_asked(),
            [WorkspaceSettingsEffect::RemoveBot {
                bot: "so.tilo.bots.standup".to_owned()
            }]
        );
    }

    phase the_moderation_switch_is_offered_only_to_who_may_turn_it;
    // Someone who may grant turns the server's moderation agent with its switch;
    // anyone else reads whether it is on.
    {
        let (mut app, state) = mounted("agents");
        assert!(says(&mut app, "Answers reports in Chat"));
        press(&mut app, "ws-bots-moderation-switch");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::SetModeration { on: false }]
        );
        let mut settings = staged_settings("agents");
        let moderation = settings.agents.moderation.as_mut().unwrap();
        moderation.switchable = false;
        let (mut app, _) = mounted_with(settings);
        assert!(
            !app.harness()
                .targets()
                .contains_key("ws-bots-moderation-switch"),
            "no switch for someone who may not grant"
        );
        assert!(says(&mut app, "On"));
    }

    phase an_agent_review_shows_its_answers_model_and_exceptions;
    {
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

    phase every_app_offers_each_answer_as_a_choice;
    // Every app shows its answers as choices, with no text to open them first.
    {
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

    phase an_outside_effect_offers_only_the_answers_it_may_be_given;
    // An agent never carries out an outside effect unasked: its line offers Ask
    // first and Deny, never an Allow the workspace would then refuse.
    {
        let (mut app, state) = mounted("agent-review");
        let nodes: std::collections::HashMap<_, _> =
            app.harness().a11y_nodes().into_iter().collect();
        let offered = |name: &str| -> Vec<String> {
            let (_, group) = nodes
                .iter()
                .find(|(_, node)| node.label() == Some(name))
                .unwrap_or_else(|| panic!("{name:?} is shown"));
            group
                .children()
                .iter()
                .filter_map(|child| nodes.get(child)?.label().map(str::to_owned))
                .collect()
        };
        assert_eq!(offered("Notes Reading"), ["Allow", "Ask first", "Deny"]);
        assert_eq!(offered("Notes Outside effects"), ["Ask first", "Deny"]);
        press(&mut app, "ws-agent-answer-so.tilo.notes-outside:segment:0");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::SetAgentAnswer {
                agent: "claude".to_owned(),
                scope: "so.tilo.notes".to_owned(),
                kind: "outside".to_owned(),
                answer: "ask".to_owned(),
            }],
            "its first choice is Ask first"
        );
    }

    phase a_line_with_fewer_answers_keeps_the_places_of_the_ones_it_has;
    // Ask first and Deny stand where they stand in a line offering Allow too, so the answers
    // line up under each other down the review.
    {
        let (mut app, _) = mounted("agent-review");
        let segments = |app: &mut HeadlessApp<WorkspaceSettingsApp>, name: &str| {
            let nodes: std::collections::HashMap<_, _> =
                app.harness().a11y_nodes().into_iter().collect();
            let (_, group) = nodes
                .iter()
                .find(|(_, node)| node.label() == Some(name))
                .unwrap_or_else(|| panic!("{name:?} is shown"));
            group
                .children()
                .iter()
                .filter_map(|child| nodes.get(child)?.bounds())
                .collect::<Vec<_>>()
        };
        let two = segments(&mut app, "Notes Outside effects");
        let three = segments(&mut app, "Notes Reading");
        assert_eq!((two.len(), three.len()), (2, 3), "each answer is placed");
        for (offered, held) in two.iter().zip(&three[1..]) {
            assert!(
                (offered.x0 - held.x0).abs() < 1.0 && (offered.x1 - held.x1).abs() < 1.0,
                "Ask first and Deny keep their places: {offered:?} then {held:?}"
            );
        }
    }

    phase an_account_rows_outside_line_says_what_those_effects_are;
    // Outside effects under the person's own account rows name the actions they cover, which
    // their row names alone do not say.
    {
        let mut settings = staged_settings("agent-review");
        settings.agents.review.as_mut().unwrap().apps[0].kinds[2].caption =
            Some("Creating and deleting your workspaces.".to_owned());
        let (mut app, _) = mounted_with(settings);
        assert!(
            says(&mut app, "Creating and deleting your workspaces."),
            "the line says what its outside effects are"
        );
    }

    phase in_a_review_escape_goes_back_to_the_agents;
    {
        let (mut app, state) = mounted("agent-review");
        app.harness().key("Escape");
        app.settle();
        assert_eq!(state.take_asked(), [WorkspaceSettingsEffect::EndReview]);
        assert!(state.presentation().is_open(), "the sheet stays up");
    }

    phase a_review_asks_for_answers_models_and_resets_by_key;
    {
        let (mut app, state) = mounted("agent-review");
        press(&mut app, "ws-agent-reset-so.tilo.notes");
        press(
            &mut app,
            "ws-agent-stop-so.tilo.notes/notes.notebook/delete-outside",
        );
        press(&mut app, "ws-agent-use-");
        press(&mut app, "ws-agent-retire");
        assert_eq!(
            state.take_asked(),
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

    phase an_agents_day_is_read_on_its_row_and_set_in_its_review;
    // What an agent spends and may spend reads on its row; a member the workspace
    // lets sets the day from its review, where a typed amount is taken by Set and by Enter.
    {
        let (mut app, _) = mounted("agents");
        assert!(
            says(&mut app, "$0.42 of $5 today"),
            "the row counts the day on its line under the name, under who added it"
        );
        assert!(
            says(&mut app, "$1.20 today"),
            "an unmetered agent says what it spent on that line too"
        );

        let (mut app, state) = mounted("agent-review");
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
            state.take_asked(),
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
            state.take_asked(),
            [WorkspaceSettingsEffect::SetAgentSpendLimit {
                agent: "claude".to_owned(),
                dollars: String::new(),
            }]
        );
    }

    phase a_review_offers_the_day_only_to_whom_the_workspace_lets_set_it;
    // What an agent may spend a day is set by whom the workspace lets, as its model
    // is: a review someone else opens says who sets it and offers no field.
    {
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
        limit.note = "Only editors set what it may spend a day.".to_owned();
        let state = WorkspaceSettingsState::default();
        state.show(settings);
        state.presentation().open(false);
        let mut app =
            HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", state).unwrap();
        app.harness().resize(VIEWPORT).unwrap();
        app.settle();

        assert!(says(&mut app, "Only editors set what it may spend a day."));
        assert!(
            app.harness()
                .perform_named("ws-agent-limit-field", &A11yAction::Focus)
                .is_err(),
            "someone the workspace does not let is offered no field"
        );
    }

    phase a_review_offers_the_model_only_to_whom_the_workspace_lets_choose_it;
    // The model an agent plans with is chosen by whom the workspace lets: a review
    // someone else opens says who chooses and offers no Use.
    {
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
        models.note = Some("Only editors choose the model it runs on".to_owned());
        let state = WorkspaceSettingsState::default();
        state.show(settings);
        state.presentation().open(false);
        let mut app =
            HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", state).unwrap();
        app.harness().resize(VIEWPORT).unwrap();
        app.settle();

        assert!(
            says(&mut app, "Only editors choose the model it runs on"),
            "the review says who chooses"
        );
        let uses = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some("Use"))
            .count();
        assert_eq!(uses, 0, "no model is offered to someone the workspace does not let");
    }

    phase an_agent_is_answered_through_the_api_too;
    {
        let (mut app, state) = mounted("agent-review");
        app.effect(&WorkspaceSettingsEffect::SetAgentAnswer {
            agent: "claude".to_owned(),
            scope: "so.tilo.calendar".to_owned(),
            kind: "change".to_owned(),
            answer: "deny".to_owned(),
        })
        .unwrap();
        app.settle();
        assert_eq!(state.take_asked().len(), 1);
    }
}

tilo_test_env::scenario! {
    /// The Permissions tab of workspace settings.
    fn the_permissions_tab_grants_and_asks_before_revoking;

    phase the_permissions_tab_groups_what_is_given_by_who_holds_it;
    {
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

    phase granting_names_who_what_and_for_how_long;
    {
        let (mut app, state) = mounted("permissions");
        press(&mut app, "ws-grant");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::Grant {
                subject: "member:ada".to_owned(),
                permission: "network".to_owned(),
                until: None,
            }]
        );
    }

    phase a_permission_until_a_time_waits_for_the_day_and_time_it_ends;
    {
        let (mut app, state) = mounted("permissions");
        press(&mut app, "ws-grant-expiry:segment:1");
        let _ = app
            .harness()
            .perform_named("ws-grant", &A11yAction::Activate);
        app.settle();
        assert!(
            state.take_asked().is_empty(),
            "nothing is given before the person picks when it ends"
        );
    }

    phase revoking_asks_first;
    {
        let (mut app, state) = mounted("permissions");
        press(&mut app, "ws-permission-revoke-grant-2");
        assert!(says(&mut app, "Take back this permission?"));
        assert!(state.take_asked().is_empty());
        press(&mut app, "ws-settings-decision.primary");
        assert_eq!(
            state.take_asked(),
            [WorkspaceSettingsEffect::Revoke {
                grant: "grant-2".to_owned()
            }]
        );
    }

    phase the_revoke_question_renders_for_captures;
    {
        let mut app = staged("permissions-revoking");
        assert!(says(&mut app, "Keep permission"));
    }
}

tilo_test_env::scenario! {
    /// The Apps tab of workspace settings.
    fn the_apps_tab_shows_and_moves_on_each_default;

    phase the_apps_tab_shows_both_choices_for_each_kind;
    {
        let mut app = staged("apps");
        for text in [".md files", "Notes", "mailto: links", "Mail"] {
            assert!(says(&mut app, text), "{text:?} is shown");
        }
        assert!(says(&mut staged("apps-empty"), "No apps open files yet"));
    }

    phase each_choice_moves_on_by_the_kind_it_is_for;
    {
        let (mut app, state) = mounted("apps");
        press(&mut app, "ws-apps-workspace-ext:md");
        press(&mut app, "ws-apps-yours-scheme:mailto");
        assert_eq!(
            state.take_asked(),
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
}

tilo_test_env::scenario! {
    /// The Emoji tab of workspace settings.
    fn the_emoji_tab_lists_adds_and_removes_emoji;

    phase the_emoji_tab_lists_each_emoji_with_who_added_it;
    {
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

    phase emoji_controls_ask_by_name;
    {
        let (mut app, state) = mounted("emoji");
        press(&mut app, "ws-emoji-add");
        press(&mut app, "ws-emoji-remove-party-parrot");
        assert_eq!(
            state.take_asked(),
            [
                WorkspaceSettingsEffect::AddEmoji,
                WorkspaceSettingsEffect::RemoveEmoji {
                    name: "party-parrot".to_owned()
                },
            ]
        );
    }

    phase only_the_emoji_the_person_may_remove_offer_remove;
    {
        let mut app = staged("emoji");
        assert!(says(&mut app, "Remove :party-parrot:"));
        assert!(!says(&mut app, "Remove :shipit:"));
    }
}
