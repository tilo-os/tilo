//! What a session's agent is told about the apps of the workspace it was
//! admitted to: every installed app, the ones it may not open by name alone,
//! and, when it cannot reach the workspace at all, why, never an empty list
//! that reads as "there are no apps".

use super::super::{BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
use crate::test_support::{Grants as _, KindSet, Subject, Workspace, WorkspaceId, WorkspaceTree};
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

/// A workspace owned by a person, with agent `scribe` admitted as an
/// observer (it reads the room) and let use Notes.
fn workspace() -> Workspace {
    let mut workspace = Workspace::new(WorkspaceId(14), "Home");
    workspace.set_owner("owner");
    workspace.admit_agent("scribe", KindSet::READ);
    workspace.allow_app(Subject::agent("scribe"), NOTES, KindSet::ALL);
    workspace
}

/// A broker hosting `workspaces` with `apps` installed; `attached` says
/// whether the session's workspaces reach it at all.
fn broker(
    workspaces: Vec<Workspace>,
    apps: Vec<&'static str>,
    attached: bool,
) -> Arc<CapnpBrokerState> {
    let mut tree = WorkspaceTree::default();
    tree.workspaces.extend(workspaces);
    let tree = tree.shared();
    let (state, _bridge) = CapnpBrokerState::new_configured(
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

/// The installed apps `caller` is told of in `room`, as the agent surface
/// asks for them.
fn installed(
    state: &Arc<CapnpBrokerState>,
    caller: &Caller,
    room: &str,
) -> Result<CataloguePage, ActionHostError> {
    state
        .action_query(
            caller,
            &CatalogueQuery {
                provider: Some(workspace_provider(room)),
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

/// Who publishes in `room`, as the agent surface asks for the running apps.
fn publishers(
    state: &Arc<CapnpBrokerState>,
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
    let room = workspace.room().to_string();
    let state = broker(vec![workspace], vec![NOTES, SHEETS], true);
    let agent = Caller::local_agent("scribe").in_room(&room);

    let page = installed(&state, &agent, &room).expect("the workspace is reachable");
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
        "Notes, which it may use, opens; Sheets is named without the action"
    );

    // The person is offered both to open, as before.
    let owner = Caller::local_person("owner").in_room(&room);
    let page = installed(&state, &owner, &room).expect("the owner reaches it");
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
    let room = workspace.room().to_string();
    let state = broker(vec![workspace], Vec::new(), true);
    let agent = Caller::local_agent("scribe").in_room(&room);

    let page = publishers(&state, &agent).expect("reachable, with nothing running");
    assert!(page.providers.is_empty() && page.rows.is_empty());
    let page = installed(&state, &agent, &room).expect("reachable, with nothing installed");
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
    let room = workspace.room().to_string();

    // Not a member: the person removed it, or never admitted it here.
    let state = broker(vec![workspace.clone()], vec![NOTES], true);
    let stranger = Caller::local_agent("intruder").in_room(&room);
    let why = reason(publishers(&state, &stranger));
    assert!(
        why.contains("intruder") && why.contains("no longer a member of workspace Home"),
        "{why}"
    );
    let why = reason(installed(&state, &stranger, &room));
    assert!(why.contains("no longer a member"), "{why}");

    // The workspace it was admitted to is gone from this device.
    let elsewhere = Workspace::new(WorkspaceId(15), "Elsewhere");
    let gone = Caller::local_agent("scribe").in_room(elsewhere.room().to_string());
    let why = reason(publishers(&state, &gone));
    assert!(why.contains("no longer on this device"), "{why}");

    // A person's own caller is not diagnosed: what it may not see is absent.
    let person = Caller::local_person("someone").in_room(&room);
    let answered = publishers(&state, &person);
    drop(state);
    assert!(answered.is_ok(), "{answered:?}");

    // The session's workspaces never reached the broker.
    let detached = broker(vec![workspace], vec![NOTES], false);
    let agent = Caller::local_agent("scribe").in_room(&room);
    let why = reason(publishers(&detached, &agent));
    drop(detached);
    assert!(why.contains("not attached"), "{why}");
}
