//! What a session's agent is told about the apps of the workspace it was
//! admitted to: every installed app, the ones it may not open by name alone,
//! and, when it cannot reach the workspace at all, why, never an empty list
//! that reads as "there are no apps".

use super::super::{AppBrokerState, BrokerChannels, FanoutState, PeerPublisher};
use crate::apps::test_support::{
    Grantee, Grants as _, KindSet, TreePath, Workspace, WorkspaceId, WorkspaceTree,
};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_KIND, ActionCallError, Caller, CataloguePage,
    CatalogueQuery, workspace_provider,
};

const NOTES: &str = "so.tilo.notes";
const SHEETS: &str = "so.tilo.sheets";

/// A launcher with `apps` installed.
struct Installed(Vec<&'static str>);

impl crate::apps::services::LauncherServices for Installed {
    fn applications(&self) -> Vec<crate::apps::services::LaunchableApplication> {
        self.0
            .iter()
            .map(|app| crate::apps::services::LaunchableApplication {
                capabilities: Vec::new(),
                package_id: (*app).into(),
                display_name: app.rsplit('.').next().unwrap_or(app).into(),
                unconfined: false,
                offers: Vec::new(),
                surface: None,
            })
            .collect()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::apps::services::LaunchPlace,
    ) -> Result<(), String> {
        Ok(())
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
}

/// What the Notes package publishes, as its signed surface.
const SURFACE: &str =
    "[[resources]]\nkind = \"notes.notebook\"\nlabel = \"Notebook\"\nversion = 1\n";

/// A launcher holding the reader from the Notes package: its row is offered
/// with what its package publishes, so an agent generates its types before
/// the app opens.
struct Surfaced;

impl crate::apps::services::LauncherServices for Surfaced {
    fn applications(&self) -> Vec<crate::apps::services::LaunchableApplication> {
        vec![crate::apps::services::LaunchableApplication {
            capabilities: Vec::new(),
            package_id: NOTES.into(),
            display_name: "Notes".into(),
            unconfined: false,
            offers: Vec::new(),
            surface: Some(SURFACE.into()),
        }]
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::apps::services::LaunchPlace,
    ) -> Result<(), String> {
        Ok(())
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
}

/// An agent `agent` acting for `person` in `workspace`.
fn agent_caller(agent: &str, person: &str, workspace: &str) -> Caller {
    Caller::local_person(person.to_owned())
        .delegated_to(agent)
        .in_workspace(workspace.to_owned())
}

/// A workspace owned by a person, whose agent `scribe` may not open Sheets:
/// the bound its record keeps, as the document holds it.
fn workspace() -> Workspace {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(14), "Home");
    workspace.policy.set_owner("owner");
    workspace.deny(
        Grantee::agent("scribe", "owner"),
        &TreePath::root().app(SHEETS),
        KindSet::ALL,
    );
    workspace
}

/// A broker hosting `workspaces` with `apps` installed; `attached` says
/// whether the session's workspaces reach it at all.
fn broker(
    workspaces: Vec<Workspace>,
    apps: Vec<&'static str>,
    attached: bool,
) -> Arc<AppBrokerState> {
    broker_with(Arc::new(Installed(apps)), workspaces, attached)
}

/// As [`broker`], with `launcher` holding the apps.
fn broker_with(
    launcher: Arc<dyn crate::apps::services::LauncherServices>,
    workspaces: Vec<Workspace>,
    attached: bool,
) -> Arc<AppBrokerState> {
    let mut tree = WorkspaceTree::default();
    tree.workspaces.extend(workspaces);
    let tree = tree.shared();
    let (state, _bridge) = AppBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        attached.then(|| tree.authority()),
        None,
        None,
        crate::apps::services::ProcessServices {
            launcher: Some(launcher),
            ..Default::default()
        },
    );
    state
}

/// The installed apps `caller` is told of in `workspace`, as the agent surface
/// asks for them.
fn installed(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    workspace: &str,
) -> Result<CataloguePage, ActionCallError> {
    state
        .action_query(
            caller,
            &CatalogueQuery {
                provider: Some(workspace_provider(workspace)),
                resource_name: Some(APPLICATION_RESOURCE_KIND.into()),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the broker answers")
}

/// Who publishes in `workspace`, as the agent surface asks for the running apps.
fn publishers(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
) -> Result<CataloguePage, ActionCallError> {
    state
        .action_query(
            caller,
            &CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the broker answers")
}

/// Notes running in `workspace` as peer 9, publishing `actions`.
fn notes_running(
    workspace: &str,
    actions: Vec<super::super::PublishedAction>,
) -> super::super::PublishedSurface {
    super::super::PublishedSurface {
        peer_id: 9,
        app_id: NOTES.into(),
        route: tilo_broker_core::CrdtRoute::record(NOTES, workspace, "notes"),
        actions,
        live: true,
    }
}

tilo_test_env::scenario! {
    fn an_agents_list_of_apps;

    phase an_installed_apps_row_carries_what_its_package_publishes;
    {
        let workspace = workspace();
        let workspace_id = workspace.id().to_string();
        let state = broker_with(Arc::new(Surfaced), vec![workspace], true);
        let agent = agent_caller("scribe", "owner", &workspace_id);
        let page = installed(&state, &agent, &workspace_id).expect("the workspace is reachable");
        let row = page
            .rows
            .iter()
            .find(|row| row.resource.id == format!("app.{NOTES}"))
            .expect("Notes is installed");
        assert_eq!(
            row.surface.as_deref(),
            Some(SURFACE),
            "the row carries the surface its package publishes, before the app opens"
        );
    }

    phase an_agent_is_told_every_installed_app_and_which_it_may_open;
    {
        let workspace = workspace();
        let workspace_id = workspace.id().to_string();
        let state = broker(vec![workspace], vec![NOTES, SHEETS], true);
        let agent = agent_caller("scribe", "owner", &workspace_id);

        let page = installed(&state, &agent, &workspace_id).expect("the workspace is reachable");
        let listed: Vec<(String, Option<String>)> = page
            .rows
            .iter()
            .map(|row| {
                (
                    row.resource.id.clone(),
                    row.action
                        .as_ref()
                        .map(|action| action.address.action.clone()),
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                (
                    format!("app.{NOTES}"),
                    Some(APPLICATION_OPEN_ACTION.to_owned())
                ),
                (format!("app.{SHEETS}"), None),
            ],
            "Notes, which its person may open, opens; Sheets, which its record          does not let it open, is named without the action"
        );

        // The person is offered both to open, as before.
        let owner = Caller::local_person("owner").in_workspace(&workspace_id);
        let page = installed(&state, &owner, &workspace_id).expect("the owner reaches it");
        drop(state);
        assert!(
            page.rows.iter().all(|row| row.action.is_some()) && page.rows.len() == 2,
            "{:?}",
            page.rows
        );
    }

    phase an_agent_in_a_workspace_with_nothing_reads_an_empty_list;
    {
        let workspace = workspace();
        let workspace_id = workspace.id().to_string();
        let state = broker(vec![workspace], Vec::new(), true);
        let agent = agent_caller("scribe", "owner", &workspace_id);

        let page = publishers(&state, &agent).expect("reachable, with nothing running");
        assert!(page.providers.is_empty() && page.rows.is_empty());
        let page = installed(&state, &agent, &workspace_id).expect("reachable, with nothing installed");
        drop(state);
        assert!(page.rows.is_empty());
    }

    phase an_agent_that_cannot_reach_its_workspace_is_told_why;
    {
        let reason = |result: Result<CataloguePage, ActionCallError>| match result {
            Err(ActionCallError::Unavailable { reason }) => reason,
            other => panic!("an unreachable workspace answered {other:?}"),
        };
        let workspace = workspace();
        let workspace_id = workspace.id().to_string();

        // Not a member: the person removed it, or never admitted it here.
        let state = broker(vec![workspace.clone()], vec![NOTES], true);
        let stranger = agent_caller("scribe", "outsider", &workspace_id);
        let why = reason(publishers(&state, &stranger));
        assert!(
            why.contains("scribe") && why.contains("no longer a member of workspace Home"),
            "{why}"
        );
        let why = reason(installed(&state, &stranger, &workspace_id));
        assert!(why.contains("no longer a member"), "{why}");

        // The workspace it was admitted to is gone from this device.
        let elsewhere = Workspace::new(WorkspaceId::from_u128(15), "Elsewhere");
        let gone = agent_caller("scribe", "owner", &elsewhere.id().to_string());
        let why = reason(publishers(&state, &gone));
        assert!(why.contains("no longer on this device"), "{why}");

        // A person's own caller is not diagnosed: what it may not see is absent.
        let person = Caller::local_person("someone").in_workspace(&workspace_id);
        let answered = publishers(&state, &person);
        drop(state);
        assert!(answered.is_ok(), "{answered:?}");

        // The session's workspaces never reached the broker.
        let detached = broker(vec![workspace], vec![NOTES], false);
        let agent = agent_caller("scribe", "owner", &workspace_id);
        let why = reason(publishers(&detached, &agent));
        drop(detached);
        assert!(why.contains("not attached"), "{why}");
    }

    phase a_running_app_is_still_offered_to_open;
    // A running app is opened as a person opens it: its open brings its window forward. An agent listing what it may do finds
    // the same open the person has.
    {
        let workspace = workspace();
        let workspace_id = workspace.id().to_string();
        let state = broker(vec![workspace], vec![NOTES], true);
        state.replace_published_surfaces_for_test(vec![notes_running(&workspace_id, Vec::new())]);
        let owner = Caller::local_person("owner").in_workspace(&workspace_id);
        let opens = |state: &Arc<AppBrokerState>| {
            installed(state, &owner, &workspace_id)
                .expect("the owner reaches it")
                .rows
                .iter()
                .any(|row| {
                    row.action
                        .as_ref()
                        .is_some_and(|action| action.address.action == APPLICATION_OPEN_ACTION)
                })
        };
        assert!(opens(&state), "a window of it is up");
        let agent = agent_caller("scribe", "owner", &workspace_id);
        let agent_opens = installed(&state, &agent, &workspace_id)
            .expect("the agent reaches it")
            .rows
            .iter()
            .any(|row| {
                row.action
                    .as_ref()
                    .is_some_and(|action| action.address.action == APPLICATION_OPEN_ACTION)
            });
        assert!(agent_opens, "the agent is offered the open the person is");
    }

    phase an_agent_naming_an_action_by_its_app_hears_whose_it_is;
    // Named by the app alone, an action is looked up wherever the app publishes it: an effect
    // only the person's screen shows is told of as theirs, and a control the window draws is
    // never named to an agent at all.
    {
        let workspace = workspace();
        let workspace_id = workspace.id().to_string();
        let state = broker(vec![workspace], vec![NOTES], true);
        let published = |resource: &str, kind: &str, action: &str| super::super::PublishedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: resource.into(),
            action: action.into(),
            resource_kind: kind.into(),
            resource_label: "Notes".into(),
            action_label: action.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract::screen(),
            view: None,
            input: super::super::PublishedInput::None,
        };
        state.replace_published_surfaces_for_test(vec![notes_running(
            &workspace_id,
            vec![
                published("notes.deep.page", "notes.page", "new-page"),
                published("tilo.control:new", "tilo.control", "invoke"),
            ],
        )]);
        let agent = agent_caller("scribe", "owner", &workspace_id);
        let named = |action: &str| {
            state
                .action_query(
                    &agent,
                    &CatalogueQuery {
                        provider: Some("9".into()),
                        action: Some(action.into()),
                        ..CatalogueQuery::default()
                    },
                )
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("the broker answers")
                .expect("the agent may ask")
                .rows
        };
        let rows = named("new-page");
        let refusal = rows
            .first()
            .and_then(|row| row.action.as_ref())
            .and_then(|action| action.contract.availability.refusal());
        assert!(
            matches!(refusal, Some(("private_view", _))),
            "{rows:?}"
        );
        assert!(named("invoke").is_empty(), "a control is never named to an agent");
    }

    phase who_is_listed_an_effect_is_what_it_declares;
    // An effect declared to only change the screen is never listed to an agent and is told of
    // as the person's when named. One that writes and answers nothing is the agent's to call
    // where its grants let it write, and is listed refused where they do not: whether it
    // answers a value says nothing of whose it is.
    {
        let mut workspace = workspace();
        workspace.deny(
            Grantee::agent("clerk", "owner"),
            &TreePath::root().app(NOTES),
            KindSet::OUTSIDE,
        );
        let workspace_id = workspace.id().to_string();
        let state = broker(vec![workspace], vec![NOTES], true);
        let published = |action: &str, contract: tilo_actions::ActionContract| {
            super::super::PublishedAction {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                resource: "notes.notebook".into(),
                action: action.into(),
                resource_kind: "notes.notebook".into(),
                resource_label: "Notebook".into(),
                action_label: action.into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                operation_kind: tilo_actions::OperationKind::Effect,
                params: Vec::new(),
                contract,
                view: None,
                input: super::super::PublishedInput::None,
            }
        };
        let note = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            ..tilo_actions::ActionContract::default()
        };
        state.replace_published_surfaces_for_test(vec![notes_running(
            &workspace_id,
            vec![
                published("show-drafts", tilo_actions::ActionContract::screen()),
                published("note", note),
            ],
        )]);
        let named = |agent: &str, action: &str| {
            state
                .action_query(
                    &agent_caller(agent, "owner", &workspace_id),
                    &CatalogueQuery {
                        provider: Some("9".into()),
                        action: Some(action.into()),
                        ..CatalogueQuery::default()
                    },
                )
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("the broker answers")
                .expect("the agent may ask")
                .rows
                .into_iter()
                .filter_map(|row| row.action)
                .map(|action| action.contract.availability)
                .collect::<Vec<_>>()
        };
        let listed = |agent: &str, action: &str| {
            let page = state
                .action_query(
                    &agent_caller(agent, "owner", &workspace_id),
                    &CatalogueQuery {
                        provider: Some("9".into()),
                        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                        ..CatalogueQuery::default()
                    },
                )
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("the broker answers")
                .expect("the agent may ask");
            page.rows
                .into_iter()
                .filter_map(|row| row.action)
                .find(|found| found.address.action == action)
                .map(|found| found.contract.availability)
        };
        assert_eq!(listed("scribe", "show-drafts"), None, "never listed");
        let shown = named("scribe", "show-drafts");
        assert!(
            matches!(shown.first().and_then(|a| a.refusal()), Some(("private_view", _))),
            "{shown:?}"
        );
        assert_eq!(
            listed("scribe", "note"),
            Some(tilo_actions::Availability::Available),
            "the agent may write"
        );
        let refused = listed("clerk", "note").expect("listed, refused");
        assert!(
            matches!(refused.refusal(), Some((code, _)) if code != "private_view"),
            "{refused:?}"
        );
    }

    phase a_running_apps_live_directory_is_what_tilo_ls_lists_it_by;
    // `tilo_ls` lists the publishers whose records a caller may walk: an app whose instance
    // publishes a directory is browsable by parent, and its notebook is reached there rather
    // than through the rows its window happens to show.
    {
        let workspace = workspace();
        let workspace_id = workspace.id().to_string();
        let state = broker(vec![workspace], vec![NOTES], true);
        state.replace_published_surfaces_for_test(vec![notes_running(
            &workspace_id,
            Vec::new(),
        )]);
        state.replace_action_directory_roots_for_test(vec![super::super::ActionDirectoryRoot {
            peer_id: 9,
            app_id: NOTES.into(),
            cluster_id: workspace_id.clone(),
            document_id: "notes".into(),
        }]);
        // `tilo_ls apps` asks who publishes, and finds Notes browsable by its directory.
        let owner = Caller::local_person("owner").in_workspace(&workspace_id);
        let page = publishers(&state, &owner).expect("the owner reaches it");
        let listed = page
            .providers
            .iter()
            .find(|provider| provider.app_id == NOTES && provider.provider == "9")
            .expect("Notes is listed among the publishers");
        assert!(
            listed.browsable,
            "a live directory makes the app browsable, so an agent walks to its records"
        );
        // A caller who may not act in the workspace is told nothing is there, never an
        // empty list that reads as "nothing publishes".
        let stranger = agent_caller("scribe", "outsider", &workspace_id);
        assert!(
            matches!(publishers(&state, &stranger), Err(ActionCallError::Unavailable { .. })),
            "a denied caller sees no providers"
        );
    }
}
