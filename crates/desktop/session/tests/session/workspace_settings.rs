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
    // The person at this session, as the workspace's own policy names them: a computer's
    // session is its identity's person, not the id a session with none runs as.
    let person = workspace
        .policy
        .owner
        .clone()
        .expect("the workspace has its person as owner");
    for member in members {
        tilo_session_core::desktop::workspace_settings::set_member_role(
            workspace,
            &person,
            member,
            tilo_action_policy::Role::Viewer,
        )
        .expect("the person may admit a member");
    }
    session.handle().replace_tree(tree);
}

/// The sheet as a person presses it: the app the session hosts, lent the state the session lends its
/// own, so a press arrives as the person's own — the person the session acts as, read from its
/// device's certificate as the session reads it at start (`identity::own`), never a name of the
/// test's.
fn sheet(session: &Session) -> TestApp<WorkspaceSettingsApp> {
    let person = tilo_session::identity::own_person(&Options::default());
    let state = session
        .workspace_settings()
        .expect("workspace settings")
        .clone();
    let mut app =
        TestApp::<WorkspaceSettingsApp>::for_person_with_lent(&person, state).expect("the sheet");
    app.harness().resize((1280, 900)).expect("a screen");
    app.settle();
    app
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
fn press(app: &mut TestApp<WorkspaceSettingsApp>, id: &str) {
    app.harness()
        .perform_named(id, &tilo_ui_base::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("press {id}: {error}"));
    app.settle();
}

/// Activate the element `name` says, as a screen reader does: the menu items a control opens have
/// no id of their own, only what they say.
fn activate(app: &mut TestApp<WorkspaceSettingsApp>, name: &str) {
    let node = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("no element says {name:?}"));
    app.harness()
        .perform_a11y_action(node, &tilo_ui_base::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("activate {name}: {error}"));
    app.settle();
}

/// Choose what `app`'s line for `kind` says at `segment` , as a screen reader does: the choice is
/// one segment of the line, addressed by the app the line answers for. The line's control is named
/// by its scope rather than the words over it, since two installed apps can share a name (the Rust
/// Notes and the tilolang Notes are both "Notes") and the words would name either.
fn choose(app: &mut TestApp<WorkspaceSettingsApp>, scope: &str, kind: &str, segment: usize) {
    press(
        app,
        &format!("ws-agent-answer-{scope}-{kind}:segment:{segment}"),
    );
}

/// Show `tab` in the sheet, as pressing its tab does, and let the session answer with the tab it
/// shows.
fn show_tab(
    session: &mut Session,
    app: &mut TestApp<WorkspaceSettingsApp>,
    tab: &WorkspaceSettingsEffect,
) {
    app.effect(tab).expect("show the tab");
    settle(session, app);
}

/// The shell's search, as it publishes it while it is open.
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
        tilo_session_core::desktop::verbs::SEARCH_RESOURCE,
    )
}

/// The People tab gives each member another role, and the workspace holds both afterwards, with the
/// sheet's rows saying so.
#[test]
fn workspace_settings_sets_each_members_role() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
        (
            workspace.policy.role_of("ada"),
            workspace.policy.role_of("bo"),
        )
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

#[test]
fn workspace_settings_assigns_a_custom_role_and_uses_it_for_document_access() {
    use tilo_action_policy::{Answer, Context};
    use tilo_action_types::ActionKind;

    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada", "bo"]);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    session.step(Duration::from_millis(1)).unwrap();
    let mut app = sheet(&session);

    app.effect(&WorkspaceSettingsEffect::SaveCustomRole {
        role: String::new(),
        name: "Reviewers".into(),
    })
    .unwrap();
    settle(&mut session, &mut app);
    let role = session.handle().read(|tree| {
        let workspace = tree.active_workspace().unwrap();
        workspace
            .policy
            .custom_roles
            .iter()
            .find_map(|(id, name)| (name == "Reviewers").then(|| id.clone()))
            .expect("the People action created a role")
    });
    for effect in [
        WorkspaceSettingsEffect::SetCustomRole {
            member: "ada".into(),
            role: role.clone(),
            assigned: true,
        },
        WorkspaceSettingsEffect::SetDocumentAccess {
            who: "everyone".into(),
            app: String::new(),
            document: String::new(),
            access: "no-access".into(),
        },
        WorkspaceSettingsEffect::SetDocumentAccess {
            who: format!("role:{role}"),
            app: String::new(),
            document: "so.tilo.notes".into(),
            access: "view".into(),
        },
        WorkspaceSettingsEffect::SetDocumentAccess {
            who: format!("role:{role}"),
            app: "so.tilo.notes".into(),
            document: "review".into(),
            access: "edit".into(),
        },
    ] {
        app.effect(&effect).unwrap();
        settle(&mut session, &mut app);
    }

    session.handle().read(|tree| {
        let workspace = tree.active_workspace().unwrap();
        let id = workspace.id().to_string();
        let document = collab_types::document_node(&id, "so.tilo.notes", "review");
        let action = tilo_action_policy::TreePath::workspace(&id)
            .app("so.tilo.notes")
            .kind("notes.note")
            .resource("review")
            .action("field-edit");
        let context = Context::at_time(tilo_clock::wall());
        for node in [document, action] {
            for (person, expected) in [("ada", Answer::Allow), ("bo", Answer::Deny)] {
                assert_eq!(
                    workspace
                        .policy
                        .party(person)
                        .decide(&node, ActionKind::Undoable, &context)
                        .answer,
                    expected,
                    "{person} at {node}",
                );
            }
        }
        let other_note = collab_types::document_node(&id, "so.tilo.notes", "other");
        let file = collab_types::document_node(&id, "so.tilo.files", "other");
        let reviewer = workspace.policy.party("ada");
        assert_eq!(
            reviewer
                .decide(&other_note, ActionKind::Read, &context)
                .answer,
            Answer::Allow
        );
        assert_eq!(
            reviewer
                .decide(&other_note, ActionKind::Undoable, &context)
                .answer,
            Answer::Deny
        );
        assert_eq!(
            reviewer.decide(&file, ActionKind::Read, &context).answer,
            Answer::Deny
        );
        let targets = collab_types::document_access_scopes(
            &workspace.policy,
            &[("so.tilo.notes".into(), "review".into())],
            [],
        );
        let rules = collab_types::document_access_rules(&workspace.policy, &targets);
        let rule_at = |app: &str, doc: &str| {
            rules
                .iter()
                .find(|rule| rule.app == app && rule.document == doc)
                .unwrap()
        };
        assert_eq!(rule_at("", "").access, "no-access");
        assert_eq!(rule_at("", "so.tilo.notes").access, "view");
        assert_eq!(
            rule_at("so.tilo.notes", "review").recipient,
            format!("role:{role}")
        );
        assert_eq!(rule_at("so.tilo.notes", "review").access, "edit");
    });
}

/// The People tab says what the owner's role is instead of offering it: who owns the workspace is a
/// fact of it, so their row has no role control and no one chooses a new owner here.
#[test]
fn the_people_tab_offers_the_owners_role_as_a_fact() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
            .and_then(|workspace| workspace.policy.owner().map(str::to_owned))
    });
    let owner = owner.expect("the workspace has an owner");
    assert!(
        app.harness()
            .perform_named(
                &format!("ws-people-role-{owner}"),
                &tilo_ui_base::a11y::Action::Activate
            )
            .is_err(),
        "the owner's row offers no role to choose"
    );
    assert!(
        app.harness()
            .perform_named("ws-people-ownership", &tilo_ui_base::a11y::Action::Activate)
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
                .perform_named(gone, &tilo_ui_base::a11y::Action::Activate)
                .is_err(),
            "{gone} is not offered"
        );
    }
}

/// The Permissions tab gives ada a permission beyond her role, and takes it back: the workspace
/// holds it in between, and the sheet lists it.
#[test]
fn workspace_settings_gives_and_revokes_a_permission() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
    assert!(
        given.detail.contains("network"),
        "the row says what the permission allows: {}",
        given.detail
    );
    let held = session
        .handle()
        .read(|tree| tree.active_workspace().expect("a workspace").sandbox.len());
    assert_eq!(held, 1, "the workspace gives ada the network");

    press(&mut app, &format!("ws-permission-revoke-{}", given.key));
    press(&mut app, "ws-settings-decision.primary");
    settle(&mut session, &mut app);
    let left = session
        .handle()
        .read(|tree| tree.active_workspace().expect("a workspace").sandbox.len());
    assert_eq!(left, 0, "the workspace gives the network to no one again");
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

/// What a grant's raw path starts with, which a row a person reads never shows.
const WORKSPACE_WORD: &str = "workspace ";

/// An action catalogue whose apps publish what it keeps: what the sheet reads an app's action's own
/// name from, as the sheet asking to allow that action does.
#[derive(Default)]
struct Publishing(std::cell::RefCell<Vec<tilo_desktop_search::providers::PublishedActions>>);

impl tilo_session::AppActions for Publishing {
    fn published(&self, workspace: &str) -> Vec<tilo_desktop_search::providers::PublishedActions> {
        self.0
            .borrow()
            .iter()
            .filter(|published| workspace.is_empty() || published.workspace == workspace)
            .cloned()
            .collect()
    }

    fn invoke(
        &self,
        _target: tilo_actions::remote::RemoteActionTarget,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::ApiError> {
        Ok(())
    }
}

/// What Chat publishes for the action `post`, named as its own catalogue names it.
fn chat_posts(workspace: &str) -> tilo_desktop_search::providers::PublishedActions {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, ExecutionLocus, ParamType};
    let post = ActionSpec::prompted_param(
        "post",
        "Post a message",
        ActionParam::required("text", ParamType::Text, "Message"),
        "",
        ExecutionLocus::ClientLocal,
        |text| EncodedAction {
            type_id: "so.tilo.chat.post".into(),
            payload: text.as_bytes().to_vec(),
        },
    );
    let chat = ResourceSpec::new("chat.workspace", "chat.workspace", "Chat").with_action(post);
    tilo_desktop_search::providers::PublishedActions::of("so.tilo.chat", workspace, &[chat])
}

/// An app's grant to take another app's action, given on the sheet that asked: the Permissions tab
/// lists it under Apps, named as the person knows the app and the action, and its Revoke takes it
/// back.
#[test]
fn workspace_settings_lists_an_apps_grant_under_apps_and_revokes_it() {
    use tilo_action_policy::agent_answers::{AnswerScope, answer_grant};
    use tilo_action_policy::{Answer, Grantee};
    use tilo_action_types::ActionKind;
    let _settled = tilo_ui_base::motion::testing::settled();
    let actions = Rc::new(Publishing::default());
    let mut session = crate::common::headless(Options {
        apps: Some(actions.clone()),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    let notes = Grantee::app("so.tilo.notes");
    let mut tree = session.handle().read(Clone::clone);
    let workspace = tree.active_workspace_mut().expect("a workspace in front");
    let grant = answer_grant(
        &workspace.id().to_string(),
        notes.clone(),
        (
            &AnswerScope::action("so.tilo.chat", "chat.conversation", "post"),
            ActionKind::Undoable,
        ),
        Answer::Allow,
        Grantee::person(PERSON),
    );
    workspace.policy.grant(grant);
    let published = workspace.id().to_string();
    session.handle().replace_tree(tree);
    actions.0.borrow_mut().push(chat_posts(&published));
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
    let apps = tilo_session_core::desktop::i18n::t("workspace-settings-section-apps");
    let section = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .permissions
        .sections
        .into_iter()
        .find(|section| section.title == apps)
        .expect("the tab lists apps' permissions");
    let [given] = section.permissions.as_slice() else {
        panic!("one app permission: {:?}", section.permissions);
    };
    assert!(
        !given.holder.contains("so.tilo"),
        "the app is named as the person knows it: {}",
        given.holder
    );
    assert!(
        given.detail.contains("Use “Post a message” in Chat")
            && !given.detail.contains("“post”")
            && !given.detail.contains(WORKSPACE_WORD),
        "the row names the action as the app's catalogue does, not its id or the grant's path: {}",
        given.detail
    );
    crate::common::app_shot(&mut app, "workspace-settings-app-grant");

    press(&mut app, &format!("ws-permission-revoke-{}", given.key));
    press(&mut app, "ws-settings-decision.primary");
    settle(&mut session, &mut app);
    let left = session.handle().read(|tree| {
        tree.active_workspace()
            .expect("a workspace")
            .policy
            .grants_of(&notes)
            .count()
    });
    assert_eq!(left, 0, "the workspace no longer lets Notes post to Chat");
}

/// The answer the review shows for changes in `app`, by its place among Allow, Ask first and Deny,
/// and whether the agent's bound sets the app's answers.
fn changes_answer(session: &Session, app: &str) -> (usize, bool) {
    let row = listed_app(session, &format!("app:{app}"));
    let line = row
        .kinds
        .iter()
        .find(|line| line.kind == "change")
        .expect("the app's line for changes");
    (line.answer, row.customised)
}

/// The answers the workspace's policy keeps for `agent`.
fn answers_of(session: &Session, agent: &str) -> Vec<tilo_action_policy::Grant> {
    session.handle().read(|tree| {
        let workspace = tree.active_workspace().expect("a workspace");
        workspace
            .policy
            .grants
            .iter()
            .filter(|grant| {
                tilo_action_policy::agent_answers::is_answer(grant)
                    && grant.grantee == tilo_action_policy::Grantee::agent(agent, PERSON)
            })
            .cloned()
            .collect()
    })
}

/// Let the session answer an ask the sheet made, and the sheet settle.
fn settle(session: &mut Session, app: &mut TestApp<WorkspaceSettingsApp>) {
    session.step(Duration::from_millis(1)).unwrap();
    app.settle();
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

/// The device the sheet's records are kept on, and a session of its person's there: this
/// computer's own identity, its daemon serving the person, and the models it offers linked into
/// the workspace the session shows as the daemon follows this computer's choice.
fn session_keeping_workspaces() -> (crate::common::Device, Session) {
    let device = crate::common::device_daemon(crate::common::device_home(), None);
    let session = device.session_with(Options {
        collab: false,
        state: Some(device.home.state().to_path_buf()),
        ..Options::default()
    });
    (device, session)
}

/// A session of a device's daemon of the test's own, whose person's workspaces the daemon keeps:
/// the records the Settings sheet writes are read and written through it.
fn session_on_its_daemon() -> Session {
    crate::common::on_its_daemon(
        &[OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        Options {
            collab: false,
            ..Options::default()
        },
    )
}

/// The Agents tab shows an agent's answers, changes one, returns them to what its admission says,
/// chooses its model and takes the agent out of the workspace.
#[test]
fn workspace_settings_answers_for_an_agent_and_takes_it_out() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (_device, mut session) = session_keeping_workspaces();
    // The settings are the running session's, installed for everything in its process.
    settings_offer_a_model();
    session.step(Duration::from_millis(1)).unwrap();
    // Nothing of Notes is open or held here: an installed app is answered for before an agent
    // ever reaches for it.
    own_workspace(&session, &[]);
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
    })
    .expect("the person makes an agent");
    // The tab's write is the device daemon's to make: the row shows once the record is written.
    pump(&mut session, &mut [], |session, _| {
        app.settle();
        agents_shown(session)
            .agents
            .iter()
            .any(|agent| agent.key == "helper")
    });
    let row = listed_agent(&session, "helper");
    assert_eq!(
        row.name, "helper",
        "the row names the agent the record keeps"
    );
    assert_eq!(
        row.status, "Added by you",
        "the row says who added it, in one line"
    );
    assert_eq!(row.model, "Model: No model chosen in Settings");
    // This computer's choice reaches the workspace's models, which the review offers: the
    // device's daemon follows the settings it saved and links the choice into its workspace.
    press(&mut app, "ws-agents-review-helper");
    settle(&mut session, &mut app);
    pump(&mut session, &mut [], |_, _| {
        app.settle();
        app.harness()
            .a11y_node_named("ws-agent-use-local/sonnet")
            .is_some()
    });
    let review = shown_review(&session);
    assert!(
        review
            .apps
            .iter()
            .any(|row| row.scope == "app:so.tilo.notes"),
        "the review lists the installed app though no window of it is open: {review:?}"
    );
    // An agent never carries out an outside effect unasked: that line offers no Allow.
    let notes = listed_app(&session, "app:so.tilo.notes");
    let choices = |kind: &str| {
        notes
            .kinds
            .iter()
            .find(|line| line.kind == kind)
            .map(|line| line.choices.clone())
    };
    assert_eq!(choices("change"), Some(vec![0, 1, 2]), "{notes:?}");
    assert_eq!(choices("outside"), Some(vec![1, 2]), "{notes:?}");
    assert_eq!(
        notes.kinds[2].caption, None,
        "an app's own name says what reaching outside it means"
    );
    // The person's own account rows say what their outside effects are, which their names do
    // not, and the review's line says what an app's outside effects may be given.
    let workspaces = listed_app(
        &session,
        &format!("app:{}", tilo_action_policy::agent_answers::ACCOUNT_APP),
    );
    assert_eq!(workspaces.label, "Your workspaces");
    assert_eq!(
        workspaces.kinds[2].caption.as_deref(),
        Some("Creating, branching, entering and deleting your workspaces.")
    );
    // The row that serves the workspace's people wears the people's mark, not another
    // window.
    let members = listed_app(
        &session,
        &format!("app:{}", tilo_action_policy::agent_answers::WORKSPACE_APP),
    );
    assert_eq!(members.label, "Members and settings");
    assert_eq!(members.icon, tilo_ui_base::icons::Icon::Users);
    assert_eq!(
        review.note,
        "In apps, outside effects ask you first or are denied; in your own account you may allow them."
    );
    // Notes' changes answer Ask first, the second segment of its line (Allow, Ask first, Deny).
    choose(&mut app, "app:so.tilo.notes", "change", 1);
    settle(&mut session, &mut app);
    assert_eq!(
        changes_answer(&session, "so.tilo.notes"),
        (1, true),
        "the review shows the person's answer, and that it was set here"
    );
    let answers = answers_of(&session, "helper");
    assert_eq!(
        answers.len(),
        1,
        "the answer is a grant of the workspace's on the agent: {answers:?}"
    );
    assert!(
        answers[0].node.to_string().ends_with("/app:so.tilo.notes")
            && answers[0].answer == tilo_action_policy::Answer::Ask,
        "{answers:?}"
    );

    // change its agents, and the review says it chose.
    // review says it chose.
    assert!(
        review
            .models
            .as_ref()
            .is_some_and(|models| models.can_choose),
        "a member who may change the workspace's agents chooses the model"
    );
    press(&mut app, "ws-agent-use-local/sonnet");
    settle(&mut session, &mut app);
    let chosen = shown_review(&session)
        .models
        .expect("the review offers models");
    assert_eq!(chosen.summary, "Sonnet from Anthropic");
    crate::common::app_shot(&mut app, "workspace-settings-agent-review");

    activate(&mut app, "Use defaults");
    settle(&mut session, &mut app);
    assert_eq!(
        changes_answer(&session, "so.tilo.notes"),
        (0, false),
        "the app follows the admission again, whose cap admits changes in it"
    );
    assert!(
        answers_of(&session, "helper").is_empty(),
        "the workspace keeps no answer for it again"
    );

    activate(&mut app, "Remove");
    settle(&mut session, &mut app);
    let after = agents_shown(&session);
    assert!(after.review.is_none(), "retiring ends the review");
    assert!(after.agents.is_empty(), "the sheet lists no agent again");
    let held = session.handle().read(|tree| {
        tree.active_workspace()
            .unwrap()
            .policy
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
fn chat_emoji(session: &Session, workspace: &str) -> Result<Option<Vec<String>>, String> {
    let Some(stored) = session.stored_record(
        tilo_app::tilo_workspace_emoji::OWNER,
        workspace,
        &collab_types::workspace_document_id(tilo_app::tilo_workspace_emoji::KIND),
    )?
    else {
        return Ok(None);
    };
    // A window appends to the record as it changes it, and this read may land on a tail it has not
    // finished writing: what came before it is the record as the last finished write left it, which
    // is what Chat loads.
    let options = automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore);
    let doc = automerge::AutoCommit::load_with_options(&stored, options)
        .map_err(|error| format!("Chat's emoji record does not open: {error}"))?;
    let state =
        tilo_app::tilo_resource::Reader::<tilo_app::tilo_workspace_emoji::WorkspaceEmoji>::read(
            &doc,
        )
        .map_err(|error| format!("Chat's emoji record does not read: {error}"))?;
    Ok(Some(
        state
            .state()
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    ))
}

/// The Emoji tab adds a picture the person chooses to the workspace's emoji,
/// where Chat's record holds it, and removes it from there again.
#[test]
fn workspace_settings_adds_and_removes_the_workspaces_emoji() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // The picture the person picks, in a directory of the test's own: the record the tab writes
    // is Chat's, in the workspace kept by the device's daemon, here one of the test's own.
    let pictures = tempfile::tempdir().expect("a directory of its own");
    let picture = pictures.path().join("wave.png");
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
    // The session is the device's daemon's person's, on the link the daemon made for it: the
    // person's workspaces, and Chat's records in them, are the daemon's to read and write.
    let mut session = crate::common::on_its_daemon_with(
        &[OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |_device| Options {
            collab: false,
            asks: Some(crate::common::person_asked()),
            ..Options::default()
        },
    );
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

    // The device answers what the tab offers before the person presses it: the tab offers
    // adding once it said they may add one.
    pump(&mut session, &mut [], |session, _| {
        session
            .workspace_settings()
            .expect("workspace settings")
            .settings()
            .emoji
            .can_add
    });
    app.settle();

    // Add emoji asks the person for a picture: the file chooser's question, answered with the
    // picture the test made, as the person choosing it in their own picker answers.
    press(&mut app, "ws-emoji-add");
    session.step(Duration::from_millis(1)).unwrap();
    let question = crate::common::person_asked_next(&mut session);
    assert_eq!(
        question.kind,
        tilo_portal::file_chooser::FILE_CHOOSER_QUESTION,
        "the tab asks for a picture"
    );
    crate::common::person_answers(
        &mut session,
        tilo_service::Answer {
            uris: vec![format!("file://{}", picture.display())],
            ..tilo_service::Answer::default()
        },
    );

    // Chat's emoji document holds it before the tab shows it: what the device holds is read
    // again until the emoji is in it, and a give-up says what the record was instead.
    let held = held_by_chat(&mut session, &workspace, |held| {
        held.is_some_and(|names| names.iter().any(|name| name == "wave"))
    });
    assert!(
        matches!(&held, Ok(Some(names)) if names.iter().any(|name| name == "wave")),
        "Chat's record holds the emoji the tab added: {held:?}"
    );
    // The tab shows the emoji.
    pump(&mut session, &mut [], |_, _| {
        app.settle();
        app.harness().a11y_node_named(":wave:").is_some()
    });

    // Remove takes it out of the tab and out of Chat's record.
    press(&mut app, "ws-emoji-remove-wave");
    session.step(Duration::from_millis(1)).unwrap();
    let held = held_by_chat(&mut session, &workspace, |held| {
        held.is_some_and(|names| !names.iter().any(|name| name == "wave"))
    });
    assert!(
        matches!(&held, Ok(Some(names)) if !names.iter().any(|name| name == "wave")),
        "removing the emoji in the tab takes it out of Chat's record: {held:?}"
    );
    pump(&mut session, &mut [], |_, _| {
        app.settle();
        app.harness().a11y_node_named(":wave:").is_none()
    });
}

/// What Chat's record of `workspace` holds once `done` says it does, stepping the session
/// meanwhile: the store commits a window's change after a debounce.
fn held_by_chat(
    session: &mut Session,
    workspace: &str,
    mut done: impl FnMut(Option<&[String]>) -> bool,
) -> Result<Option<Vec<String>>, String> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = chat_emoji(session, workspace);
        if done(held.as_ref().ok().and_then(Option::as_deref))
            || std::time::Instant::now() >= deadline
        {
            return held;
        }
        session.step(Duration::from_millis(100)).expect("step");
    }
}

/// Every name the sheet says, with what it says of it, as a screen reader reads it. A list row
/// is named by its title and its secondary line; it is said here by the line it is titled with.
fn said(app: &mut TestApp<WorkspaceSettingsApp>) -> Vec<(String, String)> {
    let titles: std::collections::HashMap<_, _> =
        app.harness().a11y_row_titles().into_iter().collect();
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(id, node)| {
            let label = node.label()?;
            let said = titles.get(&id).cloned().unwrap_or_else(|| label.to_owned());
            Some((said, node.value().unwrap_or_default().to_owned()))
        })
        .collect()
}

/// The General tab says what the workspace is: its name under its label,
/// inviting people, the workspace at a glance, the devices it shows on and
/// leaving it.
#[test]
fn workspace_settings_general_says_what_the_workspace_is() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
        "nothing says the layout stays on one device: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|(label, _)| label == "Untitled" || label == "Ohne Titel"),
        "every row is named: {labels:?}"
    );
}

/// Leaving from the General tab takes the workspace off this device's shell,
/// as the same choice in the workspace menu does.
#[test]
fn workspace_settings_general_leaves_the_workspace() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
        "the workspace left this device's shell"
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
fn chat_agents(session: &Session, workspace: &str) -> Result<Option<Vec<StoredAgent>>, String> {
    let Some(stored) = session.stored_record(
        tilo_app::tilo_workspace_agent::OWNER,
        workspace,
        &collab_types::workspace_document_id(tilo_app::tilo_workspace_agent::KIND),
    )?
    else {
        return Ok(None);
    };
    let options = automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore);
    let doc = automerge::AutoCommit::load_with_options(&stored, options)
        .map_err(|error| format!("Chat's agents record does not open: {error}"))?;
    let state =
        tilo_app::tilo_resource::Reader::<tilo_app::tilo_workspace_agent::WorkspaceAgents>::read(
            &doc,
        )
        .map_err(|error| format!("Chat's agents record does not read: {error}"))?;
    Ok(Some(
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
    ))
}

/// What Chat's record of `workspace` holds once `done` says it does, stepping the session
/// meanwhile: the store commits a window's change after a debounce.
fn held_agents(
    session: &mut Session,
    workspace: &str,
    mut done: impl FnMut(Option<&[StoredAgent]>) -> bool,
) -> Result<Option<Vec<StoredAgent>>, String> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = chat_agents(session, workspace);
        if done(held.as_ref().ok().and_then(Option::as_deref))
            || std::time::Instant::now() >= deadline
        {
            return held;
        }
        session.step(Duration::from_millis(100)).expect("step");
    }
}

/// The Agents tab of the workspace in front, open.
fn agents_tab(session: &mut Session) -> TestApp<WorkspaceSettingsApp> {
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
fn make_agent(session: &mut Session, app: &mut TestApp<WorkspaceSettingsApp>, name: &str) {
    press(app, "ws-agents-new");
    app.harness()
        .perform_named(
            "ws-agents-name",
            &tilo_ui_base::a11y::Action::SetValue(name.to_owned()),
        )
        .expect("name the agent");
    app.settle();
    crate::common::app_shot(app, "workspace-settings-agents-new");
    press(app, "ws-agents-create");
    session.step(Duration::from_millis(1)).unwrap();
    pump(session, &mut [], |_, _| {
        app.settle();
        app.harness().a11y_node_named(name).is_some()
    });
}

/// The Agents tab makes an agent named in its field, where Chat's record keeps it, admits it to the
/// workspace, and a member chooses the model it plans with.
#[test]
fn workspace_settings_makes_the_workspaces_agents() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (_device, mut session) = session_keeping_workspaces();
    // The settings are the running session's, installed for everything in its process.
    settings_offer_a_model();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &[]);
    let workspace = front_workspace(&session);
    let mut app = agents_tab(&mut session);
    crate::common::app_shot(&mut app, "workspace-settings-agents-empty");
    make_agent(&mut session, &mut app, "Analyst");
    let row = listed_agent(&session, "analyst");
    assert_eq!(row.name, "Analyst");
    assert_eq!(
        row.status, "Added by you",
        "the row says who added it, in one line"
    );
    assert_eq!(row.model, "Model: No model chosen in Settings");
    // A member chooses the model it plans with, of the ones the workspace keeps once this
    // computer's choice reached it, and the row says which.
    press(&mut app, "ws-agents-review-analyst");
    settle(&mut session, &mut app);
    pump(&mut session, &mut [], |_, _| {
        app.settle();
        app.harness()
            .a11y_node_named("ws-agent-use-local/sonnet")
            .is_some()
    });
    press(&mut app, "ws-agent-use-local/sonnet");
    settle(&mut session, &mut app);
    press(&mut app, "ws-agent-back");
    settle(&mut session, &mut app);
    let row = listed_agent(&session, "analyst");
    assert_eq!(row.model, "Model: Sonnet from Anthropic");
    crate::common::app_shot(&mut app, "workspace-settings-agents");
    let admitted = session.handle().read(|tree| {
        tree.active_workspace()
            .unwrap()
            .policy
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
        matches!(&held, Ok(Some(agents)) if agents
            .iter()
            .any(|(id, name, _, active, ..)| id == "analyst" && name == "Analyst" && *active)),
        "Chat's record holds the agent the tab made: {held:?}"
    );
}

/// Removing an agent takes it out of the roster and out of the workspace,
/// and the record keeps it, retired, with its name.
#[test]
fn workspace_settings_retires_the_workspaces_agents() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session_on_its_daemon();
    // The settings are the running session's, installed for everything in its process.
    settings_offer_a_model();
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
        matches!(&held, Ok(Some(agents)) if agents
            .iter()
            .any(|(id, name, _, active, ..)| id == "analyst" && name == "Analyst" && !active)),
        "Chat's record keeps the retired agent and its name: {held:?}"
    );
    let live = session.handle().read(|tree| {
        tree.active_workspace()
            .unwrap()
            .policy
            .agent_members_at(tilo_clock::unix_now())
            .len()
    });
    assert_eq!(live, 0, "the workspace holds nothing of the agent again");
}

/// A member sets what the workspace's agent may spend a day from its review: the record keeps
/// the limit, and the row counts the day against it.
#[test]
fn workspace_settings_sets_what_an_agent_may_spend_a_day() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session_on_its_daemon();
    // The settings are the running session's, installed for everything in its process.
    settings_offer_a_model();
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
    assert!(
        limit.can_choose,
        "the person here may change the workspace's agents"
    );
    app.harness()
        .perform_named(
            "ws-agent-limit-field",
            &tilo_ui_base::a11y::Action::SetValue("5".to_owned()),
        )
        .expect("type the day's limit");
    app.settle();
    press(&mut app, "ws-agent-limit-set");
    settle(&mut session, &mut app);
    let limit = shown_review(&session).limit.expect("the day's limit");
    assert_eq!(limit.summary, "$5 a day", "the limit is the record's now");
    assert_eq!(limit.today, "$0 today");
    // The field takes the room, Set keeps its own at the row's right end, and the two share one
    // line on the shell as they do in the sheet.
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
    crate::common::app_shot(&mut app, "workspace-settings-agent-review-limit");
    press(&mut app, "ws-agent-back");
    settle(&mut session, &mut app);
    let row = listed_agent(&session, "analyst");
    assert_eq!(
        row.spend, "$0 of $5 today",
        "the row counts the day against what the workspace allows"
    );
    crate::common::app_shot(&mut app, "workspace-settings-agents-spend");
    let held = held_agents(&mut session, &workspace, |held| {
        held.is_some_and(|agents| {
            agents
                .iter()
                .any(|(id, _, _, _, limit, _)| id == "analyst" && *limit == Some(5_000_000))
        })
    });
    assert!(
        matches!(&held, Ok(Some(agents)) if agents
            .iter()
            .any(|(id, _, _, _, limit, _)| id == "analyst" && *limit == Some(5_000_000))),
        "Chat's record keeps the day's limit: {held:?}"
    );
}

/// An agent admitted to the workspace before Chat kept agents of its own is listed in its place
/// when a member who may change the workspace's agents opens the tab: under the id its grants
/// carry, numbered around the names the workspace uses. The agents are the workspace's, so
/// another member's admission is listed as well as the person's own.
#[test]
fn workspace_settings_reads_an_agents_admission_into_chats_record() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session_on_its_daemon();
    session.step(Duration::from_millis(1)).unwrap();
    own_workspace(&session, &["ada"]);
    let workspace = front_workspace(&session);
    // The admissions as workspaces stored them before Chat kept agents of its own: grants naming
    // each agent, held by the workspace, given by the person who admitted it: one the person's
    // here, one ada's.
    let mut tree = session.handle().read(Clone::clone);
    let front = tree.active_workspace_mut().expect("a workspace in front");
    // The person at this session, as the workspace's own policy names them.
    let person = front
        .policy
        .owner
        .clone()
        .expect("the workspace has its person as owner");
    for (agent, admitted_by) in [("helper", person.as_str()), ("analyst", "ada")] {
        front.policy.grants.push(tilo_action_policy::Grant::new(
            format!("agent-member:{agent}:observer"),
            tilo_action_policy::Grantee::agent(agent, admitted_by.to_owned()),
            front.policy.node(),
            tilo_action_types::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(admitted_by.to_owned()),
        ));
    }
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
        held.is_some_and(|agents| agents.len() == 2)
    });
    let held = held
        .expect("Chat's agents in the workspace")
        .expect("Chat's record of them");
    let mut ids: Vec<&str> = held.iter().map(|(id, ..)| id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["analyst", "helper"],
        "each admission is given an agent of the workspace's, whoever gave it: {held:?}"
    );
    let mut names: Vec<&str> = held.iter().map(|(_, name, ..)| name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["Agent 1", "Agent 2"],
        "Chat numbers the admitted agents around the names here: {held:?}"
    );
    assert!(held.iter().all(|(_, _, _, active, ..)| *active), "{held:?}");
}
