//! The workspace settings sheet: what its tabs show of the workspace in
//! front, and what a person's choices there change.

use super::*;

/// Who sits here, as the session acts for them.
const PERSON: &str = "person";

/// The workspace in front belongs to the person who sits here, as it does
/// once the room's custody has taken it, with `members` already in it.
fn own_workspace(session: &Session, members: &[&str]) {
    let mut tree = session.handle().read(Clone::clone);
    let workspace = tree.active_workspace_mut().expect("a workspace in front");
    workspace.set_owner(PERSON.to_owned());
    for member in members {
        tilo_desktop::workspace_settings::set_member_role(
            workspace,
            &PERSON.to_owned(),
            &(*member).to_owned(),
            tilo_action_policy::Role::Viewer,
        );
    }
    session.handle().replace_tree(tree);
}

/// The sheet as a person presses it: the app the session hosts, lent the
/// seat the session lends its own, so a press arrives as the person's own.
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

/// Press the control `id` of `app`, as a person's own gesture.
fn press(app: &mut HeadlessApp<WorkspaceSettingsApp>, id: &str) {
    app.harness()
        .perform_named(id, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("press {id}: {error}"));
    app.settle();
}

/// Activate the element `name` says, as a screen reader does: the menu items
/// a control opens have no id of their own, only what they say.
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

/// Choose what the control `name` says at `choice`, as a screen reader does:
/// the choice is one segment of the control.
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

/// Show `tab` in the sheet, as pressing its tab does, and let the session
/// answer with the tab it shows.
fn show_tab(
    session: &mut Session,
    app: &mut HeadlessApp<WorkspaceSettingsApp>,
    tab: &WorkspaceSettingsEffect,
) {
    app.effect(tab).expect("show the tab");
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
}

/// The People tab gives a member another role, and the workspace holds it
/// afterwards, with the sheet's row saying so.
#[test]
fn workspace_settings_sets_a_members_role() {
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
    press(&mut app, "ws-people-role-ada");
    activate(&mut app, "Editor");
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
    let role = session.handle().read(|tree| {
        tree.active_workspace()
            .and_then(|workspace| workspace.role_of("ada"))
    });
    assert_eq!(
        role,
        Some(tilo_action_policy::Role::Editor),
        "ada is an editor of the workspace"
    );
    let row = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .people
        .people
        .into_iter()
        .find(|person| person.key == "ada")
        .expect("the sheet lists ada");
    assert_eq!(
        row.role_key.as_deref(),
        Some("editor"),
        "the sheet's row says which role she has"
    );
}

/// The Permissions tab gives ada a permission beyond her role, and takes it
/// back: the workspace holds it in between, and the sheet lists it.
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
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
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
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
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

/// Admit `agent` to the workspace in front, with the grants of `preset`.
fn admit_agent(session: &Session, agent: &str, preset: tilo_action_policy::roles::AgentPreset) {
    let mut tree = session.handle().read(Clone::clone);
    let workspace = tree.active_workspace_mut().expect("a workspace in front");
    tilo_desktop::workspace_settings::admit_agent(workspace, &PERSON.to_owned(), agent, preset);
    session.handle().replace_tree(tree);
}

/// Have `app` hold a capability here, so the workspace shows it and the
/// sheet offers it.
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

/// The agent's answer for one kind of action at one app, as the domain
/// reads the workspace.
fn answer(
    session: &Session,
    agent: &str,
    app: &str,
    kind: tilo_action_policy::ActionKind,
) -> tilo_desktop::workspace_settings::agent_access::AnswerCell {
    session.handle().read(|tree| {
        tilo_desktop::workspace_settings::agent_access::answer_at(
            tree.active_workspace().expect("a workspace"),
            agent,
            &tilo_action_policy::agent_answers::AnswerScope::app(app),
            kind,
        )
    })
}

/// The Agents tab shows an agent's answers, changes one, returns them to
/// what its admission says, and takes the agent out of the workspace.
#[test]
fn workspace_settings_answers_for_an_agent_and_takes_it_out() {
    use tilo_action_policy::ActionKind;
    use tilo_action_policy::roles::AgentPreset;
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    app_here(&session, "so.tilo.notes");
    admit_agent(&session, "helper", AgentPreset::Observer);
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
    let row = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .agents
        .agents
        .into_iter()
        .find(|agent| agent.key == "helper")
        .expect("the sheet lists the agent");
    assert!(
        row.detail.contains("Read admission"),
        "the row says what it was admitted with: {}",
        row.detail
    );
    press(&mut app, "ws-agents-review-helper");
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
    let review = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .agents
        .review
        .expect("the review is up");
    assert!(
        review
            .apps
            .iter()
            .any(|row| row.scope == "app:so.tilo.notes"),
        "the review lists the app: {review:?}"
    );
    activate(&mut app, "so.tilo.notes");
    choose(&mut app, "so.tilo.notes Changes", "Allow");
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
    let setting = answer(&session, "helper", "so.tilo.notes", ActionKind::Undoable);
    assert!(setting.explicit, "the person's answer is the agent's own");
    assert_eq!(setting.answer, tilo_action_policy::Answer::Allow);
    let listed = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .agents
        .review
        .expect("the review is up")
        .apps
        .into_iter()
        .find(|row| row.scope == "app:so.tilo.notes")
        .expect("the review lists the app");
    assert!(listed.customised, "the review says the app was customised");

    activate(&mut app, "Use defaults");
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
    let back = answer(&session, "helper", "so.tilo.notes", ActionKind::Undoable);
    assert!(!back.explicit, "the app follows the admission again");
    assert_ne!(
        back.answer,
        tilo_action_policy::Answer::Allow,
        "an observer is not allowed to change without asking"
    );

    press(&mut app, "ws-agent-back");
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
    activate(&mut app, "Revoke");
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
    assert!(
        session
            .workspace_settings()
            .expect("workspace settings")
            .settings()
            .agents
            .agents
            .is_empty(),
        "the sheet lists no agent again"
    );
    let admitted = session.handle().read(|tree| {
        tilo_desktop::workspace_settings::agent_summaries(tree.active_workspace().unwrap()).len()
    });
    assert_eq!(admitted, 0, "the workspace holds no agent again");
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
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
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
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
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
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
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
