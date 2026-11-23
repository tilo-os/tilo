//! The Store's side of the workspace's tree and this device's package cache, one real user flow
//! at a time. The provider answers the session's action API directly, the way a desktop's broker
//! does in process, so the worker's own reads, changes and waits are what is exercised.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::Duration;

use ui::RemoteActions;
use ui::tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, Completer, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest,
    Pending, ReceiptStatus, RemoteActionMessage, ResourceAddress, ResourceCopy,
    ResourceReadRequest, ResourceReads, ResourceWake, WORKSPACE_APPS, WatchScope, pending,
    workspace_governance_child,
};
use ui::tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionOutput, ExecutionLocus,
    InputSpec, InvocationId, InvocationUpdate, OperationKind,
};

use super::packages::{self, Installed};
use super::{Snapshot, State, handle};
use crate::model::{ActionKind, StoreItem, StoreItemVersion, StoreSource, StoreTrust};
use package_actions::{PackageRow, RowState};

const WORKSPACE: &str = "7f0a0d38-0f7b-4f1e-9a3c-3f2c1a7f0001";
const DEVICE: &str = "devices/11111111111111111111111111111111/packages";

/// A workspace's tree and a device's package cache, answered as one provider.
struct Fake {
    apps: Mutex<BTreeMap<String, FakeApp>>,
    rows: Mutex<BTreeMap<String, PackageRow>>,
    wakes: Mutex<Vec<ResourceWake>>,
    changes: Mutex<Vec<Completer<Result<(), ActionCallError>>>>,
    /// Refuse every change, as a workspace that does not let this person install apps.
    refuse: Mutex<bool>,
    /// Answer the next change as cancelled.
    cancel: Mutex<bool>,
    /// The package files handed to the workspace, by their BLAKE3.
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    /// The arguments of every install the workspace took.
    installs: Mutex<Vec<serde_json::Map<String, serde_json::Value>>>,
    /// What the device's package cache answers `runtime-offer` with: the offer, or `null`.
    offer: Mutex<serde_json::Value>,
    /// The arguments of every `runtime-fetch` the cache took.
    fetches: Mutex<Vec<serde_json::Map<String, serde_json::Value>>>,
    /// What the cache answers `catalogue` with, or its failure.
    catalogue: Mutex<Result<serde_json::Value, String>>,
    me: OnceLock<Weak<Fake>>,
}

struct FakeApp {
    version: String,
    pinned: bool,
}

impl Fake {
    fn start() -> Arc<Self> {
        let fake = Arc::new(Self {
            apps: Mutex::default(),
            rows: Mutex::default(),
            wakes: Mutex::default(),
            changes: Mutex::default(),
            refuse: Mutex::default(),
            cancel: Mutex::default(),
            files: Mutex::default(),
            installs: Mutex::default(),
            offer: Mutex::new(serde_json::Value::Null),
            fetches: Mutex::default(),
            catalogue: Mutex::new(Ok(serde_json::json!({"entries": []}))),
            me: OnceLock::new(),
        });
        let _ = fake.me.set(Arc::downgrade(&fake));
        fake
    }

    fn arc(&self) -> Arc<Fake> {
        self.me
            .get()
            .and_then(Weak::upgrade)
            .expect("the provider outlives its reads")
    }

    fn apps_resource() -> String {
        workspace_governance_child(WORKSPACE, Some(WORKSPACE_APPS))
    }

    fn app_resource(app: &str) -> String {
        format!("{}/{app}", Self::apps_resource())
    }

    /// This device has fetched `app`: its row is installed.
    fn installed(&self, app: &str) {
        if let Some(row) = self.rows.lock().expect("rows").get_mut(app) {
            row.state = RowState::Installed;
        }
        self.moved();
    }

    /// This device has fetched `bytes` of `app` so far.
    fn fetching(&self, app: &str, bytes: u64) {
        if let Some(row) = self.rows.lock().expect("rows").get_mut(app)
            && let RowState::Fetching { bytes: seen, .. } = &mut row.state
        {
            *seen = bytes;
        }
        self.moved();
    }

    fn pinned(&self, app: &str) -> bool {
        self.apps
            .lock()
            .expect("apps")
            .get(app)
            .is_some_and(|app| app.pinned)
    }

    fn version(&self, app: &str) -> Option<String> {
        self.apps
            .lock()
            .expect("apps")
            .get(app)
            .map(|app| app.version.clone())
    }

    fn running(&self, app: &str) -> bool {
        self.apps.lock().expect("apps").contains_key(app)
    }

    /// The workspace runs `app` at `version`, and this device has it.
    fn put(&self, app: &str, version: &str) {
        self.apps.lock().expect("apps").insert(
            app.to_owned(),
            FakeApp {
                version: version.to_owned(),
                pinned: false,
            },
        );
        self.rows.lock().expect("rows").insert(
            app.to_owned(),
            PackageRow {
                app: app.to_owned(),
                version: version.to_owned(),
                root: "ab".repeat(32),
                state: RowState::Installed,
                about: None,
                size_bytes: 0,
            },
        );
        self.moved();
    }

    fn set_pinned(&self, app: &str, pinned: bool) {
        if let Some(entry) = self.apps.lock().expect("apps").get_mut(app) {
            entry.pinned = pinned;
        }
        self.moved();
    }

    /// The device's package cache offers `offer` for every runtime it is asked about.
    fn offers(&self, offer: &serde_json::Value) {
        *self.offer.lock().expect("offer") = offer.clone();
    }

    fn installs(&self) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.installs.lock().expect("installs").clone()
    }

    fn fetches(&self) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.fetches.lock().expect("fetches").clone()
    }

    /// The device's package cache's answers to its reading and runtime actions.
    fn cache(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        match action {
            "catalogue" => self.catalogue.lock().expect("catalogue").clone(),
            "runtime-offer" => {
                let offer = self.offer.lock().expect("offer").clone();
                let asked = entry_text(arguments, "runtime");
                let offered = offer.get("runtime").and_then(serde_json::Value::as_str);
                Ok(serde_json::json!({
                    "offer": if offered == Some(asked.as_str()) { offer } else { serde_json::Value::Null }
                }))
            }
            "runtime-fetch" => {
                let offer = self.offer.lock().expect("offer").clone();
                let field = |name: &str| {
                    offer
                        .get(name)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                if entry_text(arguments, "package") != field("tapp_root_hash_hex")
                    || entry_text(arguments, "publisher") != field("publisher_pubkey_hex")
                {
                    return Err("the repository offers another package".into());
                }
                self.fetches
                    .lock()
                    .expect("fetches")
                    .push(arguments.clone());
                self.rows.lock().expect("rows").insert(
                    field("package_id"),
                    PackageRow {
                        app: field("package_id"),
                        version: field("version"),
                        root: field("tapp_root_hash_hex"),
                        state: RowState::Installed,
                        about: None,
                        size_bytes: 0,
                    },
                );
                self.moved();
                Ok(serde_json::json!({}))
            }
            other => Err(format!("{other} is no action of the package cache")),
        }
    }

    fn refuses(&self) {
        *self.refuse.lock().expect("refuse") = true;
    }

    fn cancels(&self) {
        *self.cancel.lock().expect("cancel") = true;
    }

    /// A row changed: its readers read again, and the tree's watchers hear it.
    fn moved(&self) {
        for wake in self.wakes.lock().expect("wakes").drain(..) {
            if wake.waiting() {
                wake.wake();
            }
        }
        for change in self.changes.lock().expect("changes").drain(..) {
            change.complete(Ok(()));
        }
    }

    fn tree_rows(&self) -> Vec<CatalogueRow> {
        let apps = self.apps.lock().expect("apps");
        let resource = Self::apps_resource();
        let mut rows = Vec::new();
        for (app, entry) in apps.iter() {
            let id = Self::app_resource(app);
            let verbs: &[&str] = if entry.pinned {
                &["unpin", "uninstall"]
            } else {
                &["update", "pin", "uninstall"]
            };
            for verb in verbs {
                let mut row = CatalogueRow {
                    provider: "tilo.workspace".into(),
                    app_id: "tilo.workspace".into(),
                    parent_resource: resource.clone(),
                    resource: CatalogueResource {
                        id: id.clone(),
                        kind: "workspace.permissions".into(),
                        label: app.clone(),
                        presentation: None,
                    },
                    ..CatalogueRow::default()
                };
                row.action = Some(descriptor(&id, verb));
                rows.push(row);
            }
        }
        rows
    }

    fn change(&self, request: &InvokeRequest) -> Result<InvocationUpdate, String> {
        let invocation = request.invocation;
        let address = &request.target.address;
        if *self.refuse.lock().expect("refuse") {
            return Err("this workspace does not let you add apps".into());
        }
        if std::mem::take(&mut *self.cancel.lock().expect("cancel")) {
            return Ok(InvocationUpdate::Cancelled {
                invocation,
                message: None,
            });
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            _ => serde_json::Map::new(),
        };
        let answer = |value: serde_json::Value| InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        };
        let succeeded = answer(serde_json::json!({}));
        if address.resource == DEVICE {
            return self.cache(&address.action, &arguments).map(answer);
        }
        if address.action == "upload" {
            let file = entry_text(&arguments, "file");
            let offset = arguments.get("offset").and_then(serde_json::Value::as_u64);
            let data = hex::decode(entry_text(&arguments, "data"))
                .map_err(|_| "a piece is hex".to_owned())?;
            let mut files = self.files.lock().expect("files");
            let bytes = files.entry(file).or_default();
            if offset != Some(bytes.len() as u64) {
                return Err("a piece starts where the file so far ends".into());
            }
            bytes.extend(data);
            return Ok(succeeded);
        }
        if address.action == "install" {
            if let Some(file) = arguments.get("file").and_then(serde_json::Value::as_str) {
                let files = self.files.lock().expect("files");
                let bytes = files
                    .get(file)
                    .ok_or_else(|| format!("no file {file} was handed over"))?;
                if blake3::hash(bytes).to_hex().as_str() != file {
                    return Err("the file handed over is not the one the install names".into());
                }
            }
            self.installs
                .lock()
                .expect("installs")
                .push(arguments.clone());
            let text = |name: &str| {
                arguments
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| format!("the install names no {name}"))
            };
            let app = text("app")?.to_owned();
            let version = text("version")?.to_owned();
            self.apps.lock().expect("apps").insert(
                app.clone(),
                FakeApp {
                    version: version.clone(),
                    pinned: false,
                },
            );
            self.rows.lock().expect("rows").insert(
                app.clone(),
                PackageRow {
                    app,
                    version,
                    root: "ab".repeat(32),
                    state: RowState::Fetching {
                        bytes: 0,
                        total: Some(4_000),
                    },
                    about: None,
                    size_bytes: 0,
                },
            );
            self.moved();
            return Ok(succeeded);
        }
        let app = address
            .resource
            .rsplit_once('/')
            .map(|(_, app)| app.to_owned())
            .ok_or_else(|| "an app's resource".to_owned())?;
        match address.action.as_str() {
            "update" => {
                let mut apps = self.apps.lock().expect("apps");
                let entry = apps.get_mut(&app).ok_or_else(|| "no such app".to_owned())?;
                if entry.pinned {
                    return Err(format!("{app} is pinned: unpin it first"));
                }
                if let Some(version) = arguments.get("version").and_then(serde_json::Value::as_str)
                {
                    entry.version = version.to_owned();
                }
                drop(apps);
                if let Some(row) = self.rows.lock().expect("rows").get_mut(&app) {
                    row.version = entry_text(&arguments, "version");
                    row.state = RowState::Fetching {
                        bytes: 0,
                        total: Some(4_000),
                    };
                }
                self.moved();
            }
            "pin" | "unpin" => {
                let mut apps = self.apps.lock().expect("apps");
                let entry = apps.get_mut(&app).ok_or_else(|| "no such app".to_owned())?;
                entry.pinned = address.action == "pin";
                drop(apps);
                self.moved();
            }
            "uninstall" => {
                self.apps.lock().expect("apps").remove(&app);
                self.rows.lock().expect("rows").remove(&app);
                self.moved();
            }
            other => return Err(format!("{other} is no change of an app")),
        }
        Ok(succeeded)
    }
}

fn entry_text(arguments: &serde_json::Map<String, serde_json::Value>, name: &str) -> String {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn descriptor(resource: &str, action: &str) -> ActionDescriptor {
    ActionDescriptor {
        address: ActionAddress::new(resource, action),
        semantic_identity: None,
        resource_kind: "workspace.permissions".into(),
        resource_label: resource.into(),
        action_label: action.into(),
        keywords: Vec::new(),
        did: None,
        icon: None,
        description: None,
        state: ui::tilo_actions::ActionState::default(),
        locus: ExecutionLocus::default(),
        kind: OperationKind::Effect,
        contract: ActionContract::default(),
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

impl ActionProvider for Fake {
    fn serves(&self, provider: &str) -> bool {
        matches!(provider, "tilo.workspace" | "so.tilo.packages")
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        match query.provider.as_deref() {
            Some("so.tilo.packages") => Pending::ready(Ok(CataloguePage {
                revision: "packages".into(),
                rows: vec![CatalogueRow {
                    provider: "so.tilo.packages".into(),
                    app_id: "so.tilo.packages".into(),
                    resource: CatalogueResource {
                        id: DEVICE.into(),
                        kind: "devices.packages".into(),
                        label: "Packages".into(),
                        presentation: None,
                    },
                    ..CatalogueRow::default()
                }],
                exhausted: true,
                ..CataloguePage::default()
            })),
            Some("tilo.workspace") => Pending::ready(Ok(CataloguePage {
                revision: "tree".into(),
                rows: self.tree_rows(),
                exhausted: true,
                ..CataloguePage::default()
            })),
            _ => Pending::ready(Ok(CataloguePage::default())),
        }
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        let status = match self.change(&request) {
            Ok(InvocationUpdate::Succeeded { output, .. }) => ReceiptStatus::Completed { output },
            Ok(InvocationUpdate::Cancelled { .. }) => ReceiptStatus::Cancelled { message: None },
            Ok(_) => ReceiptStatus::Cancelled { message: None },
            Err(message) => ReceiptStatus::Failed {
                error: ActionFailure {
                    code: "refused".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        };
        Pending::ready(Ok(InvocationReceipt { invocation, status }))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "a change to the workspace's apps runs to its end".into(),
        }))
    }

    fn changed(
        &self,
        _caller: &Caller,
        _scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let (change, hearing) = pending();
        self.changes.lock().expect("changes").push(change);
        Some(hearing)
    }

    fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        Some(Arc::new(Reads { fake: self.arc() }))
    }
}

/// The fake's record reads: this device's rows, and nothing else.
struct Reads {
    fake: Arc<Fake>,
}

impl ResourceReads for Reads {
    fn read(
        &self,
        _caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if address.app != "so.tilo.packages" {
            return Ok(ResourceCopy::Absent);
        }
        let rows: Vec<PackageRow> = self
            .fake
            .rows
            .lock()
            .expect("rows")
            .values()
            .cloned()
            .collect();
        let reading = if address.record == DEVICE {
            serde_json::to_value(rows)
        } else {
            let Some(app) = address
                .record
                .strip_prefix(DEVICE)
                .and_then(|rest| rest.strip_prefix('/'))
            else {
                return Ok(ResourceCopy::Absent);
            };
            match rows.into_iter().find(|row| row.app == app) {
                Some(row) => serde_json::to_value(row),
                None => return Ok(ResourceCopy::Absent),
            }
        };
        reading
            .map(|value| ResourceCopy::reading(value, None))
            .map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        ui::tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        _caller: &Caller,
        _address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        self.fake.wakes.lock().expect("wakes").push(wake.clone());
        Ok(())
    }
}

fn item(app: &str, name: &str, version: &str) -> StoreItem {
    StoreItem {
        bot: None,
        awaiting_developer: false,
        listing: crate::model::Listing::default(),
        id: app.into(),
        name: name.into(),
        description: String::new(),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: Vec::new(),
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: "Tilo".into(),
        },
        trust: StoreTrust::PublisherSigned,
        versions: vec![StoreItemVersion {
            version: version.into(),
            released_at_unix: 0,
            download_url: Some("https://apps.tilo.so/x.tapp".into()),
            root_hash_hex: Some("ab".repeat(32)),
            surface_hex: Some("cd".repeat(32)),
            size_bytes: 4_000,
            runtime: None,
        }],
        publisher_pubkey_hex: Some("ef".repeat(32)),
        installed: crate::InstalledState::Not,
    }
}

fn snapshots() -> (super::Snapshots, mpsc::Receiver<Snapshot>) {
    let (sender, received) = mpsc::channel();
    (
        super::Snapshots {
            sender,
            wake: ui::WakeHandle::detached(),
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        },
        received,
    )
}

/// A worker state over `items`, reading the provider the test entered.
fn state_with(items: Vec<StoreItem>) -> State {
    let mut state = super::tests::state_with_items(items, crate::search::StoreDirectory::default());
    state.packages =
        packages::AppPackages::new(ui::RemoteActions::current(), Some(WORKSPACE.into()));
    state
}

fn last(received: &mpsc::Receiver<Snapshot>) -> Snapshot {
    received.try_iter().last().expect("a snapshot")
}

/// Answer what the provider has to say until `done` holds, as the worker's loop does.
fn settle(state: &mut State, done: impl Fn(&State) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let _ = state.packages.advance();
        if done(state) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the provider did not answer"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The one app `state` lists.
fn the_app(state: &State) -> &StoreItem {
    state.base_items.first().expect("the app")
}

tilo_test_env::scenario! {
    fn getting_an_app_is_the_workspace_s_choice_and_opening_waits_for_it;

    phase a_get_records_the_version_and_the_card_shows_its_bytes;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        let app = item("so.tilo.notes", "Notes", "1.4.0");
        let mut state = state_with(vec![app.clone()]);
        settle(&mut state, |state| state.packages.installed() == &Installed::default());
        let (snapshots, received) = snapshots();
        let (done, answered) = mpsc::channel();
        let (alive, outcome) = handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Get),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        assert!(alive);
        assert!(outcome.is_none(), "a Get answers when the workspace took it");
        assert!(super::advance_packages(&mut state, &snapshots));
        let finished = answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        let said = &finished.status;
        assert!(said.contains("to this workspace"), "{said}");
        assert_eq!(fake.version("so.tilo.notes").as_deref(), Some("1.4.0"));
        assert!(fake.running("so.tilo.notes"));
        settle(&mut state, |state| {
            state.packages.installed().progress("so.tilo.notes").is_some()
        });
        super::advance_packages(&mut state, &snapshots);
        let snapshot = last(&received);
        assert_eq!(
            snapshot.installs.get("so.tilo.notes").and_then(ui::arriving::Progress::fraction),
            Some(0.0),
            "the card shows the fetch from its first byte"
        );
        fake.fetching("so.tilo.notes", 1_500);
        settle(&mut state, |state| {
            state
                .packages
                .installed()
                .progress("so.tilo.notes")
                .is_some_and(|progress| progress.received == 1_500)
        });
        super::advance_packages(&mut state, &snapshots);
        let snapshot = last(&received);
        assert_eq!(
            snapshot.installs.get("so.tilo.notes").and_then(ui::arriving::Progress::fraction),
            Some(0.375)
        );
        settle(&mut state, |state| {
            state.packages.installed().workspace.contains_key("so.tilo.notes")
        });
        super::apply_installed(&mut state);
        assert_eq!(the_app(&state).action_kind(true), ActionKind::Open);
    }

    phase a_press_on_open_before_the_package_arrives_waits_and_is_taken;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        let app = item("so.tilo.notes", "Notes", "1.4.0");
        let mut state = state_with(vec![app.clone()]);
        settle(&mut state, |state| state.packages.installed() == &Installed::default());
        let (snapshots, _received) = snapshots();
        let (done, answered) = mpsc::channel();
        let (_, outcome) = handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Get),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        assert!(outcome.is_none());
        super::advance_packages(&mut state, &snapshots);
        let _ = answered.recv_timeout(Duration::from_secs(5)).unwrap();

        let (done, answered) = mpsc::channel();
        let (_, outcome) = handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Open),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        assert!(outcome.is_none(), "the press waits for the package");
        fake.installed("so.tilo.notes");
        settle(&mut state, |state| state.packages.installed().root("so.tilo.notes").is_some());
        super::advance_packages(&mut state, &snapshots);
        let finished = answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        let launch = finished.launch.expect("the press is taken once the app is here");
        assert_eq!(launch.app_id, "so.tilo.notes");
        let root = "ab".repeat(32);
        assert_eq!(launch.package_root.as_deref(), Some(root.as_str()));
    }
}

tilo_test_env::scenario! {
    fn a_running_app_updates_uninstalls_and_keeps_or_releases_itself;

    phase an_app_the_workspace_runs_can_move_to_its_newest_version;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        let app = item("so.tilo.notes", "Notes", "2.0.0");
        let mut state = state_with(vec![app.clone()]);
        fake.put("so.tilo.notes", "1.0.0");
        settle(&mut state, |state| {
            state.packages.installed().workspace.contains_key("so.tilo.notes")
        });
        super::apply_installed(&mut state);
        assert!(
            the_app(&state).installed.update_available(),
            "the workspace runs 1.0.0 and the source offers 2.0.0"
        );
        let (snapshots, _received) = snapshots();
        let (done, answered) = mpsc::channel();
        let (_, outcome) = handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Update),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        assert!(outcome.is_none());
        super::advance_packages(&mut state, &snapshots);
        answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(fake.version("so.tilo.notes").as_deref(), Some("2.0.0"));
    }

    phase a_pinned_app_refuses_an_update;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        let app = item("so.tilo.notes", "Notes", "2.0.0");
        let mut state = state_with(vec![app.clone()]);
        fake.put("so.tilo.notes", "1.0.0");
        fake.set_pinned("so.tilo.notes", true);
        settle(&mut state, |state| {
            state.packages.installed().workspace.contains_key("so.tilo.notes")
        });
        super::apply_installed(&mut state);
        assert!(the_app(&state).installed.is_pinned());
        let (snapshots, _received) = snapshots();
        let (done, answered) = mpsc::channel();
        handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Update),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        super::advance_packages(&mut state, &snapshots);
        let refusal = answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err();
        assert!(refusal.contains("unpin it first"), "{refusal}");
        assert_eq!(fake.version("so.tilo.notes").as_deref(), Some("1.0.0"));
    }

    phase an_app_is_kept_and_released_and_removed_from_the_workspace;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        let app = item("so.tilo.notes", "Notes", "1.0.0");
        let mut state = state_with(vec![app.clone()]);
        fake.put("so.tilo.notes", "1.0.0");
        settle(&mut state, |state| {
            state.packages.installed().workspace.contains_key("so.tilo.notes")
        });
        super::apply_installed(&mut state);
        let (snapshots, _received) = snapshots();
        for (action, pinned) in [
            (ActionKind::MarkInstalled, true),
            (ActionKind::Unmark, false),
        ] {
            let (done, answered) = mpsc::channel();
            handle(
                super::Request::Act(Box::new(app.clone()), action),
                &mut state,
                &snapshots,
                &mut Some(done),
            );
            super::advance_packages(&mut state, &snapshots);
            answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            assert_eq!(fake.pinned("so.tilo.notes"), pinned, "{action:?}");
        }
        let (done, answered) = mpsc::channel();
        handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Remove),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        super::advance_packages(&mut state, &snapshots);
        answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert!(!fake.running("so.tilo.notes"));
    }
}

tilo_test_env::scenario! {
    fn a_refused_or_cancelled_change_is_said_and_leaves_nothing_behind;

    phase a_workspace_that_does_not_let_this_person_install_says_so;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        fake.refuses();
        let app = item("so.tilo.notes", "Notes", "1.0.0");
        let mut state = state_with(vec![app.clone()]);
        settle(&mut state, |state| state.packages.installed() == &Installed::default());
        let (snapshots, _received) = snapshots();
        let (done, answered) = mpsc::channel();
        handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Get),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        super::advance_packages(&mut state, &snapshots);
        let refusal = answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err();
        assert!(refusal.contains("does not let you add apps"), "{refusal}");
        assert!(!fake.running("so.tilo.notes"));
        assert!(state.packages.installed().workspace.is_empty());
    }

    phase a_cancelled_change_ends_the_wait_without_an_install;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        fake.cancels();
        let app = item("so.tilo.notes", "Notes", "1.0.0");
        let mut state = state_with(vec![app.clone()]);
        settle(&mut state, |state| state.packages.installed() == &Installed::default());
        let (snapshots, _received) = snapshots();
        let (done, answered) = mpsc::channel();
        handle(
            super::Request::Act(Box::new(app.clone()), ActionKind::Get),
            &mut state,
            &snapshots,
            &mut Some(done),
        );
        super::advance_packages(&mut state, &snapshots);
        let ended = answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err();
        assert!(ended.to_lowercase().contains("cancelled"), "{ended}");
        assert!(!fake.running("so.tilo.notes"));
        assert!(state.running.is_none(), "nothing keeps running");
    }
}

/// The worker's own loop, not only one pass of it: a Get asked of a spawned worker ends, and the
/// window hears about it.
#[test]
fn a_spawned_worker_adds_an_app_and_says_so() {
    let fake = Fake::start();
    let _entered = RemoteActions::enter(
        Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
        Caller::local_person("ada").in_workspace(WORKSPACE),
    );
    let worker = super::StoreWorker::spawn_with(
        crate::search::StoreDirectory::default(),
        ui::WakeHandle::detached(),
        ui::RemoteActions::current(),
        Some(WORKSPACE.into()),
        Vec::new(),
    );
    let (done, answered) = mpsc::channel();
    assert!(worker.request_job(
        super::Request::Act(
            Box::new(item("so.tilo.notes", "Notes", "1.4.0")),
            ActionKind::Get
        ),
        done
    ));
    let finished = answered
        .recv_timeout(Duration::from_secs(20))
        .expect("the worker answers the Get")
        .expect("the workspace takes it");
    let said = &finished.status;
    assert!(said.contains("to this workspace"), "{said}");
    assert!(fake.running("so.tilo.notes"));
    let mut heard = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !heard {
        heard = worker.try_snapshot().is_some();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(heard, "the window hears the worker's snapshot");
}

const RUNTIME: &str = "tilo-runtime/2026.10.4-chromium";

/// The runtime build the repository `main` offers.
fn runtime_offer() -> serde_json::Value {
    serde_json::json!({
        "package_id": "so.tilo.runtime.chromium", "display_name": "Chromium", "version": "4.0.0",
        "registry": "main", "download_url": "https://example.invalid/engine.tapp",
        "tapp_root_hash_hex": "11".repeat(32), "tapp_size_bytes": 6656,
        "publisher_pubkey_hex": "22".repeat(32), "runtime": RUNTIME,
        "trust": "publisher-signed"
    })
}

/// The workspace's one repository, `main`, enabled.
fn main_repository() -> (tilo_test_env::ScopedEnv, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("registries.toml"),
        "[[registry]]\nname = \"main\"\nbase_url = \"https://registry.invalid\"\nowner_pubkey_hex = \"ab\"\nenabled = true\n",
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_STORE_CONFIG_DIR", dir.path());
    (env, dir)
}

/// The item the worker's review of `runtime` showed.
fn reviewed_runtime(received: &mpsc::Receiver<Snapshot>) -> StoreItem {
    received
        .try_iter()
        .find_map(|snapshot| match snapshot.detail {
            Some(super::DetailUpdate::OpenedReference(item)) => Some(item),
            _ => None,
        })
        .expect("the runtime's review")
}

tilo_test_env::scenario! {
    fn a_runtime_review_installs_only_the_exact_offer;

    phase reviewing_a_runtime_installs_nothing_and_installing_fetches_exactly_its_offer;
    {
        let fake = Fake::start();
        let _entered = RemoteActions::enter(
            Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace(WORKSPACE),
        );
        let _repository = main_repository();
        fake.offers(&runtime_offer());
        let mut state = state_with(Vec::new());
        let (snapshots, received) = snapshots();
        let (_, result) = handle(
            super::Request::RuntimeDetails {
                runtime: RUNTIME.into(),
                registry: None,
            },
            &mut state,
            &snapshots,
            &mut None,
        );
        assert!(result.unwrap().is_ok());
        assert!(fake.fetches().is_empty(), "a review installs nothing");
        let item = reviewed_runtime(&received);
        assert_eq!(item.versions[0].runtime.as_deref(), Some(RUNTIME));
        assert_eq!(item.action_kind(true), ActionKind::RuntimeInstall);
        let (_, outcome) = handle(
            super::Request::Act(Box::new(item), ActionKind::RuntimeInstall),
            &mut state,
            &snapshots,
            &mut None,
        );
        let finished = outcome.unwrap().unwrap();
        assert!(
            finished.launch.is_none(),
            "a runtime must never launch as an application"
        );
        let fetches = fake.fetches();
        assert_eq!(fetches.len(), 1);
        assert_eq!(entry_text(&fetches[0], "registry"), "main");
        assert_eq!(entry_text(&fetches[0], "runtime"), RUNTIME);
        assert_eq!(entry_text(&fetches[0], "package"), "11".repeat(32));
        assert_eq!(entry_text(&fetches[0], "publisher"), "22".repeat(32));
        assert!(fake.installs().is_empty(), "a runtime is no app of the workspace");
    }

    phase a_runtime_review_marks_only_the_exact_installed_root_installed;
    {
        for (root, installed) in [("11".repeat(32), true), ("33".repeat(32), false)] {
            let fake = Fake::start();
            let _entered = RemoteActions::enter(
                Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
                Caller::local_person("ada").in_workspace(WORKSPACE),
            );
            let _repository = main_repository();
            fake.offers(&runtime_offer());
            fake.rows.lock().expect("rows").insert(
                "so.tilo.runtime.chromium".into(),
                PackageRow {
                    app: "so.tilo.runtime.chromium".into(),
                    version: "4.0.0".into(),
                    root,
                    state: RowState::Installed,
                    about: None,
                    size_bytes: 0,
                },
            );
            let mut state = state_with(Vec::new());
            settle(&mut state, |state| {
                state.packages.installed().device.contains_key("so.tilo.runtime.chromium")
            });
            let item = super::runtime_details(RUNTIME, None, &mut state).unwrap();
            assert_eq!(item.installed.is_installed(), installed);
            assert_eq!(
                item.unavailable_action(ActionKind::RuntimeInstall)
                    .map(|(code, _)| code),
                installed.then_some("runtime_already_installed")
            );
        }
    }

    phase a_missing_foreign_or_malformed_runtime_offer_is_refused;
    {
        let valid = runtime_offer();
        let changed = |name: &str, value: serde_json::Value| {
            let mut offer = valid.clone();
            offer[name] = value;
            offer
        };
        for offer in [
            serde_json::Value::Null,
            changed("runtime", "tilo-runtime/2026.10.5-chromium".into()),
            changed("trust", "untrusted".into()),
            changed("registry", "foreign".into()),
            changed("tapp_size_bytes", 0.into()),
            changed("tapp_root_hash_hex", "xx".repeat(32).into()),
            changed("publisher_pubkey_hex", "x".into()),
        ] {
            let fake = Fake::start();
            let _entered = RemoteActions::enter(
                Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
                Caller::local_person("ada").in_workspace(WORKSPACE),
            );
            let _repository = main_repository();
            fake.offers(&offer);
            let mut state = state_with(Vec::new());
            assert!(
                super::runtime_details(RUNTIME, None, &mut state).is_err(),
                "{offer}"
            );
            assert!(fake.fetches().is_empty());
        }
    }
}

tilo_test_env::scenario! {
    fn a_package_import_is_reviewed_by_the_worker_before_its_install;

    phase a_signed_package_is_reviewed_then_handed_over_and_installed;
    {
        check_import_worker(crate::package_import::tests::signed_fixture());
    }

    phase an_offered_package_is_reviewed_then_handed_over_and_installed;
    {
        check_import_worker(crate::package_import::tests::catalogue_fixture());
    }
}

/// Opening a package file installs nothing; installing it names exactly the reviewed package,
/// with its bytes handed to the workspace first; once the offer is gone it installs nothing.
fn check_import_worker((bytes, item): (Vec<u8>, StoreItem)) {
    let fake = Fake::start();
    let _entered = RemoteActions::enter(
        Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
        Caller::local_person("ada").in_workspace(WORKSPACE),
    );
    let reviewed =
        crate::package_import::review(bytes.clone(), std::slice::from_ref(&item)).unwrap();
    let mut state = state_with(vec![item.clone()]);
    let (snapshots, received) = snapshots();
    let (alive, outcome) = handle(
        super::Request::ImportPackage(crate::package_import::Input::Bytes(bytes.clone())),
        &mut state,
        &snapshots,
        &mut None,
    );
    assert!(alive && outcome.unwrap().is_ok());
    assert!(fake.installs().is_empty(), "opening never installs");
    assert!(received.try_iter().any(|snapshot| matches!(
        snapshot.detail,
        Some(super::DetailUpdate::ImportedPackage { item: shown, .. }) if shown.id == item.id
    )));
    let (_, outcome) = handle(
        super::Request::InstallImported {
            app: item.id.clone(),
            root: "00".repeat(32),
        },
        &mut state,
        &snapshots,
        &mut None,
    );
    assert!(outcome.unwrap().is_err());
    assert!(
        fake.installs().is_empty() && fake.files.lock().expect("files").is_empty(),
        "a stale reviewed identity installs and hands over nothing"
    );
    let root = reviewed.expected.package_root.to_hex();
    let (_, outcome) = handle(
        super::Request::InstallImported {
            app: item.id.clone(),
            root: root.clone(),
        },
        &mut state,
        &snapshots,
        &mut None,
    );
    assert!(outcome.unwrap().is_ok());
    let installs = fake.installs();
    assert_eq!(installs.len(), 1);
    let file = blake3::hash(&bytes).to_hex().to_string();
    assert_eq!(entry_text(&installs[0], "app"), item.id);
    assert_eq!(
        entry_text(&installs[0], "version"),
        reviewed.expected.version
    );
    assert_eq!(entry_text(&installs[0], "package"), root);
    assert_eq!(
        entry_text(&installs[0], "publisher"),
        hex::encode(reviewed.expected.publisher)
    );
    assert_eq!(entry_text(&installs[0], "file"), file);
    assert_eq!(fake.files.lock().expect("files").get(&file), Some(&bytes));
    assert!(state.imported_package.is_none() && state.refresh_requested);
    handle(
        super::Request::ImportPackage(crate::package_import::Input::Bytes(bytes)),
        &mut state,
        &snapshots,
        &mut None,
    );
    state.base_items.clear();
    let (_, outcome) = handle(
        super::Request::InstallImported { app: item.id, root },
        &mut state,
        &snapshots,
        &mut None,
    );
    assert!(outcome.unwrap().is_err());
    assert_eq!(fake.installs().len(), 1, "an offer gone installs nothing");
}

/// A Flatpak's install names its reference on the workspace's tree, and its removal takes it off
/// again: the device's daemon does the rest.
#[test]
fn a_flatpak_is_installed_and_removed_on_the_workspace_s_tree() {
    let fake = Fake::start();
    let _entered = RemoteActions::enter(
        Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
        Caller::local_person("ada").in_workspace(WORKSPACE),
    );
    let mut chess = item("org.example.Chess", "Chess", "stable");
    chess.source = StoreSource::Flathub;
    chess.trust = StoreTrust::Flathub;
    chess.publisher_pubkey_hex = None;
    chess.versions[0].download_url = None;
    chess.versions[0].root_hash_hex = None;
    chess.versions[0].surface_hex = None;
    let mut state = state_with(vec![chess.clone()]);
    state.flatpak_available = true;
    settle(&mut state, |state| {
        state.packages.installed() == &Installed::default()
    });
    let (snapshots, _received) = snapshots();
    let (done, answered) = mpsc::channel();
    handle(
        super::Request::Act(Box::new(chess.clone()), ActionKind::FlatpakInstall),
        &mut state,
        &snapshots,
        &mut Some(done),
    );
    super::advance_packages(&mut state, &snapshots);
    answered
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let installs = fake.installs();
    assert_eq!(installs.len(), 1);
    assert_eq!(entry_text(&installs[0], "app"), "org.example.chess");
    assert_eq!(
        entry_text(&installs[0], "flatpak"),
        format!("app/org.example.Chess/{}/stable", std::env::consts::ARCH)
    );
    assert!(fake.running("org.example.chess"));
    let (done, answered) = mpsc::channel();
    handle(
        super::Request::Act(Box::new(chess), ActionKind::FlatpakRemove),
        &mut state,
        &snapshots,
        &mut Some(done),
    );
    super::advance_packages(&mut state, &snapshots);
    answered
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(!fake.running("org.example.chess"));
}

/// A repository's refusal crosses the action API as its type: the Store says why a catalogue
/// has no rows from the refusal's name, whatever its detail says.
#[test]
fn a_catalogue_refusal_crosses_the_action_api_typed() {
    let fake = Fake::start();
    let _entered = RemoteActions::enter(
        Arc::clone(&fake) as Arc<dyn ActionProvider + Send + Sync>,
        Caller::local_person("ada").in_workspace(WORKSPACE),
    );
    let mut state = state_with(Vec::new());
    let mut arguments = serde_json::Map::new();
    arguments.insert("registry".into(), "main".into());
    for (name, refusal) in [
        ("expired", super::refresh::Refusal::Expired),
        ("behind-accepted", super::refresh::Refusal::BehindAccepted),
        (
            "system-update-needed",
            super::refresh::Refusal::SystemUpdateNeeded,
        ),
    ] {
        *fake.catalogue.lock().expect("catalogue") = Ok(serde_json::json!({
            "refused": name, "detail": "worded any way at all"
        }));
        let answer =
            state
                .packages
                .cache_call_now("catalogue", arguments.clone(), Duration::from_secs(5));
        let error = super::refresh::catalogue_of(answer).unwrap_err();
        assert_eq!(error.refusal(), Some(refusal), "{name}");
    }
    *fake.catalogue.lock().expect("catalogue") = Err("the repository is offline".into());
    let answer = state
        .packages
        .cache_call_now("catalogue", arguments, Duration::from_secs(5));
    let error = super::refresh::catalogue_of(answer).unwrap_err();
    assert_eq!(error.refusal(), None);
    assert!(error.to_string().contains("offline"), "{error}");
}
