//! What a session's agent is told about the apps of the workspace it was
//! admitted to: every installed app, the ones it may not open by name alone,
//! and, when it cannot reach the workspace at all, why, never an empty list
//! that reads as "there are no apps".

use super::super::{AppBrokerState, BrokerChannels, FanoutState, PeerPublisher};
use crate::test_support::{
    Grants as _, KindSet, Subject, TreePath, Workspace, WorkspaceSlot, WorkspaceTree,
};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_KIND, ActionHostError, Caller, CataloguePage,
    CatalogueQuery, workspace_provider,
};

const NOTES: &str = "so.tilo.notes";
const SHEETS: &str = "so.tilo.sheets";

/// A launcher with `apps` installed.
struct Installed(Vec<&'static str>);

impl crate::services::LauncherServices for Installed {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        self.0
            .iter()
            .map(|app| crate::services::LaunchableApplication {
                capabilities: Vec::new(),
                package_id: (*app).into(),
                display_name: app.rsplit('.').next().unwrap_or(app).into(),
                unconfined: false,
                offers: Vec::new(),
            })
            .collect()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::services::LaunchPlace,
    ) -> Result<(), String> {
        Ok(())
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
}

/// An agent inside a turn: `person`'s message in a conversation it takes part
/// in caused what it asks, in `workspace`. Nothing an agent does stands
/// outside one.
fn turn_caller(agent: &str, person: &str, workspace: &str) -> Caller {
    Caller::agent_turn(
        agent,
        person,
        tilo_action_policy::turn::Turn::new("chat/workspace/channels/1", "m1"),
    )
    .in_workspace(workspace.to_owned())
}

/// A workspace owned by a person, whose agent `scribe` may not open Sheets:
/// the bound its record keeps, as the document holds it.
fn workspace() -> Workspace {
    let mut workspace = Workspace::new(WorkspaceSlot(14), "Home");
    workspace.set_owner("owner");
    workspace.deny(
        Subject::agent("scribe"),
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
        crate::services::HostServices {
            launcher: Some(Arc::new(Installed(apps))),
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
) -> Result<CataloguePage, ActionHostError> {
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
) -> Result<CataloguePage, ActionHostError> {
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

#[test]
fn an_agent_is_told_every_installed_app_and_which_it_may_open() {
    let workspace = workspace();
    let workspace_id = workspace.id().to_string();
    let state = broker(vec![workspace], vec![NOTES, SHEETS], true);
    let agent = turn_caller("scribe", "owner", &workspace_id);

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

#[test]
fn an_agent_in_a_workspace_with_nothing_reads_an_empty_list() {
    let workspace = workspace();
    let workspace_id = workspace.id().to_string();
    let state = broker(vec![workspace], Vec::new(), true);
    let agent = turn_caller("scribe", "owner", &workspace_id);

    let page = publishers(&state, &agent).expect("reachable, with nothing running");
    assert!(page.providers.is_empty() && page.rows.is_empty());
    let page = installed(&state, &agent, &workspace_id).expect("reachable, with nothing installed");
    drop(state);
    assert!(page.rows.is_empty());
}

#[test]
fn an_agent_that_cannot_reach_its_workspace_is_told_why() {
    let reason = |result: Result<CataloguePage, ActionHostError>| match result {
        Err(ActionHostError::Unavailable { reason }) => reason,
        other => panic!("an unreachable workspace answered {other:?}"),
    };
    let workspace = workspace();
    let workspace_id = workspace.id().to_string();

    // Not a member: the person removed it, or never admitted it here.
    let state = broker(vec![workspace.clone()], vec![NOTES], true);
    let stranger = turn_caller("scribe", "outsider", &workspace_id);
    let why = reason(publishers(&state, &stranger));
    assert!(
        why.contains("scribe") && why.contains("no longer a member of workspace Home"),
        "{why}"
    );
    let why = reason(installed(&state, &stranger, &workspace_id));
    assert!(why.contains("no longer a member"), "{why}");

    // The workspace it was admitted to is gone from this device.
    let elsewhere = Workspace::new(WorkspaceSlot(15), "Elsewhere");
    let gone = turn_caller("scribe", "owner", &elsewhere.id().to_string());
    let why = reason(publishers(&state, &gone));
    assert!(why.contains("no longer on this device"), "{why}");

    // A person's own caller is not diagnosed: what it may not see is absent.
    let person = Caller::local_person("someone").in_workspace(&workspace_id);
    let answered = publishers(&state, &person);
    drop(state);
    assert!(answered.is_ok(), "{answered:?}");

    // The session's workspaces never reached the broker.
    let detached = broker(vec![workspace], vec![NOTES], false);
    let agent = turn_caller("scribe", "owner", &workspace_id);
    let why = reason(publishers(&detached, &agent));
    drop(detached);
    assert!(why.contains("not attached"), "{why}");
}

/// An app running only without a window (started for an action that shows
/// nothing) is still opened: its open gives that instance its window. One
/// with a window is not offered to open again.
#[test]
fn an_app_running_without_a_window_is_still_offered_to_open() {
    let workspace = workspace();
    let workspace_id = workspace.id().to_string();
    let state = broker(vec![workspace], vec![NOTES], true);
    state.replace_published_surfaces_for_test(vec![super::super::PublishedSurface {
        peer_id: 9,
        app_id: NOTES.into(),
        route: tilo_broker_core::CrdtRoute::record(NOTES, &workspace_id, "notes"),
        actions: Vec::new(),
        live: true,
    }]);
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
    assert!(!opens(&state), "a window of it is up");
    state
        .launchable
        .windowless_peers
        .lock()
        .expect("windowless")
        .insert(9);
    assert!(opens(&state), "it runs with no window");
}
