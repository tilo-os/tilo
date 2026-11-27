//! Which rows an application filter builds: the workspace provider's
//! services and a live directory asked for one application's rows, and the
//! same rows when the whole catalogue is built and filtered afterwards.

#![expect(
    clippy::future_not_send,
    reason = "a live directory's request queue is drained on the broker's LocalSet"
)]

use super::super::{
    ActionDirectoryRoot, AppBrokerState, LocalActionDirectoryRegistry, RegisteredActionDirectory,
    directory_pagination,
};
use crate::collab_broker::CrdtRoute;
use crate::services::{
    KeyboardShortcutServices, LauncherServices, ProcessServices, ShortcutLibraryRequest,
    ShortcutLibraryServices, WorkspaceGovernanceServices,
};
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueQuery, CatalogueRow, MAX_CATALOGUE_PAGE,
    Pending, PendingPoll, workspace_provider,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind,
};
use tilo_automation::SHORTCUTS_APP_ID;

/// A view action `action` on `resource`, offered to anyone the workspace lets
/// read it.
fn descriptor(resource: &str, action: &str) -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(resource, action),
        resource_kind: "workspace".into(),
        resource_label: "Workspace".into(),
        action_label: "Inspect".into(),
        keywords: Vec::new(),
        icon: None,
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
        "tilo.workspace"
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
        _: &settings_store::hardware::ShortcutConfig,
        _: &ActionAddress,
        _: Option<&str>,
    ) -> Result<tilo_actions::ActionOutput, Box<tilo_actions::ActionFailure>> {
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
    fn store(&self, _: ShortcutLibraryRequest) -> crate::services::ShortcutStore {
        Box::pin(async { Err("this test stores no shortcut".into()) })
    }
    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        Err("this test runs no shortcut".into())
    }
}

/// Two installed applications, one of them the shortcuts app.
#[derive(Default)]
struct Launcher {
    enumerations: AtomicUsize,
    lookups: AtomicUsize,
}
impl Launcher {
    fn records() -> Vec<crate::services::LaunchableApplication> {
        ["so.tilo.shortcuts", "so.tilo.notes"]
            .into_iter()
            .map(|package_id| crate::services::LaunchableApplication {
                capabilities: Vec::new(),
                package_id: package_id.into(),
                display_name: package_id.into(),
                unconfined: false,
                offers: Vec::new(),
                surface: None,
            })
            .collect()
    }
}
impl LauncherServices for Launcher {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        self.enumerations.fetch_add(1, Ordering::Relaxed);
        Self::records()
    }
    fn applications_for(&self, app_id: &str) -> Vec<crate::services::LaunchableApplication> {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        Self::records()
            .into_iter()
            .filter(|app| app.package_id == app_id)
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
    state: Arc<AppBrokerState>,
    governance: Arc<Governance>,
    keys: Arc<Keys>,
    launcher: Arc<Launcher>,
    caller: Caller,
    workspace: String,
}

/// One workspace the owner may act in, holding the shortcut `tidy`, with
/// governance, keyboard, shortcut-library and launcher services behind
/// the workspace provider.
fn fixture(keys: Vec<CatalogueRow>) -> Fixture {
    let user = [41; 32];
    let mut workspace = Workspace::new(WorkspaceId::from_u128(81), "Scoped");
    workspace.policy.set_owner(hex::encode(user));
    workspace.shortcuts.push(
        serde_json::from_value(serde_json::json!({"id":"tidy","name":"Tidy","steps":[]})).unwrap(),
    );
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let governance = Arc::new(Governance::default());
    let keys = Arc::new(Keys {
        calls: AtomicUsize::new(0),
        rows: keys,
    });
    let launcher = Arc::new(Launcher::default());
    let (state, _bridge_rx) = AppBrokerState::test_state(
        Some(tree.shared().authority()),
        ProcessServices {
            launcher: Some(launcher.clone()),
            workspace_governance: Some(governance.clone()),
            shortcut_library: Some(Arc::new(Library)),
            keyboard_shortcuts: Some(keys.clone()),
            ..Default::default()
        },
    );
    let caller = Caller::local_person(hex::encode(user)).in_workspace(workspace_id.clone());
    Fixture {
        state,
        governance,
        keys,
        launcher,
        caller,
        workspace: workspace_id,
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

fn workspace_query(workspace: &str) -> CatalogueQuery {
    CatalogueQuery {
        provider: Some(workspace_provider(workspace)),
        max_results: MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    }
}

fn scoped_query(workspace: &str, app_id: &str) -> CatalogueQuery {
    CatalogueQuery {
        app_id: Some(app_id.into()),
        ..workspace_query(workspace)
    }
}

tilo_test_env::scenario! {
    fn an_app_scoped_query_is_bounded_to_its_app;

    phase an_app_scoped_query_builds_only_that_apps_rows;
    {
        let fixture = fixture(vec![row("so.tilo.settings", "keyboard-shortcuts", "list")]);
        let all = page(
            &fixture,
            &fixture.caller,
            &workspace_query(&fixture.workspace),
        );
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
            &scoped_query(&fixture.workspace, SHORTCUTS_APP_ID),
        );
        assert_eq!(fixture.launcher.enumerations.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.launcher.lookups.load(Ordering::Relaxed), 1);
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

    phase an_app_filter_applies_before_the_page_bound;
    {
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
            &scoped_query(&fixture.workspace, "so.tilo.settings"),
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

        let all = page(
            &fixture,
            &fixture.caller,
            &workspace_query(&fixture.workspace),
        );
        drop(fixture);
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

    phase an_app_scoped_query_still_refuses_a_stranger;
    {
        let fixture = fixture(vec![row("so.tilo.settings", "keyboard-shortcuts", "list")]);
        let stranger =
            Caller::authenticated([99; 32], [98; 32]).in_workspace(fixture.workspace.clone());
        let page = page(
            &fixture,
            &stranger,
            &scoped_query(&fixture.workspace, SHORTCUTS_APP_ID),
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
        drop(fixture);
    }
}

/// One live directory row, `read` of `workspace:atlas`.
fn directory_rows() -> Vec<runtime_collab::ActionDirectoryRow> {
    let mut contract = ActionContract::view();
    // A presence view is disclosed to anyone the workspace lets read it, as the
    // production directory fixtures' Atlas row is. A plain view would be the
    // publishing person's private UI, which this fixture names no person for.
    contract.tags.push(tilo_actions::PRESENCE_VIEW_TAG.into());
    let descriptor = runtime_collab::ExportedAction {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: "workspace:atlas".into(),
        action: "read".into(),
        resource_kind: "atlas.marker".into(),
        resource_label: "Atlas".into(),
        action_label: "Read Atlas".into(),
        keywords: Vec::new(),
        icon: None,
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

impl super::super::wire::DirectorySource for Directory {
    fn query(
        &self,
        _query: runtime_collab::ActionDirectoryQuery,
        _max_bytes: u32,
    ) -> super::super::wire::Asked<runtime_collab::ActionDirectoryPage> {
        self.asked
            .pages
            .set(self.asked.pages.get().saturating_add(1));
        Box::pin(std::future::ready(Ok(
            runtime_collab::ActionDirectoryPage {
                revision: "directory-r1".into(),
                rows: self.rows.clone(),
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            },
        )))
    }

    fn resolve(
        &self,
        _target: (&str, &str, &str),
        _local_document: bool,
        _max_bytes: u32,
    ) -> super::super::wire::Asked<runtime_collab::ActionDirectoryResolution> {
        self.asked
            .targets
            .set(self.asked.targets.get().saturating_add(1));
        Box::pin(std::future::ready(Ok(match self.rows.first() {
            Some(row) => runtime_collab::ActionDirectoryResolution::Found {
                row: Box::new(row.clone()),
                revision: None,
            },
            None => runtime_collab::ActionDirectoryResolution::NotFound,
        })))
    }
}

/// A workspace whose owner may browse one live directory publishing `app_id`'s
/// rows as peer `47`'s provider.
struct DirectoryFixture {
    state: Arc<AppBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    asked: Rc<Asked>,
    caller: Caller,
    provider: String,
    app_id: String,
}

fn directory_fixture(app_id: &str) -> DirectoryFixture {
    let user = [43; 32];
    let mut workspace = Workspace::new(WorkspaceId::from_u128(83), "Live directory");
    workspace.policy.set_owner(hex::encode(user));
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let (state, bridge_rx) =
        AppBrokerState::test_state(Some(tree.shared().authority()), ProcessServices::default());
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
            route: CrdtRoute::record(app_id, workspace_id.clone(), "atlas"),
            directory: Rc::new(Directory {
                rows: directory_rows(),
                asked: Rc::clone(&asked),
            }),
        },
    );
    state.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
        peer_id: provider,
        app_id: app_id.into(),
        cluster_id: workspace_id.clone(),
        document_id: "atlas".into(),
    }]);
    DirectoryFixture {
        state,
        directories,
        asked,
        caller: Caller::local_person(hex::encode(user)).in_workspace(workspace_id),
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
    async fn answer(&self, query: &CatalogueQuery) -> Result<CataloguePage, ActionCallError> {
        let mut pending = self.state.action_query(&self.caller, query);
        tilo_test_env::within("the app-filtered directory query", async {
            loop {
                self.state.take_turn(&self.directories);
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
            drop(fixture);
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
            drop(fixture);
        })
        .await;
}

/// A step provider registered for turns: the session's own actions, which a
/// person's shortcut may run on this device.
#[derive(Default)]
struct TurnStep;

impl tilo_actions::remote::Admits for TurnStep {
    const PROVIDER: &'static str = tilo_actions::remote::session_actions::PROVIDER;
    const APP: &'static str = tilo_actions::remote::session_actions::PROVIDER;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        tilo_actions::remote::admitted::top_level(vec![ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new("desktop:chrome", "new-strip"),
            resource_kind: "desktop".into(),
            resource_label: "The shell".into(),
            action_label: "New strip".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::default(),
            kind: OperationKind::Effect,
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                ..tilo_actions::ActionContract::default()
            },
            input: InputSpec::None,
            params: vec![tilo_actions::params::ActionParam::optional(
                "name",
                tilo_actions::params::ParamType::Text,
                "What to call the strip",
            )],
            view: None,
        }])
    }

    fn start(
        &self,
        _caller: &Caller,
        _request: &tilo_actions::remote::InvokeRequest,
        _admitted: &tilo_actions::remote::Admitted,
    ) -> Result<tilo_actions::remote::Pending<tilo_actions::remote::Outcome>, String> {
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
    }
}

/// A shortcut saved from this device's own shell runs here, so its steps are held
/// to what this device offers: a step only turns reach is offered to a caller from
/// the shell, whose device key alone does not say the call is local, where a
/// shortcut save lists this device's providers.
#[tokio::test(flavor = "current_thread")]
async fn a_shortcut_saves_a_step_only_this_device_offers() {
    let user = [51; 32];
    let mut workspace = Workspace::new(WorkspaceId::from_u128(91), "Shortcut saves");
    workspace.policy.set_owner(hex::encode(user));
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let providers = tilo_workspace_service::workspaces::providers::Providers::default();
    providers.provide(
        tilo_actions::remote::session_actions::PROVIDER,
        tilo_actions::remote::session_actions::PROVIDER,
        tilo_workspace_service::workspaces::providers::Reach::Turns,
        Arc::new(TurnStep) as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    );
    let (state, _bridge_rx) = AppBrokerState::test_state(
        Some(tree.shared().authority()),
        ProcessServices {
            providers: Some(providers),
            ..Default::default()
        },
    );
    // The person's own shell: the device's key, and no app or turn origin.
    let caller = Caller::authenticated(user, [52; 32]).in_workspace(workspace_id);
    let offers_strip = |rows: Vec<CatalogueRow>| {
        rows.iter().any(|row| {
            row.app_id == tilo_actions::remote::session_actions::PROVIDER
                && row
                    .action
                    .as_ref()
                    .is_some_and(|action| action.address.action == "new-strip")
        })
    };
    let (listed, _) = state.eager_catalogue(&caller, super::RowScope::All);
    assert!(
        !offers_strip(listed),
        "a call carrying a device key alone lists no turn step"
    );
    let (listed, _) = state.device_catalogue(&caller, super::RowScope::All);
    assert!(
        offers_strip(listed),
        "a shortcut saved here lists the steps this device runs"
    );
}
