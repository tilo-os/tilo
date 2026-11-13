#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

//! A run's address: the resource Console publishes for it, the view a link
//! opens, and the link itself, round-tripping through the link grammar.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tilo_console::{App, ConsoleEffect};
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::prelude::*;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, RemoteActionMessage, RemoteActionTarget,
    ResourceViewLocator,
};
use ui::tilo_actions::{InvocationId, InvocationUpdate, ResourceSpec};
use ui::tilo_reference::link::{AppId, Link, Mode, RecordSegment, Scope, WebEntries, WebEntry};
use ui::{RemoteActions, RemoteActionsGuard};

const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
const ENTRY: &str = "https://tilo.so/";
/// The run the console's answers carry: `r1`.
const RUN: &str = "console.run:r1";

fn english() {
    ui::i18n::I18nManager::init("en-US");
}

fn shows(app: &mut TestApp<App>, target: &str) -> bool {
    app.harness().targets().contains_key(target)
}

/// Whether the window's bar carries the shell's way back: a run's page or an agent's card stands
/// in place of its list.
fn shows_back(app: &mut TestApp<App>) -> bool {
    app.frame();
    ui::window_toolbar::declared().is_some_and(|bar| {
        bar.leading
            .iter()
            .any(|action| action.id == ui::widget::WINDOW_BACK)
    })
}

/// The reads and writes the window asked for since last asked.
fn asked(app: &mut TestApp<App>) -> Vec<InvokeRequest> {
    app.frame();
    app.harness()
        .take_collaboration_requests()
        .into_iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            } => Some(request),
            _ => None,
        })
        .collect()
}

/// The request to `target` among `requests`; a missing one fails where the
/// test names it.
#[track_caller]
fn find(requests: &[InvokeRequest], target: &RemoteActionTarget) -> InvokeRequest {
    requests
        .iter()
        .find(|request| request.target == *target)
        .cloned()
        .unwrap_or_else(|| panic!("the window asks for {target:?}"))
}

fn answer(app: &mut TestApp<App>, invocation: InvocationId, value: serde_json::Value) {
    app.harness()
        .handle_collaboration_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(
                    invocation,
                    InvocationUpdate::Item {
                        invocation,
                        sequence: 1,
                        value,
                    },
                )),
            },
        });
    app.frame();
}

fn agent() -> wire::Author {
    wire::Author {
        name: "Claude Code #2".into(),
        identity: "agent:claude".into(),
        relation: wire::Relation::Software,
        on_behalf_of: Some("Ada".into()),
        kind: ui::tilo_actions::ActorKind::Agent,
        ..wire::Author::default()
    }
}

fn run() -> wire::AgentRun {
    wire::AgentRun {
        id: "r1".into(),
        agent: agent(),
        workspace: Some("Launch".into()),
        instruction: "Tidy the plan".into(),
        decision: None,
        started_ms: 1,
        last_ms: 2,
        range: wire::Range {
            from: "aa".into(),
            to: "ab".into(),
        },
        actions: Vec::new(),
        undo_blocked: None,
        now: None,
    }
}

fn the_door() -> door::Door {
    door::Door {
        endpoint: "https://mcp.example".into(),
        grants: vec![door::Grant {
            id: "g1".into(),
            agent: "Claude Code".into(),
            runs: Vec::new(),
            ..door::Grant::default()
        }],
        workspaces: Vec::new(),
    }
}

/// A workspace host answering view lookups with its locator, announcing
/// `entry` as the web page its links open.
struct Host {
    entry: Option<String>,
    asked: Mutex<Vec<CatalogueQuery>>,
}

impl ActionHost for Host {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let provider = query.provider.clone().unwrap_or_default();
        self.asked.lock().unwrap().push(query);
        Pending::ready(Ok(CataloguePage {
            locator: Some(ResourceViewLocator {
                origin: "https://host.example".into(),
                workspace: WORKSPACE.into(),
                host_device: "ab".repeat(32),
                provider,
                web_entry: self.entry.clone(),
            }),
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        Pending::ready(Err(ActionHostError::NotPermitted))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Ok(()))
    }
}

fn serve(entry: Option<&str>) -> (Arc<Host>, RemoteActionsGuard) {
    let host = Arc::new(Host {
        entry: entry.map(str::to_owned),
        asked: Mutex::default(),
    });
    let guard = RemoteActions::enter(host.clone(), Caller::local_person("owner"));
    (host, guard)
}

/// The resource `id` among `resources`, at any depth.
fn published(app: &mut TestApp<App>, id: &str) -> ResourceSpec {
    fn find_in(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            (resource.id.as_str() == id)
                .then(|| resource.clone())
                .or_else(|| find_in(&resource.children, id))
        })
    }
    let resources = app.harness().resources();
    find_in(&resources, id).unwrap_or_else(|| panic!("{id} is published"))
}

/// Open the console on a session whose runs read answers with `run`, or
/// whose answers the caller gives.
fn open() -> TestApp<App> {
    TestApp::<App>::for_person("ada").expect("open")
}

/// Answer `requests`' runs read with `runs` and its door read.
fn answer_reads(app: &mut TestApp<App>, requests: &[InvokeRequest], runs: Vec<wire::AgentRun>) {
    let runs_read = find(requests, &wire::target(wire::RUNS));
    let door_read = find(requests, &door::target(door::DOOR));
    answer(
        app,
        runs_read.invocation,
        serde_json::to_value(wire::Runs {
            total: runs.len(),
            runs,
            pending: false,
            mutations: true,
        })
        .unwrap(),
    );
    answer(
        app,
        door_read.invocation,
        serde_json::to_value(the_door()).unwrap(),
    );
}

/// Answer the reads the window asks for as it opens.
fn answered_with(app: &mut TestApp<App>, runs: Vec<wire::AgentRun>) {
    let requests = asked(app);
    answer_reads(app, &requests, runs);
}

/// Open the console with its reads answered with the run `r1`.
fn opened() -> TestApp<App> {
    let mut app = open();
    answered_with(&mut app, vec![run()]);
    app
}

#[test]
fn a_run_is_published_as_its_address_with_a_primary_view_and_a_link() {
    english();
    let mut app = opened();
    let resource = published(&mut app, RUN);
    assert_eq!(resource.kind, "console.run");
    assert_eq!(
        resource.label, "Tidy the plan",
        "the run's own words name its address"
    );
    let [view] = &resource.views[..] else {
        panic!("a run opens in one view, not {:?}", resource.views);
    };
    assert_eq!(view.id, "run");
    assert!(
        view.primary,
        "a run's page is the view its link opens by default"
    );
    assert_eq!(
        view.open_action,
        ConsoleEffect::VIEW_RUN.id,
        "the view is opened by the effect that shows the run"
    );
    // What a link's open reads: the open action carries the view, so a
    // window that follows the link finds it and invokes it.
    let resources = app.harness().resources();
    let opened = ui::tilo_actions::door::survey(&resources)
        .into_iter()
        .find(|descriptor| {
            descriptor.address.resource == RUN && descriptor.address.action == view.open_action
        })
        .unwrap_or_else(|| panic!("{RUN} exports the view's open action"));
    assert_eq!(opened.kind, ui::tilo_actions::OperationKind::View);
    let descriptor = opened.view.expect("the open action carries its view");
    assert_eq!(descriptor.id, "run");
    assert!(descriptor.primary);
    assert_eq!(descriptor.open_action, view.open_action);
    for action in ["share-link", "share-invite-link"] {
        assert!(
            resource.actions.iter().any(|spec| spec.id == action),
            "a run offers {action}: {:?}",
            resource
                .actions
                .iter()
                .map(|spec| &spec.id)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn opening_a_runs_view_shows_that_runs_page() {
    english();
    let mut app = opened();
    assert!(
        shows(&mut app, "console-filter-agent"),
        "the window opens on every run"
    );
    let view = published(&mut app, RUN).views[0].clone();
    app.call_on(RUN, &view.open_action, json!({}))
        .expect("open the run");
    assert!(
        shows_back(&mut app),
        "the run's page stands in place of every run"
    );
    assert!(
        shows(&mut app, "console-run-copy-link"),
        "the page offers the run's link"
    );
    assert!(
        !shows(&mut app, "console-filter-agent"),
        "the filters are the list's, not the page's"
    );
}

#[test]
fn a_runs_link_parses_back_to_that_run() {
    english();
    // The host is served before the window opens: the window's root keeps the
    // host it started with.
    let (_host, _guard) = serve(Some(ENTRY));
    let mut app = open();
    let answer = app
        .call_on(RUN, "share-link", json!({}))
        .expect("ask for the link")
        .expect("the host answers with a link");
    let url = answer["url"].as_str().expect("a url");
    assert!(
        url.contains("console.run:r1"),
        "the link names the run in one self-describing segment: {url}"
    );

    let entries = WebEntries::new(vec![WebEntry::new(ENTRY).unwrap()]);
    let link = Link::parse(url, &entries).unwrap();
    let target = &link.target;
    // A run is read from the workspace's history, which the desktop's
    // session serves, so its link opens the desktop at the run.
    assert_eq!(link.mode, Mode::Desktop, "{url}");
    assert_eq!(target.app, Some(AppId::new(App::ID)), "{url}");
    assert_eq!(
        target.record,
        Some(RecordSegment::new(app.record().as_str()).unwrap()),
        "{url}"
    );
    let resource = target.resource.as_ref().expect("the run's address");
    assert_eq!(resource.kind, "console.run", "{url}");
    assert_eq!(resource.id, RUN, "{url}");
    assert_eq!(
        target.view, None,
        "a run's page is its primary view, which a link leaves out: {url}"
    );
    let Scope::Room { id, .. } = &target.scope else {
        panic!("a link opens in the workspace's session: {url}");
    };
    assert_eq!(id.to_string(), WORKSPACE.to_lowercase(), "{url}");
}

#[test]
fn a_runs_address_is_answered_before_the_history_read_lands() {
    english();
    // A fresh window: its runs read has not answered, so no run is listed.
    let mut app = open();
    let requests = asked(&mut app);
    assert!(
        !requests.is_empty(),
        "a fresh window asks the history for its runs"
    );
    // The run's address still resolves, as the link's open does before the
    // read lands.
    app.call_on(RUN, ConsoleEffect::VIEW_RUN.id, json!({}))
        .expect("open the run a link names");
    assert!(
        !shows(&mut app, "console-filter-agent"),
        "the run's page opened, not every run"
    );

    // The read lands: the page fills in, without the person navigating again.
    answer_reads(&mut app, &requests, vec![run()]);
    assert!(
        shows_back(&mut app),
        "the run's page shows the run once the history answers"
    );
}

#[test]
fn a_run_the_history_does_not_list_says_so() {
    english();
    let mut app = open();
    answered_with(&mut app, Vec::new());
    app.call_on(RUN, ConsoleEffect::VIEW_RUN.id, json!({}))
        .expect("open the run a link names");
    assert!(
        shows(&mut app, "console-run-missing"),
        "a run that is not here says so rather than falling back to the list"
    );
    assert!(
        shows_back(&mut app),
        "the way back to every run stays in the window's bar"
    );
}
