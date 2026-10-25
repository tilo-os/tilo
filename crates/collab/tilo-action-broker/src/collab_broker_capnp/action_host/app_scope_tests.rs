//! Which rows an application filter builds: the workspace provider's
//! services and a live directory asked for one application's rows, and the
//! same rows when the whole catalogue is built and filtered afterwards.

#![expect(
    clippy::future_not_send,
    reason = "a live directory's request queue is drained on the broker's LocalSet"
)]

use super::super::{
    ActionDirectoryRoot, BrokerChannels, CapnpBrokerState, FanoutState,
    LocalActionDirectoryRegistry, PeerPublisher, RegisteredActionDirectory, directory_pagination,
};
use crate::collab_broker::CrdtRoute;
use crate::services::{
    HostServices, KeyboardShortcutServices, LauncherServices, ShortcutLibraryRequest,
    ShortcutLibraryServices, WorkspaceGovernanceServices,
};
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use capnp::capability::Promise;
use runtime_collab::raw::action_directory;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    ActionHostError, Caller, CataloguePage, CatalogueQuery, CatalogueRow, MAX_CATALOGUE_PAGE,
    PendingPoll, workspace_provider,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind,
};
use tilo_automation::SHORTCUTS_APP_ID;

/// A view action `action` on `resource`, offered to anyone the room lets
/// read it.
fn descriptor(resource: &str, action: &str) -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new(resource, action),
        resource_kind: "workspace".into(),
        resource_label: "Workspace".into(),
        action_label: "Inspect".into(),
        description: None,
        locus: ExecutionLocus::default(),
        kind: OperationKind::View,
        contract: tilo_actions::ActionContract::view(),
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

fn row(app: &str, resource: &str, action: &str) -> CatalogueRow {
    CatalogueRow::for_action("", app, descriptor(resource, action))
}

/// Workspace governance, counting how often its descriptors are built.
#[derive(Default)]
struct Governance(AtomicUsize);
impl WorkspaceGovernanceServices for Governance {
    fn app_id(&self) -> &'static str {
        "so.tilo.workspace"
    }
    fn handles(&self, action: &str) -> bool {
        action == "inspect"
    }
    fn descriptors(&self, _: &Workspace) -> Vec<ActionDescriptor> {
        self.0.fetch_add(1, Ordering::Relaxed);
        vec![descriptor("workspace", "inspect")]
    }
}

/// The device's keyboard shortcuts, counting how often their rows are
/// built, and offering `rows` as its catalogue.
struct Keys {
    calls: AtomicUsize,
    rows: Vec<CatalogueRow>,
}
impl KeyboardShortcutServices for Keys {
    fn app_id(&self) -> &'static str {
        "so.tilo.settings"
    }
    fn is_resource(&self, _: &str) -> bool {
        false
    }
    fn catalogue_rows(
        &self,
        provider: &str,
        _: &settings_store::hardware::ShortcutConfig,
    ) -> Vec<CatalogueRow> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.rows
            .iter()
            .cloned()
            .map(|mut row| {
                row.provider = provider.to_owned();
                row
            })
            .collect()
    }
    fn descriptor(
        &self,
        _: &settings_store::hardware::ShortcutConfig,
        _: &ActionAddress,
    ) -> Option<ActionDescriptor> {
        None
    }
    fn apply(
        &self,
        _: &mut settings_store::hardware::ShortcutConfig,
        _: &ActionAddress,
        _: Option<&str>,
    ) -> Result<(tilo_actions::ActionOutput, bool), Box<tilo_actions::ActionFailure>> {
        Err(Box::new(tilo_actions::ActionFailure {
            code: "unavailable".into(),
            message: "this test changes no keyboard shortcut".into(),
            details: None,
            retryable: false,
        }))
    }
}

#[derive(Default)]
struct Library;
impl ShortcutLibraryServices for Library {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::default()
    }
    fn store(&self, _: &ShortcutLibraryRequest<'_>) -> Result<(), String> {
        Err("this test stores no shortcut".into())
    }
    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        Err("this test runs no shortcut".into())
    }
}

/// Two installed applications, one of them the shortcuts app.
#[derive(Default)]
struct Launcher;
impl LauncherServices for Launcher {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        ["so.tilo.shortcuts", "so.tilo.notes"]
            .into_iter()
            .map(|package_id| crate::services::LaunchableApplication {
                capabilities: Vec::new(),
                package_id: package_id.into(),
                display_name: package_id.into(),
                unconfined: false,
                offers: Vec::new(),
            })
            .collect()
    }
    fn launch(
        &self,
        _: &str,
        _: &Workspace,
        _: Option<&str>,
        _: &crate::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("this test launches nothing".into())
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
    fn closes_applications(&self) -> bool {
        false
    }
}

struct Fixture {
    state: Arc<CapnpBrokerState>,
    governance: Arc<Governance>,
    keys: Arc<Keys>,
    caller: Caller,
    room: String,
}

/// One workspace the owner may act in, holding the shortcut `tidy`, with
/// governance, keyboard, shortcut-library and launcher services behind
/// the workspace provider.
fn fixture(keys: Vec<CatalogueRow>) -> Fixture {
    let user = [41; 32];
    let mut workspace = Workspace::new(WorkspaceId(81), "Scoped");
    workspace.set_owner(hex::encode(user));
    workspace.shortcuts.push(
        serde_json::from_value(serde_json::json!({"id":"tidy","name":"Tidy","steps":[]})).unwrap(),
    );
    let room = workspace.room().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let governance = Arc::new(Governance::default());
    let keys = Arc::new(Keys {
        calls: AtomicUsize::new(0),
        rows: keys,
    });
    let (state, _bridge_rx) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared().authority()),
        None,
        None,
        HostServices {
            launcher: Some(Arc::new(Launcher)),
            workspace_governance: Some(governance.clone()),
            shortcut_library: Some(Arc::new(Library)),
            keyboard_shortcuts: Some(keys.clone()),
            ..Default::default()
        },
    );
    let caller = Caller::local_person(hex::encode(user)).in_room(room.clone());
    Fixture {
        state,
        governance,
        keys,
        caller,
        room,
    }
}

fn page(
    fixture: &Fixture,
    caller: &Caller,
    query: &CatalogueQuery,
) -> tilo_actions::remote::CataloguePage {
    let mut pending = fixture.state.action_query(caller, query);
    let PendingPoll::Ready(Ok(page)) = pending.poll_ready() else {
        panic!("an eager catalogue answers at once");
    };
    page
}

fn workspace_query(room: &str) -> CatalogueQuery {
    CatalogueQuery {
        provider: Some(workspace_provider(room)),
        max_results: MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    }
}

fn scoped_query(room: &str, app_id: &str) -> CatalogueQuery {
    CatalogueQuery {
        app_id: Some(app_id.into()),
        ..workspace_query(room)
    }
}

#[test]
fn an_app_scoped_query_builds_only_that_apps_rows() {
    let fixture = fixture(vec![row("so.tilo.settings", "keyboard-shortcuts", "list")]);
    let all = page(&fixture, &fixture.caller, &workspace_query(&fixture.room));
    assert_eq!(fixture.governance.0.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.keys.calls.load(Ordering::Relaxed), 1);
    let expected: Vec<_> = all
        .rows
        .iter()
        .filter(|row| row.app_id == SHORTCUTS_APP_ID)
        .cloned()
        .collect();
    assert!(
        expected.iter().any(|row| row
            .action
            .as_ref()
            .is_some_and(|action| action.address.resource == "app.so.tilo.shortcuts")),
        "the shortcuts app's own open row is offered"
    );
    assert!(
        expected.iter().any(|row| row
            .action
            .as_ref()
            .is_some_and(|action| action.address.action == "run")),
        "the saved shortcut's run is offered"
    );

    let scoped = page(
        &fixture,
        &fixture.caller,
        &scoped_query(&fixture.room, SHORTCUTS_APP_ID),
    );
    let governance_calls = fixture.governance.0.load(Ordering::Relaxed);
    let keyboard_calls = fixture.keys.calls.load(Ordering::Relaxed);
    drop(fixture);
    assert_eq!(
        scoped.rows, expected,
        "the same shortcut rows as filtering the whole catalogue"
    );
    assert_eq!(
        governance_calls, 1,
        "another app's query builds no governance descriptors"
    );
    assert_eq!(
        keyboard_calls, 1,
        "another app's query builds no keyboard rows"
    );
}

#[test]
fn an_app_filter_applies_before_the_page_bound() {
    // The keyboard service publishes more rows of another app than a page
    // holds, and its last row is the one asked for.
    let mut rows: Vec<_> = (0..MAX_CATALOGUE_PAGE)
        .map(|index| {
            row(
                "so.tilo.notes",
                &format!("keyboard-shortcut:{index}"),
                "rebind",
            )
        })
        .collect();
    rows.push(row("so.tilo.settings", "keyboard-shortcuts", "list"));
    let fixture = fixture(rows);

    let scoped = page(
        &fixture,
        &fixture.caller,
        &scoped_query(&fixture.room, "so.tilo.settings"),
    );
    assert_eq!(scoped.rows.len(), 1, "the wanted row is found, past a page");
    assert_eq!(
        scoped.rows[0]
            .action
            .as_ref()
            .map(|action| action.address.action.as_str()),
        Some("list")
    );
    assert!(scoped.exhausted);

    let all = page(&fixture, &fixture.caller, &workspace_query(&fixture.room));
    assert_eq!(
        all.rows.len(),
        usize::try_from(MAX_CATALOGUE_PAGE).unwrap_or(usize::MAX)
    );
    assert!(
        all.rows.iter().all(|row| row.app_id != "so.tilo.settings"),
        "unfiltered, the wanted row lies past the first page"
    );
    assert!(!all.exhausted);
}

#[test]
fn an_app_scoped_query_still_refuses_a_stranger() {
    let fixture = fixture(vec![row("so.tilo.settings", "keyboard-shortcuts", "list")]);
    let stranger = Caller::authenticated([99; 32], [98; 32]).in_room(fixture.room.clone());
    let page = page(
        &fixture,
        &stranger,
        &scoped_query(&fixture.room, SHORTCUTS_APP_ID),
    );
    assert!(
        page.rows.is_empty(),
        "narrowing by app offers a caller without a role nothing"
    );
    assert_eq!(
        fixture.governance.0.load(Ordering::Relaxed),
        0,
        "a caller with no role builds no governance descriptors"
    );
}

/// One live directory row, `read` of `workspace:atlas`.
fn directory_rows() -> Vec<runtime_collab::ActionDirectoryRow> {
    let mut contract = ActionContract::view();
    // A participant view is disclosed to anyone the room lets read it, as the
    // production directory fixtures' Atlas row is. A plain view would be the
    // publishing person's private UI, which this fixture names no person for.
    contract
        .tags
        .push(tilo_actions::PARTICIPANT_VIEW_TAG.into());
    let descriptor = runtime_collab::ExportedAction {
        semantic_identity: None,
        resource: "workspace:atlas".into(),
        action: "read".into(),
        resource_kind: "atlas.marker".into(),
        resource_label: "Atlas".into(),
        action_label: "Read Atlas".into(),
        description: None,
        operation_kind: OperationKind::View,
        params: Vec::new(),
        contract: contract.clone(),
        input: runtime_collab::ExportedInput::None,
        view: None,
    };
    vec![runtime_collab::ActionDirectoryRow {
        parent_resource: String::new(),
        resource: "workspace:atlas".into(),
        resource_kind: "atlas.marker".into(),
        resource_label: "Atlas".into(),
        presentation: None,
        action: Some("read".into()),
        descriptor: Some(descriptor),
        has_children: false,
        visibility: Some(runtime_collab::RowVisibility {
            action: "read".into(),
            contract,
            governing_kind: Some(OperationKind::View),
        }),
    }]
}

/// What a live directory was asked, counted by the test that owns it.
#[derive(Default)]
struct Asked {
    pages: Cell<usize>,
    targets: Cell<usize>,
}

/// A live directory of one application: one page of `rows`, and its first row
/// as the exact target it resolves.
struct Directory {
    rows: Vec<runtime_collab::ActionDirectoryRow>,
    asked: Rc<Asked>,
}

impl action_directory::Server for Directory {
    fn query(
        self: Rc<Self>,
        _params: action_directory::QueryParams,
        mut results: action_directory::QueryResults,
    ) -> Promise<(), capnp::Error> {
        self.asked
            .pages
            .set(self.asked.pages.get().saturating_add(1));
        Promise::from(runtime_collab::write_action_directory_page(
            results.get().init_page(),
            &runtime_collab::ActionDirectoryPage {
                revision: "directory-r1".into(),
                rows: self.rows.clone(),
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            },
        ))
    }

    fn resolve(
        self: Rc<Self>,
        _params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        self.asked
            .targets
            .set(self.asked.targets.get().saturating_add(1));
        let Some(row) = self.rows.first() else {
            results.get().init_resolution().set_not_found(());
            return Promise::ok(());
        };
        Promise::from(runtime_collab::write_action_directory_row(
            results.get().init_resolution().init_found(),
            row,
        ))
    }
}

/// A room whose owner may browse one live directory publishing `app_id`'s
/// rows as peer `47`'s provider.
struct DirectoryFixture {
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    asked: Rc<Asked>,
    caller: Caller,
    provider: String,
    app_id: String,
}

fn directory_fixture(app_id: &str) -> DirectoryFixture {
    let user = [43; 32];
    let mut workspace = Workspace::new(WorkspaceId(83), "Live directory");
    workspace.set_owner(hex::encode(user));
    let room = workspace.room().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let (state, bridge_rx) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared().authority()),
        None,
        None,
        HostServices::default(),
    );
    let asked = Rc::new(Asked::default());
    let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
    let provider = 47;
    directories.directories.borrow_mut().insert(
        provider,
        RegisteredActionDirectory {
            registration: Rc::new(uuid::Uuid::new_v4()),
            pagination: directory_pagination::DirectoryPagination::default(),
            publishing_person: None,
            app_id: app_id.into(),
            route: CrdtRoute::record(app_id, room.clone(), "atlas"),
            directory: capnp_rpc::new_client(Directory {
                rows: directory_rows(),
                asked: Rc::clone(&asked),
            }),
        },
    );
    state.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
        peer_id: provider,
        app_id: app_id.into(),
        cluster_id: room.clone(),
        record_id: "atlas".into(),
    }]);
    DirectoryFixture {
        state,
        directories,
        asked,
        caller: Caller::local_person(hex::encode(user)).in_room(room),
        provider: provider.to_string(),
        app_id: app_id.into(),
    }
}

impl DirectoryFixture {
    /// The numeric provider's page, narrowed to `app_id` when one is named.
    fn page_query(&self, app_id: Option<&str>) -> CatalogueQuery {
        CatalogueQuery {
            provider: Some(self.provider.clone()),
            app_id: app_id.map(str::to_owned),
            max_results: 8,
            ..CatalogueQuery::default()
        }
    }

    /// The numeric provider's exact target, re-resolved at `revision`.
    fn target_query(&self, app_id: Option<&str>, revision: Option<&str>) -> CatalogueQuery {
        CatalogueQuery {
            provider: Some(self.provider.clone()),
            app_id: app_id.map(str::to_owned),
            target: Some(ActionAddress::new("workspace:atlas", "read")),
            revision: revision.map(str::to_owned),
            max_results: 8,
            ..CatalogueQuery::default()
        }
    }

    /// `query`'s answer, draining the directory bridge until it arrives.
    async fn answer(&self, query: &CatalogueQuery) -> Result<CataloguePage, ActionHostError> {
        let mut pending = self.state.action_query(&self.caller, query);
        tilo_test_env::within("the app-filtered directory query", async {
            loop {
                self.state.drain_action_directory_bridge(&self.directories);
                match pending.poll_ready() {
                    PendingPoll::Ready(page) => break page,
                    PendingPoll::Abandoned => panic!("the directory query was abandoned"),
                    PendingPoll::Waiting => tokio::task::yield_now().await,
                }
            }
        })
        .await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_numeric_directory_answers_another_apps_query_without_asking_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = directory_fixture("so.tilo.atlas");
            let page = fixture
                .answer(&fixture.page_query(Some("so.tilo.notes")))
                .await
                .unwrap();
            assert!(
                page.rows.is_empty() && page.exhausted,
                "another app's rows cannot be this directory's"
            );
            assert_eq!(
                fixture.asked.pages.get(),
                0,
                "the directory is not asked for rows it cannot publish"
            );
            let target = fixture
                .answer(&fixture.target_query(Some("so.tilo.notes"), Some("r1")))
                .await
                .unwrap();
            assert!(
                target.rows.is_empty() && target.exhausted,
                "an exact target under another app is answered empty"
            );
            assert_eq!(
                fixture.asked.targets.get(),
                0,
                "an exact target the directory cannot publish is not resolved"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_numeric_directory_answers_its_own_apps_query() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = directory_fixture("so.tilo.atlas");
            let unfiltered = fixture.answer(&fixture.page_query(None)).await.unwrap();
            assert_eq!(fixture.asked.pages.get(), 1);
            assert_eq!(unfiltered.rows.len(), 1);
            let page = fixture
                .answer(&fixture.page_query(Some(fixture.app_id.as_str())))
                .await
                .unwrap();
            assert_eq!(fixture.asked.pages.get(), 2);
            assert_eq!(
                page.rows, unfiltered.rows,
                "the directory's own app keeps every row it publishes"
            );
            assert_eq!(page.rows[0].provider, fixture.provider);
            assert_eq!(page.rows[0].app_id, fixture.app_id);

            let target = fixture
                .answer(&fixture.target_query(Some(fixture.app_id.as_str()), Some("directory-r1")))
                .await
                .unwrap();
            assert_eq!(fixture.asked.targets.get(), 1);
            assert_eq!(target.rows, unfiltered.rows, "the exact target re-resolves");
            assert!(
                fixture
                    .answer(&fixture.target_query(Some(fixture.app_id.as_str()), None))
                    .await
                    .is_err(),
                "an app filter that matches still needs the revision of its target"
            );
        })
        .await;
}
