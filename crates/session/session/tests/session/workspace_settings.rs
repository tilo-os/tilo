//! The workspace settings sheet: what its tabs show of the workspace in front, and what a person's
//! choices there change.

use super::*;

/// Who sits here, as the session acts for them.
const PERSON: &str = "person";

/// The workspace in front, which the session has taken as the person's own,
/// with `members` already in it.
fn own_workspace(session: &Session, members: &[&str]) {
    let mut tree = session.handle().read(Clone::clone);
    let workspace = tree.active_workspace_mut().expect("a workspace in front");
    for member in members {
        tilo_desktop::workspace_settings::set_member_role(
            workspace,
            &PERSON.to_owned(),
            &(*member).to_owned(),
            tilo_action_policy::Role::Viewer,
        )
        .expect("the person may admit a member");
    }
    session.handle().replace_tree(tree);
}

/// The sheet as a person presses it: the app the session hosts, lent the seat the session lends its
/// own, so a press arrives as the person's own.
fn sheet(session: &Session) -> HeadlessApp<WorkspaceSettingsApp> {
    let seat = session
        .workspace_settings()
        .expect("workspace settings")
        .clone();
    let mut app =
        HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent(PERSON, seat).expect("the sheet");
    app.harness().resize((1280, 900)).expect("a screen");
    app.settle();
    app
}

/// Write what the sheet shows where a person can look at it, when the run asked for shots:
/// `TILO_SURFACE_SHOTS` names where they go.
fn capture(app: &mut HeadlessApp<WorkspaceSettingsApp>, name: &str) {
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(std::path::PathBuf::from)
    else {
        return;
    };
    app.settle();
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    app.harness()
        .render_png(&directory.join(format!("{name}.png")));
}

/// The Agents tab as the sheet shows it now.
fn agents_shown(session: &Session) -> tilo_sheets::workspace_settings::Agents {
    session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .agents
}

/// The agent `key` as the Agents tab lists it.
fn listed_agent(session: &Session, key: &str) -> tilo_sheets::workspace_settings::Agent {
    agents_shown(session)
        .agents
        .into_iter()
        .find(|agent| agent.key == key)
        .expect("the sheet lists the agent")
}

/// The review the Agents tab shows now.
fn shown_review(session: &Session) -> tilo_sheets::workspace_settings::AgentReview {
    agents_shown(session).review.expect("the review is up")
}

/// The app `scope` 's row in the review the Agents tab shows now.
fn listed_app(session: &Session, scope: &str) -> tilo_sheets::workspace_settings::AccessApp {
    shown_review(session)
        .apps
        .into_iter()
        .find(|row| row.scope == scope)
        .expect("the review lists the app")
}

/// The id of the workspace in front.
fn front_workspace(session: &Session) -> String {
    session.handle().read(|tree| {
        tree.active_workspace()
            .expect("a workspace in front")
            .id()
            .to_string()
    })
}

/// Press the control `id` of `app` , as a person's own gesture.
fn press(app: &mut HeadlessApp<WorkspaceSettingsApp>, id: &str) {
    app.harness()
        .perform_named(id, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("press {id}: {error}"));
    app.settle();
}

/// Activate the element `name` says, as a screen reader does: the menu items a control opens have
/// no id of their own, only what they say.
fn activate(app: &mut HeadlessApp<WorkspaceSettingsApp>, name: &str) {
    let node = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("no element says {name:?}"));
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("activate {name}: {error}"));
    app.settle();
}

/// Choose what the control `name` says at `choice` , as a screen reader does: the choice is one
/// segment of the control.
fn choose(app: &mut HeadlessApp<WorkspaceSettingsApp>, name: &str, choice: &str) {
    let nodes = app.harness().a11y_nodes();
    let group = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("no control says {name:?}"));
    let children: Vec<tilo_app::accesskit::NodeId> = nodes
        .iter()
        .find(|(id, _)| *id == group)
        .map(|(_, node)| node.children().to_vec())
        .unwrap_or_default();
    let segment = nodes
        .iter()
        .find_map(|(id, node)| {
            (children.contains(id) && node.label() == Some(choice)).then_some(*id)
        })
        .unwrap_or_else(|| {
            let under: Vec<Option<&str>> = nodes
                .iter()
                .filter(|(id, _)| children.contains(id))
                .map(|(_, node)| node.label())
                .collect();
            panic!("{name:?} offers no {choice:?}; it offers {under:?}");
        });
    app.harness()
        .perform_a11y_action(segment, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("choose {choice:?} of {name:?}: {error}"));
    app.settle();
}

/// Show `tab` in the sheet, as pressing its tab does, and let the session answer with the tab it
/// shows.
fn show_tab(
    session: &mut Session,
    app: &mut HeadlessApp<WorkspaceSettingsApp>,
    tab: &WorkspaceSettingsEffect,
) {
    app.effect(tab).expect("show the tab");
    settle(session, app);
}

/// The desk's search, as it publishes it while it is open.
fn search_open(session: &Session) -> Option<ResourceSpec> {
    fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find(&resource.children, id)
            }
        })
    }
    find(
        &session.resources(),
        tilo_desktop::desk_verbs::SEARCH_RESOURCE,
    )
}

/// The People tab gives each member another role, and the workspace holds both afterwards, with the
/// sheet's rows saying so.
#[test]
fn workspace_settings_sets_each_members_role() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada", "bo"]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "people".to_owned(),
        },
    );
    for (member, role) in [("ada", "Editor"), ("bo", "Commenter")] {
        press(&mut app, &format!("ws-people-role-{member}"));
        activate(&mut app, role);
        session.step(Duration::from_millis(1)).unwrap();
        app.settle();
    }
    let roles = session.handle().read(|tree| {
        let workspace = tree.active_workspace().expect("a workspace in front");
        (workspace.role_of("ada"), workspace.role_of("bo"))
    });
    assert_eq!(
        roles,
        (
            Some(tilo_action_policy::Role::Editor),
            Some(tilo_action_policy::Role::Commenter)
        ),
        "the workspace holds each member's new role"
    );
    let listed = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .people
        .people;
    for (member, key) in [("ada", "editor"), ("bo", "commenter")] {
        let row = listed
            .iter()
            .find(|person| person.key == member)
            .unwrap_or_else(|| panic!("the sheet lists {member}"));
        assert_eq!(
            row.role_key.as_deref(),
            Some(key),
            "the sheet's row for {member} says which role they have"
        );
    }
}

/// The People tab says what the owner's role is instead of offering it: who owns the workspace is a
/// fact of it, so their row has no role control and no one chooses a new owner here.
#[test]
fn the_people_tab_offers_the_owners_role_as_a_fact() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada"]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "people".to_owned(),
        },
    );
    let owner = session.handle().read(|tree| {
        tree.active_workspace()
            .and_then(|workspace| workspace.owner().cloned())
    });
    let owner = owner.expect("the workspace has an owner");
    assert!(
        app.harness()
            .perform_named(
                &format!("ws-people-role-{owner}"),
                &ui::a11y::Action::Activate
            )
            .is_err(),
        "the owner's row offers no role to choose"
    );
    assert!(
        app.harness()
            .perform_named("ws-people-ownership", &ui::a11y::Action::Activate)
            .is_err(),
        "the tab offers no ownership control"
    );
    let row = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .people
        .people
        .into_iter()
        .find(|person| person.key == owner)
        .expect("the sheet lists the owner");
    assert_eq!(
        row.offers,
        tilo_sheets::workspace_settings::Offers::Nothing,
        "the owner's row shows their role and offers nothing"
    );
}

/// No tab of workspace settings sends a person to the search: every control there changes the
/// workspace or asks the session for a surface it shows, so none of them opens a search that finds
/// nothing.
#[test]
fn no_workspace_settings_tab_leads_into_the_search() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada"]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    for tab in [
        "general",
        "people",
        "agents",
        "permissions",
        "apps",
        "emoji",
    ] {
        show_tab(
            &mut session,
            &mut app,
            &WorkspaceSettingsEffect::ShowTab {
                tab: tab.to_owned(),
            },
        );
        assert!(
            search_open(&session).is_none(),
            "the {tab} tab opens no search"
        );
    }
    // The two controls that used to, when the governance actions they searched were published, are
    // gone.
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "permissions".to_owned(),
        },
    );
    for gone in ["ws-permissions-roles", "ws-people-ownership"] {
        assert!(
            app.harness()
                .perform_named(gone, &ui::a11y::Action::Activate)
                .is_err(),
            "{gone} is not offered"
        );
    }
}

/// The Permissions tab gives ada a permission beyond her role, and takes it back: the workspace
/// holds it in between, and the sheet lists it.
#[test]
fn workspace_settings_gives_and_revokes_a_permission() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada"]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "permissions".to_owned(),
        },
    );
    press(&mut app, "ws-grant-who");
    activate(&mut app, "ada");
    press(&mut app, "ws-grant");
    settle(&mut session, &mut app);
    let given = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .permissions
        .sections
        .into_iter()
        .flat_map(|section| section.permissions)
        .find(|permission| permission.holder == "ada")
        .expect("the sheet lists ada's permission");
    let camera = tilo_desktop_surface::i18n::t("workspace-settings-scope-camera");
    assert!(
        given.detail.contains(&camera),
        "the row says what the permission allows: {}",
        given.detail
    );
    let held = session
        .handle()
        .read(|tree| tree.active_workspace().expect("a workspace").sandbox.len());
    assert_eq!(held, 1, "the workspace gives ada the camera");

    press(&mut app, &format!("ws-permission-revoke-{}", given.key));
    press(&mut app, "ws-settings-decision.primary");
    settle(&mut session, &mut app);
    let left = session
        .handle()
        .read(|tree| tree.active_workspace().expect("a workspace").sandbox.len());
    assert_eq!(left, 0, "the workspace gives the camera to no one again");
    assert!(
        session
            .workspace_settings()
            .expect("workspace settings")
            .settings()
            .permissions
            .sections
            .into_iter()
            .flat_map(|section| section.permissions)
            .all(|permission| permission.holder != "ada"),
        "the sheet lists no permission of ada's"
    );
}

/// Have `app` hold a capability here, so the workspace shows it and the sheet offers it.
fn app_here(session: &Session, app: &str) {
    let mut tree = session.handle().read(Clone::clone);
    let workspace = tree.active_workspace_mut().expect("a workspace in front");
    workspace.grant_app(
        app,
        &tilo_desktop_model::Capability::Camera {
            scope: tilo_desktop_model::GRANT_SESSION,
        },
    );
    session.handle().replace_tree(tree);
}

/// The agent's answer for one kind of action at one app, as the domain reads the workspace.
fn answer(
    session: &Session,
    agent: &str,
    app: &str,
    kind: tilo_action_policy::ActionKind,
) -> tilo_desktop::workspace_settings::agent_access::AnswerCell {
    session.handle().read(|tree| {
        let workspace = tree.active_workspace().expect("a workspace");
        let admission =
            tilo_action_policy::roles::agent_turn(&workspace.policy.workspace, agent, PERSON, &[])
                .grants;
        tilo_desktop::workspace_settings::agent_access::answer_at(
            workspace,
            agent,
            &admission,
            &tilo_action_policy::agent_answers::AnswerScope::app(app),
            kind,
        )
    })
}

/// Let the session answer an ask the sheet made, and the sheet settle.
fn settle(session: &mut Session, app: &mut HeadlessApp<WorkspaceSettingsApp>) {
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
}

/// Its keeper dresses it from its row: the palette opens under it, and the colour picked is the one
/// its devices draw it in.
fn recolour(session: &mut Session, app: &mut HeadlessApp<WorkspaceSettingsApp>) {
    press(app, "ws-agents-palette-helper");
    settle(session, app);
    activate(app, "Green");
    settle(session, app);
    assert_eq!(
        listed_agent(session, "helper").colour,
        4,
        "the keeper's colour lands in the record"
    );
}

/// This device's Settings offer one model the agents may plan with.
fn settings_offer_a_model() {
    let profile = "local".to_owned();
    let config = agent_provider_config::ProviderConfig {
        profiles: vec![agent_provider_config::ProviderProfile {
            id: profile.clone(),
            label: "Anthropic".to_owned(),
            models: vec![agent_provider_config::ModelDescriptor {
                id: "sonnet".to_owned(),
                label: "Sonnet".to_owned(),
                ..agent_provider_config::ModelDescriptor::default()
            }],
            default_model: Some("sonnet".to_owned()),
            ..agent_provider_config::ProviderProfile::local_vllm()
        }],
        default_profile: Some(profile),
        ..agent_provider_config::ProviderConfig::default()
    };
    config
        .save()
        .expect("save the providers this device offers");
}

/// The Agents tab shows an agent's answers, changes one, returns them to what its admission says,
/// chooses its model and takes the agent out of the workspace.
#[test]
fn workspace_settings_answers_for_an_agent_and_takes_it_out() {
    use tilo_action_policy::ActionKind;
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    settings_offer_a_model();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    app_here(&session, "so.tilo.notes");
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "agents".to_owned(),
        },
    );
    app.effect(&WorkspaceSettingsEffect::AddAgent {
        name: "helper".to_owned(),
        model: String::new(),
        colour: "violet".to_owned(),
    })
    .expect("the person makes an agent");
    settle(&mut session, &mut app);
    let row = listed_agent(&session, "helper");
    assert_eq!(
        row.name, "helper",
        "the row names the agent the record keeps"
    );
    assert_eq!(
        row.status, "Kept by you",
        "the row says who keeps it, in one line"
    );
    assert_eq!(row.model, "Model: No model chosen in Settings");
    assert!(
        row.mine,
        "the person here keeps it, so its colour is theirs"
    );
    assert_eq!(row.colour, 9, "it is drawn in the violet its maker picked");
    recolour(&mut session, &mut app);
    press(&mut app, "ws-agents-review-helper");
    settle(&mut session, &mut app);
    let review = shown_review(&session);
    assert!(
        review
            .apps
            .iter()
            .any(|row| row.scope == "app:so.tilo.notes"),
        "the review lists the app: {review:?}"
    );
    choose(&mut app, "Notes Changes", "Ask first");
    settle(&mut session, &mut app);
    let setting = answer(&session, "helper", "so.tilo.notes", ActionKind::Undoable);
    assert!(setting.explicit, "the person's answer is the agent's own");
    assert_eq!(setting.answer, tilo_action_policy::Answer::Ask);
    assert!(
        listed_app(&session, "app:so.tilo.notes").customised,
        "the review says the app was customised"
    );

    // The model it plans with is the keeper's to choose, from this device's Settings, and the
    // review says it chose.
    assert!(
        review
            .models
            .as_ref()
            .is_some_and(|models| models.can_choose),
        "the person who keeps it chooses the model"
    );
    press(&mut app, "ws-agent-use-local/sonnet");
    settle(&mut session, &mut app);
    let chosen = shown_review(&session)
        .models
        .expect("the review offers models");
    assert_eq!(chosen.summary, "Sonnet from Anthropic");
    capture(&mut app, "workspace-settings-agent-review");

    activate(&mut app, "Use defaults");
    settle(&mut session, &mut app);
    let back = answer(&session, "helper", "so.tilo.notes", ActionKind::Undoable);
    assert!(!back.explicit, "the app follows the admission again");
    assert_eq!(
        back.answer,
        tilo_action_policy::Answer::Allow,
        "the turn's cap admits changes in the app"
    );

    activate(&mut app, "Remove");
    settle(&mut session, &mut app);
    let after = agents_shown(&session);
    assert!(after.review.is_none(), "retiring ends the review");
    assert!(after.agents.is_empty(), "the sheet lists no agent again");
    let held = session.handle().read(|tree| {
        tree.active_workspace()
            .unwrap()
            .agent_members_at(tilo_clock::unix_now())
            .len()
    });
    assert_eq!(held, 0, "the workspace holds no agent again");
}

/// An app that opens `kind` here.
fn opens(package_id: &str, name: &str, kind: &str) -> desktop_policy::AppCandidate {
    desktop_policy::AppCandidate {
        package_id: package_id.to_owned(),
        display_name: name.to_owned(),
        resources: std::iter::once(
            desktop_policy::ResourceKind::parse(kind).expect("a kind of file or link"),
        )
        .collect(),
        secondary: false,
    }
}

/// The workspace's choice of default app, and the person's own: pressing
/// the Apps tab's control moves each to the next app that opens the kind,
/// and the workspace keeps what was pressed.
#[test]
fn workspace_settings_pins_an_app_for_the_workspace() {
    use desktop_policy::{ResourceKind, UserWorkspaceChoice, WorkspaceDefaultPolicy};
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_lists(tilo_session::AppLists {
        candidates: vec![
            opens("so.tilo.notes", "Notes", "ext:md"),
            opens("so.tilo.writer", "Writer", "ext:md"),
        ],
        ..tilo_session::AppLists::default()
    });
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "apps".to_owned(),
        },
    );
    let md = ResourceKind::parse("ext:md").expect("a kind of file");
    let shown = |session: &Session| {
        session
            .workspace_settings()
            .expect("workspace settings")
            .settings()
            .apps
            .rows
            .into_iter()
            .find(|row| row.resource == "ext:md")
            .expect("the tab lists .md files")
    };
    press(&mut app, "ws-apps-workspace-ext:md");
    settle(&mut session, &mut app);
    let policy = session.handle().read(|tree| {
        tree.active_workspace()
            .expect("a workspace")
            .default_apps
            .policy
            .get(&md)
            .cloned()
    });
    assert_eq!(
        policy,
        Some(WorkspaceDefaultPolicy::App("so.tilo.notes".to_owned())),
        "the workspace opens .md in Notes"
    );
    assert_eq!(shown(&session).workspace, "Notes", "the tab says so");

    press(&mut app, "ws-apps-workspace-ext:md");
    settle(&mut session, &mut app);
    let policy = session.handle().read(|tree| {
        tree.active_workspace()
            .expect("a workspace")
            .default_apps
            .policy
            .get(&md)
            .cloned()
    });
    assert_eq!(
        policy,
        Some(WorkspaceDefaultPolicy::App("so.tilo.writer".to_owned())),
        "a second press moves to the next app that opens .md"
    );

    press(&mut app, "ws-apps-yours-ext:md");
    settle(&mut session, &mut app);
    let choice = session.handle().read(|tree| {
        tree.active_workspace()
            .expect("a workspace")
            .default_apps
            .user_choices
            .get(PERSON)
            .and_then(|choices| choices.get(&md))
            .cloned()
    });
    assert_eq!(
        choice,
        Some(UserWorkspaceChoice::PersonalDefault),
        "the person chose their own default for .md here"
    );
}

/// What Chat's record of `workspace` 's emoji holds, as the device's store keeps it: read without
/// taking its writer (Chat owns the record), the changes a window made committed as the store
/// writes them.
/// Read on a thread of its own: the session's loop never waits on storage and is told "not yet"
/// instead, while this asks the disk itself, as the session's own reader of other apps' records
/// does.
fn chat_emoji(session: &Session, workspace: &str) -> Option<Vec<String>> {
    let stored = session.stored_record(
        tilo_app::tilo_workspace_emoji::OWNER,
        workspace,
        &collab_types::workspace_record_id(tilo_app::tilo_workspace_emoji::KIND),
    )?;
    // A window appends to the record as it changes it, and this read may land on a tail it has not
    // finished writing: what came before it is the record as the last finished write left it, which
    // is what Chat loads.
    let options = automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore);
    let doc = automerge::AutoCommit::load_with_options(&stored, options).ok()?;
    let state =
        tilo_app::tilo_resource::Reader::<tilo_app::tilo_workspace_emoji::WorkspaceEmoji>::read(
            &doc,
        )
        .ok()?;
    Some(
        state
            .state()
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
}

/// The Emoji tab adds a picture the person chooses to the workspace's emoji,
/// where Chat's record holds it, and removes it from there again.
#[test]
fn workspace_settings_adds_and_removes_the_workspaces_emoji() {
    let _settled = ui::motion::testing::settled();
    // The device store every app's records are kept in, here a home of the test's own: what the tab
    // adds reaches no other test's workspace.
    let home = tilo_test_env::TestHome::new().expect("a home of its own");
    let picture = home.home().join("wave.png");
    image::save_buffer(
        picture.as_path(),
        &[
            255, 204, 0, 255, 255, 204, 0, 255, 255, 204, 0, 255, 255, 204, 0, 255,
        ],
        2,
        2,
        image::ColorType::Rgba8,
    )
    .expect("a picture to add");
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .expect("a workspace in front")
            .id()
            .to_string()
    });
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "emoji".to_owned(),
        },
    );

    // Add emoji asks the session's chooser for a picture.
    press(&mut app, "ws-emoji-add");
    session.step(Duration::from_millis(1)).unwrap();
    let picker = session.file_picker().expect("the file chooser").clone();
    let request = picker.request().expect("the tab asks for a picture");
    pump(&mut session, &mut [], |_, _| {
        let listing = picker.listing();
        !listing.reading && listing.entries.iter().any(|entry| entry.name == "wave.png")
    });
    let select = FilePickerEffect::SELECT.id;
    // The chooser's own resource: "select" is a control of the sheet's tabs as well, and the action
    // is taken by the app that publishes it.
    let chooser = publishers(&session, select)
        .into_iter()
        .find(|resource| resource.starts_with("file-chooser"))
        .expect("the chooser publishes choosing");
    session
        .invoke(
            &chooser,
            select,
            &arguments(serde_json::json!({
                "handle": request.handle,
                "path": picture.to_string_lossy(),
            })),
        )
        .expect("select");
    pump(&mut session, &mut [], |_, _| picker.listing().can_confirm);
    session
        .invoke(
            &chooser,
            FilePickerEffect::CONFIRM.id,
            &arguments(serde_json::json!({ "handle": request.handle })),
        )
        .expect("confirm");

    // The tab shows the emoji, and Chat's record holds it: the store commits what a window changed
    // after a debounce, so what the device holds is read again until the emoji is in it.
    pump(&mut session, &mut [], |_, _| {
        app.settle();
        app.harness().a11y_node_named(":wave:").is_some()
    });
    let held = held_by_chat(&mut session, &workspace, |held| {
        held.is_some_and(|names| names.iter().any(|name| name == "wave"))
    });
    assert!(
        held.as_deref()
            .is_some_and(|names| names.iter().any(|name| name == "wave")),
        "Chat's record holds the emoji the tab added: {held:?}"
    );

    // Remove takes it out of the tab and out of Chat's record.
    press(&mut app, "ws-emoji-remove-wave");
    session.step(Duration::from_millis(1)).unwrap();
    pump(&mut session, &mut [], |_, _| {
        app.settle();
        app.harness().a11y_node_named(":wave:").is_none()
    });
    let held = held_by_chat(&mut session, &workspace, |held| {
        held.is_some_and(|names| !names.iter().any(|name| name == "wave"))
    });
    assert!(
        held.as_deref()
            .is_some_and(|names| !names.iter().any(|name| name == "wave")),
        "removing the emoji in the tab takes it out of Chat's record: {held:?}"
    );
}

/// What Chat's record of `workspace` holds once `done` says it does, stepping the session
/// meanwhile: the store commits a window's change after a debounce.
fn held_by_chat(
    session: &mut Session,
    workspace: &str,
    mut done: impl FnMut(Option<&[String]>) -> bool,
) -> Option<Vec<String>> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = chat_emoji(session, workspace);
        if done(held.as_deref()) || std::time::Instant::now() >= deadline {
            return held;
        }
        session.step(Duration::from_millis(100)).expect("step");
    }
}

/// Every name the sheet says, with what it says of it, as a screen reader reads it.
fn said(app: &mut HeadlessApp<WorkspaceSettingsApp>) -> Vec<(String, String)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| {
            node.label().map(|label| {
                (
                    label.to_owned(),
                    node.value().unwrap_or_default().to_owned(),
                )
            })
        })
        .collect()
}

/// The General tab says what the workspace is: its name under its label,
/// inviting people, the workspace at a glance, the devices it shows on and
/// leaving it.
#[test]
fn workspace_settings_general_says_what_the_workspace_is() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada"]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    let labels = said(&mut app);
    for wanted in [
        "Workspace identity",
        "Workspace name",
        "Invite people",
        "Create invite",
        "At a glance",
        "Your devices",
        "Leave workspace",
        "Leave",
    ] {
        assert!(
            labels.iter().any(|(label, _)| label == wanted),
            "the General tab says {wanted:?}: {labels:?}"
        );
    }
    assert!(
        labels
            .iter()
            .any(|(label, value)| label == "members" && value == "2"),
        "the tab says how many share the workspace: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|(label, _)| label.contains("stay on each device")),
        "nothing says the arrangement stays on one device: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|(label, _)| label == "Untitled" || label == "Ohne Titel"),
        "every row is named: {labels:?}"
    );
}

/// Leaving from the General tab takes the workspace off this device's desk,
/// as the same choice in the workspace menu does.
#[test]
fn workspace_settings_general_leaves_the_workspace() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    // Leaving is for a workspace that stays with the people in it: the new one has someone else in
    // it, so leaving it means something.
    own_workspace(&session, &["ada"]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    assert_eq!(
        session.handle().read(|tree| tree.workspaces.len()),
        2,
        "two workspaces to choose from"
    );
    let mut app = sheet(&session);
    press(&mut app, "ws-general-leave");
    for _ in 0..3 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(
        session.handle().read(|tree| tree.workspaces.len()),
        1,
        "the workspace left this device's desk"
    );
}

/// One agent as Chat's record of `workspace` holds it: its id, its name, the model it plans with,
/// whether it still works here, what it may spend a day and the day it last counted, with what it
/// spent.
type StoredAgent = (
    String,
    String,
    Option<String>,
    bool,
    Option<u64>,
    Option<(String, u64)>,
);

/// What Chat's record of `workspace` 's agents holds, as the device's store keeps it: read without
/// taking its writer (Chat owns the record), the changes a window made committed as the store
/// writes them.
fn chat_agents(session: &Session, workspace: &str) -> Option<Vec<StoredAgent>> {
    let stored = session.stored_record(
        tilo_app::tilo_workspace_agent::OWNER,
        workspace,
        &collab_types::workspace_record_id(tilo_app::tilo_workspace_agent::KIND),
    )?;
    let options = automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore);
    let doc = automerge::AutoCommit::load_with_options(&stored, options).ok()?;
    let state =
        tilo_app::tilo_resource::Reader::<tilo_app::tilo_workspace_agent::WorkspaceAgents>::read(
            &doc,
        )
        .ok()?;
    Some(
        state
            .state()
            .agents
            .iter()
            .map(|agent| {
                (
                    agent.id.0.clone(),
                    agent.name.clone(),
                    agent.model.clone(),
                    agent.is_active(),
                    agent.spend_limit_micros,
                    agent
                        .spent
                        .as_ref()
                        .map(|spent| (spent.day.clone(), spent.cost_micros)),
                )
            })
            .collect(),
    )
}

/// What Chat's record of `workspace` holds once `done` says it does, stepping the session
/// meanwhile: the store commits a window's change after a debounce.
fn held_agents(
    session: &mut Session,
    workspace: &str,
    mut done: impl FnMut(Option<&[StoredAgent]>) -> bool,
) -> Option<Vec<StoredAgent>> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = chat_agents(session, workspace);
        if done(held.as_deref()) || std::time::Instant::now() >= deadline {
            return held;
        }
        session.step(Duration::from_millis(100)).expect("step");
    }
}

/// The Agents tab of the workspace in front, open.
fn agents_tab(session: &mut Session) -> HeadlessApp<WorkspaceSettingsApp> {
    invoke(session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(session);
    show_tab(
        session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "agents".to_owned(),
        },
    );
    app
}

/// `name` typed into the Agents tab's field and added: the tab makes an agent where Chat's record
/// keeps it.
fn make_agent(session: &mut Session, app: &mut HeadlessApp<WorkspaceSettingsApp>, name: &str) {
    press(app, "ws-agents-new");
    app.harness()
        .perform_named(
            "ws-agents-name",
            &ui::a11y::Action::SetValue(name.to_owned()),
        )
        .expect("name the agent");
    app.settle();
    capture(app, "workspace-settings-agents-new");
    press(app, "ws-agents-create");
    session.step(Duration::from_millis(1)).unwrap();
    pump(session, &mut [], |_, _| {
        app.settle();
        app.harness().a11y_node_named(name).is_some()
    });
}

/// The Agents tab makes an agent named in its field, where Chat's record keeps it, admits it to the
/// workspace, and its keeper chooses the model it plans with.
#[test]
fn workspace_settings_makes_the_workspaces_agents() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    settings_offer_a_model();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    let workspace = front_workspace(&session);
    let mut app = agents_tab(&mut session);
    capture(&mut app, "workspace-settings-agents-empty");
    make_agent(&mut session, &mut app, "Analyst");
    let row = listed_agent(&session, "analyst");
    assert_eq!(row.name, "Analyst");
    assert_eq!(
        row.status, "Kept by you",
        "the row says who keeps it, in one line"
    );
    assert_eq!(row.model, "Model: No model chosen in Settings");
    assert!(
        row.mine,
        "the person here keeps it, so its colour is theirs"
    );
    // Its keeper dresses it from its row: the palette opens under it.
    press(&mut app, "ws-agents-palette-analyst");
    settle(&mut session, &mut app);
    capture(&mut app, "workspace-settings-agents-palette");
    activate(&mut app, "Green");
    settle(&mut session, &mut app);
    assert_eq!(
        listed_agent(&session, "analyst").colour,
        4,
        "the keeper's colour lands in the record"
    );
    // Its keeper chooses the model it plans with, and the row says which.
    press(&mut app, "ws-agents-review-analyst");
    settle(&mut session, &mut app);
    press(&mut app, "ws-agent-use-local/sonnet");
    settle(&mut session, &mut app);
    press(&mut app, "ws-agent-back");
    settle(&mut session, &mut app);
    let row = listed_agent(&session, "analyst");
    assert_eq!(row.model, "Model: Sonnet from Anthropic");
    capture(&mut app, "workspace-settings-agents");
    let admitted = session.handle().read(|tree| {
        tree.active_workspace()
            .unwrap()
            .agent_members_at(tilo_clock::unix_now())
            .is_empty()
    });
    assert!(
        admitted,
        "the record is the whole admission: the workspace's grants name no agent"
    );
    let held = held_agents(&mut session, &workspace, |held| {
        held.is_some_and(|agents| {
            agents
                .iter()
                .any(|(id, name, _, active, ..)| id == "analyst" && name == "Analyst" && *active)
        })
    });
    assert!(
        held.as_deref().is_some_and(|agents| agents
            .iter()
            .any(|(id, name, _, active, ..)| id == "analyst" && name == "Analyst" && *active)),
        "Chat's record holds the agent the tab made: {held:?}"
    );
}

/// Removing an agent takes it out of the roster and out of the workspace,
/// and the record keeps it, retired, with its name.
#[test]
fn workspace_settings_retires_the_workspaces_agents() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    settings_offer_a_model();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    let workspace = front_workspace(&session);
    let mut app = agents_tab(&mut session);
    make_agent(&mut session, &mut app, "Analyst");
    press(&mut app, "ws-agents-remove-analyst");
    session.step(Duration::from_millis(1)).unwrap();
    pump(&mut session, &mut [], |_, _| {
        app.settle();
        app.harness().a11y_node_named("Analyst").is_none()
    });
    let held = held_agents(&mut session, &workspace, |held| {
        held.is_some_and(|agents| {
            agents
                .iter()
                .any(|(id, name, _, active, ..)| id == "analyst" && name == "Analyst" && !active)
        })
    });
    assert!(
        held.as_deref().is_some_and(|agents| agents
            .iter()
            .any(|(id, name, _, active, ..)| id == "analyst" && name == "Analyst" && !active)),
        "Chat's record keeps the retired agent and its name: {held:?}"
    );
    let live = session.handle().read(|tree| {
        tree.active_workspace()
            .unwrap()
            .agent_members_at(tilo_clock::unix_now())
            .len()
    });
    assert_eq!(live, 0, "the workspace holds nothing of the agent again");
}

/// An agent's keeper sets what it may spend a day from its review: the record keeps the limit, and
/// the row counts the day against it.
#[test]
fn workspace_settings_sets_what_an_agent_may_spend_a_day() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    settings_offer_a_model();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    let workspace = front_workspace(&session);
    let mut app = agents_tab(&mut session);
    make_agent(&mut session, &mut app, "Analyst");
    press(&mut app, "ws-agents-review-analyst");
    settle(&mut session, &mut app);
    let limit = shown_review(&session).limit.expect("the day's limit");
    assert_eq!(limit.summary, "No limit", "nobody set a day yet");
    assert_eq!(limit.today, "$0 today");
    assert!(limit.can_choose, "the person here keeps it");
    app.harness()
        .perform_named(
            "ws-agent-limit-field",
            &ui::a11y::Action::SetValue("5".to_owned()),
        )
        .expect("type the day's limit");
    app.settle();
    press(&mut app, "ws-agent-limit-set");
    settle(&mut session, &mut app);
    let limit = shown_review(&session).limit.expect("the day's limit");
    assert_eq!(limit.summary, "$5 a day", "the limit is the record's now");
    assert_eq!(limit.today, "$0 today");
    // The field takes the room, Set keeps its own at the row's right end, and the two share one
    // line on the desk as they do in the sheet.
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
    capture(&mut app, "workspace-settings-agent-review-limit");
    press(&mut app, "ws-agent-back");
    settle(&mut session, &mut app);
    let row = listed_agent(&session, "analyst");
    assert_eq!(
        row.spend, "$0 of $5 today",
        "the row counts the day against what the keeper allows, under who keeps it"
    );
    capture(&mut app, "workspace-settings-agents-spend");
    let held = held_agents(&mut session, &workspace, |held| {
        held.is_some_and(|agents| {
            agents
                .iter()
                .any(|(id, _, _, _, limit, _)| id == "analyst" && *limit == Some(5_000_000))
        })
    });
    assert!(
        held.as_deref().is_some_and(|agents| agents
            .iter()
            .any(|(id, _, _, _, limit, _)| id == "analyst" && *limit == Some(5_000_000))),
        "Chat's record keeps the day's limit: {held:?}"
    );
}

/// An agent the workspace admitted before Chat kept records of them is made
/// one of the record when the tab opens it: under the id its grants carry,
/// numbered around the names the workspace uses, and kept by whoever
/// admitted it.
#[test]
fn workspace_settings_reads_an_agents_admission_into_chats_record() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada"]);
    let workspace = front_workspace(&session);
    // The admission as workspaces stored it before records: a grant naming the agent, held by the
    // workspace, given by the person who admitted it.
    let mut tree = session.handle().read(Clone::clone);
    let front = tree.active_workspace_mut().expect("a workspace in front");
    front.policy.grants.push(tilo_action_policy::Grant::new(
        "agent-member:helper:observer",
        tilo_action_policy::Subject::agent("helper"),
        front.node(),
        tilo_action_policy::KindSet::READ,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::Person("ada".to_owned()),
    ));
    session.handle().replace_tree(tree);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);
    show_tab(
        &mut session,
        &mut app,
        &WorkspaceSettingsEffect::ShowTab {
            tab: "agents".to_owned(),
        },
    );

    let held = held_agents(&mut session, &workspace, |held| {
        held.is_some_and(|agents| {
            agents
                .iter()
                .any(|(id, name, _, active, ..)| id == "helper" && name == "Agent 1" && *active)
        })
    });
    assert!(
        held.as_deref().is_some_and(|agents| agents
            .iter()
            .any(|(id, name, _, active, ..)| id == "helper" && name == "Agent 1" && *active)),
        "Chat's record names the admitted agent, under its id and numbered around the names here: {held:?}"
    );
}
