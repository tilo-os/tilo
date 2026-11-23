//! This device's package cache: the packages, runtimes and environment images its apps run from,
//! which the daemon alone writes while it runs. Its changes are the actions of provider
//! `so.tilo.packages` on resource `devices/<device>/packages` ([`package_actions::published`]),
//! answered before anyone logs the device in and after, as the device's own resource.
//!
//! Nothing here decides who may call: before a login only the device's own accounts reach the
//! daemon, and after it the device action service admits each caller against the person's grants
//! before a request reaches this provider. A file an action names is bytes the caller put first,
//! `tilo.content:<hash>`, into the device's content: the one place a device keeps what callers
//! hand its own resources, which its steps (`so.tilo.devices`) read too.
//!
//! What the cache holds reads as rows ([`package_actions::PackageRow`]), through the one record
//! read every view follows (app `so.tilo.packages`): the resource reads as the list of them,
//! `<resource>/<app>` as one, and a reader is woken when one moves, so every view of an app's
//! package (the Store's Installing, its Open) shows it arriving.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use package_actions::{PackageRow, RowState};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ContentPuts,
    InvocationLog, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    Reservation, ResourceAddress, ResourceCopy, ResourceReadRequest, ResourceReads, ResourceWake,
};
use tilo_actions::{
    ActionFailure, ActionKind, ActionOutput, ActionSpec, EncodedAction, InvocationId,
    InvocationUpdate, OperationKind, ResourceSpec,
};
use tilo_lock::MutexRecover as _;

use super::control::DeviceContent;

/// The provider the cache's actions are published under.
pub const PROVIDER: &str = "so.tilo.packages";

/// The device's package cache as an [`ActionProvider`].
pub struct PackageCache {
    resource: String,
    resources: Vec<ResourceSpec>,
    writer: Writer,
    log: Mutex<InvocationLog<Arc<Mutex<InvocationState>>>>,
}

/// What makes one change: the cache's root, and where the files it names are.
#[derive(Clone)]
struct Writer {
    /// The cache's root, which every package, runtime and image of the device's is under.
    root: PathBuf,
    content: Arc<DeviceContent>,
    rows: Arc<Rows>,
    /// One Flatpak change at a time: the state of what this device installed is read, changed and
    /// written under it, so two changes never lose each other's entry.
    flatpaks: Arc<Mutex<()>>,
    /// One change to the record of the apps the follower installed at a time.
    followed: Arc<Mutex<()>>,
}

/// The cache's rows: what its catalog has installed, and the packages on their way.
struct Rows {
    resource: String,
    root: PathBuf,
    /// The packages on their way, by app.
    fetching: Mutex<BTreeMap<String, PackageRow>>,
    /// Moves on with every change to the rows.
    revision: tokio::sync::watch::Sender<u64>,
}

impl Rows {
    /// The rows as they are now.
    fn now(&self) -> Result<Vec<PackageRow>, ActionCallError> {
        let catalog =
            package_store::Catalog::at(&self.root, package_store::SystemStore::from_env());
        let fetching: Vec<PackageRow> = self.fetching.lock_recover().values().cloned().collect();
        package_actions::rows::rows(&catalog, fetching)
            .map_err(|reason| ActionCallError::Unavailable { reason })
    }

    /// Wake every watch of the rows.
    fn moved(&self) {
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// `release`'s package is on its way, `bytes` of it here, of `total`.
    fn fetching(
        &self,
        release: &tilo_peer_state::workspace_membership::AppRelease,
        bytes: u64,
        total: Option<u64>,
    ) {
        self.fetching.lock_recover().insert(
            release.app.clone(),
            PackageRow {
                app: release.app.clone(),
                version: release.version.clone(),
                root: release.package.clone(),
                state: RowState::Fetching { bytes, total },
                about: None,
                size_bytes: 0,
            },
        );
        self.moved();
    }

    /// `app`'s package is no longer on its way: installed, or not to be had.
    fn arrived(&self, app: &str) {
        self.fetching.lock_recover().remove(app);
        self.moved();
    }
}

impl Rows {
    /// The app `address` names, `Some("")` for the rows themselves; `None` for no resource of
    /// the cache's.
    fn named<'a>(&self, address: &'a ResourceAddress) -> Option<&'a str> {
        if address.app != PROVIDER {
            return None;
        }
        let rest = address.record.strip_prefix(self.resource.as_str())?;
        if rest.is_empty() {
            Some("")
        } else {
            rest.strip_prefix('/')
        }
    }
}

impl ResourceReads for Rows {
    fn read(
        &self,
        _caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let Some(app) = self.named(address) else {
            return Ok(ResourceCopy::Absent);
        };
        // The revision first: a row that changes while they are read moves it past this one, so
        // a follow reads again.
        let revision = self.revision.borrow().to_string();
        let rows = self.now()?;
        let reading = if app.is_empty() {
            serde_json::to_value(rows)
        } else {
            match rows.into_iter().find(|row| row.app == app) {
                Some(row) => serde_json::to_value(row),
                None => return Ok(ResourceCopy::Absent),
            }
        };
        reading
            .map(|value| ResourceCopy::reading(value, Some(revision)))
            .map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        _caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        if self.named(address).is_none() {
            return Ok(());
        }
        let mut seen = self.revision.subscribe();
        let wake = wake.clone();
        tilo_rt::app().spawn(async move {
            if seen.changed().await.is_ok() {
                wake.wake();
            }
        });
        Ok(())
    }
}

/// The resource the cache of the device whose key is `device` is published as.
#[must_use]
pub fn resource(device: &[u8; 32]) -> String {
    format!("devices/{}/packages", hex::encode(device))
}

impl PackageCache {
    /// The cache of the device whose key is `device`, rooted at `root`, reading what callers put
    /// from `content`.
    #[must_use]
    pub fn new(device: &[u8; 32], root: &Path, content: Arc<DeviceContent>) -> Self {
        let resource = resource(device);
        let mut spec = ResourceSpec::new(&resource, "devices.packages", "Packages");
        for published in package_actions::published() {
            let type_id = format!("{PROVIDER}/{}", published.id);
            let encoded = type_id.clone();
            let mut action = ActionSpec::typed_operation(
                published.id,
                published.label,
                "",
                OperationKind::Effect,
                type_id,
                published.params(),
                move |_| EncodedAction {
                    type_id: encoded.clone(),
                    payload: Vec::new(),
                },
            );
            action.type_mut().contract.kind = ActionKind::Outside;
            spec.actions.push(action);
        }
        // What the cache answers about the device, changing nothing: its Flatpak installation, the
        // apps a configured Flatpak or Tilo repository offers, and a repository's runtime builds.
        for reading in package_actions::readings() {
            let type_id = format!("{PROVIDER}/{}", reading.id);
            let encoded = type_id.clone();
            let mut action = ActionSpec::typed_operation(
                reading.id,
                reading.label,
                "",
                OperationKind::Effect,
                type_id,
                reading.params(),
                move |_| EncodedAction {
                    type_id: encoded.clone(),
                    payload: Vec::new(),
                },
            );
            action.type_mut().contract.kind = ActionKind::Read;
            spec.actions.push(action);
        }
        let rows = Arc::new(Rows {
            resource: resource.clone(),
            root: root.to_owned(),
            fetching: Mutex::default(),
            revision: tokio::sync::watch::Sender::new(0),
        });
        Self {
            resource,
            resources: vec![spec],
            writer: Writer {
                root: root.to_owned(),
                content,
                rows,
                flatpaks: Arc::default(),
                followed: Arc::default(),
            },
            log: Mutex::default(),
        }
    }

    /// The cache this computer's daemon keeps, one for the daemon's life, before a login and
    /// after it: where its apps are installed, reading what callers put from the device's content.
    ///
    /// # Errors
    /// This account has no data directory to keep packages in.
    pub fn this_device(device: &[u8; 32]) -> Result<Arc<Self>, String> {
        if let Some(cache) = CACHE.get() {
            return Ok(Arc::clone(cache));
        }
        let root = cache_root().ok_or("this account has no data directory to keep packages in")?;
        let content = Arc::new(DeviceContent::at(&device_content(&root)));
        Ok(Arc::clone(CACHE.get_or_init(|| {
            Arc::new(Self::new(device, &root, content))
        })))
    }

    /// Record that `app` was opened, as the cache's `opened` change does: what was used last is
    /// evicted last.
    fn opened(&self, app: &str) {
        let opened = package_actions::Change::Opened {
            app: app.to_owned(),
        };
        if let Err(error) = self.writer.apply(opened) {
            tracing::warn!(app, %error, "the cache did not record that an app was opened");
        }
    }

    /// Whether `request` names this cache.
    fn names(&self, request: &InvokeRequest) -> bool {
        request.target.provider == PROVIDER && request.target.address.resource == self.resource
    }
}

/// Where this computer's daemon keeps its packages: the account's app store on Linux, where the
/// launcher and every app sandbox find them; on macOS, beside the rest of the daemon's state.
fn cache_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return tilo_paths::machine::account_daemon_profile().map(|profile| profile.join("packages"));
    #[cfg(not(target_os = "macos"))]
    return package_store::layout::default_user_root();
}

/// The cache this computer's daemon keeps ([`PackageCache::this_device`]).
static CACHE: OnceLock<Arc<PackageCache>> = OnceLock::new();

/// Record in this daemon's cache that `app` was opened: the one place last use is recorded. A
/// daemon that keeps no cache records nothing.
pub(crate) fn opened(app: &str) {
    if let Some(cache) = CACHE.get() {
        cache.opened(app);
    }
}

/// Where the device keeps what callers put for its own resources: beside its steps' on a device
/// that runs them (Tilo OS), else beside the cache at `root`.
fn device_content(root: &Path) -> PathBuf {
    if Path::new(super::control::STEP_PROGRAM).is_file() {
        PathBuf::from(super::control::CONTENT_DIR)
    } else {
        root.join("content")
    }
}

impl Writer {
    /// Make `change` to the cache, to its end.
    fn apply(&self, change: package_actions::Change) -> Result<package_actions::Report, String> {
        let store = package_store::PackageStore::at(&self.root);
        let catalog =
            package_store::Catalog::at(&self.root, package_store::SystemStore::from_env());
        let files = |source: &str| self.file(source);
        package_actions::apply(
            &package_actions::Target {
                store: &store,
                catalog: &catalog,
                max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
            },
            change,
            &package_actions::Entry::Device(&files),
        )
    }

    /// Where the device keeps the bytes `source` names.
    fn file(&self, source: &str) -> Option<PathBuf> {
        let hash = tilo_actions::content_hash(source)?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.content.dir().join(hash)).filter(|path| path.is_file())
    }

    /// Make the change `request` asks for, to its end.
    fn run(&self, request: &InvokeRequest) -> InvocationUpdate {
        let invocation = request.invocation;
        let failed = |message: String| InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: "package_cache".into(),
                message,
                details: None,
                retryable: false,
            },
        };
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None | InvokeArgument::Text(_) => serde_json::Map::new(),
        };
        let action = request.target.address.action.as_str();
        if let Some(answer) = package_actions::reading(action, &arguments) {
            return match answer {
                Ok(value) => InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
                Err(error) => failed(error),
            };
        }
        let Some(published) = package_actions::published()
            .iter()
            .find(|published| published.id == action)
        else {
            return failed(format!("{action} is no action of the cache"));
        };
        let change = match published.change(arguments) {
            Ok(change) => change,
            Err(error) => return failed(error),
        };
        match self.apply(change) {
            Ok(report) => InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: serde_json::to_value(report).ok(),
                    resources: Vec::new(),
                },
            },
            Err(error) => failed(error),
        }
    }
}

impl ActionProvider for PackageCache {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::published(
            PROVIDER,
            &self.resources,
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if !self.names(&request) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None | InvokeArgument::Text(_) => serde_json::Map::new(),
        };
        if let Err(error) = tilo_actions::api::admit(
            &self.resources,
            &request.target.address,
            &arguments,
            request.confirmation.as_ref(),
        ) {
            return Pending::ready(Err(ActionCallError::from_api(&error)));
        }
        let id = request.invocation;
        let reserved = self.log.lock_recover().reserve(caller, &request);
        match reserved {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                let run = self.log.lock_recover().get(caller, &id).cloned();
                return Pending::ready(
                    run.map(|run| run.lock_recover().receipt())
                        .ok_or(ActionCallError::UnknownInvocation),
                );
            }
            Ok(Reservation::Reserved) => {}
        }
        let state = Arc::new(Mutex::new(InvocationState::new(
            id,
            InvocationUpdate::Accepted { invocation: id },
        )));
        let receipt = state.lock_recover().receipt();
        self.log.lock_recover().complete(&id, Arc::clone(&state));
        // A change can take minutes (a runtime built, a registry fetched): it runs on its own
        // thread, and its caller follows it.
        let writer = self.writer.clone();
        let started = std::thread::Builder::new()
            .name("package-cache".into())
            .spawn(move || {
                let update = writer.run(&request);
                writer.rows.moved();
                *state.lock_recover() = InvocationState::new(id, update);
            });
        if let Err(error) = started {
            self.log.lock_recover().abandon(&id);
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: format!("the device could not make the change: {error}"),
            }));
        }
        Pending::ready(Ok(receipt))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.log
            .lock_recover()
            .get(caller, id)
            .map(|state| state.lock_recover().clone())
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(match self.log.lock_recover().get(caller, id) {
            None => Err(ActionCallError::UnknownInvocation),
            Some(_) => Err(ActionCallError::NotCancellable {
                reason: "a change to the device's packages runs to its end once it began".into(),
            }),
        })
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::clone(&self.writer.content) as Arc<dyn ContentPuts>)
    }

    fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        Some(Arc::clone(&self.writer.rows) as Arc<dyn ResourceReads>)
    }
}

/// The trust a workspace keeps for packages, as the installer takes it: the
/// attestors it accepts and the publisher keys it refuses.
type TrustLists = (
    Vec<tilo_app_package::trust::TrustedAttestor>,
    Vec<tilo_app_package::trust::RevokedKey>,
);

/// The workspace's trust list, converted to the installer's own rows.
fn trust_lists(
    (attestors, revoked): (
        Vec<tilo_peer_state::TrustAttestor>,
        Vec<tilo_peer_state::RevokedPublisher>,
    ),
) -> TrustLists {
    (
        attestors
            .into_iter()
            .filter_map(|attestor| {
                Some(tilo_app_package::trust::TrustedAttestor {
                    scope: tilo_app_package::trust::AttestorScope::parse(&attestor.scope).ok()?,
                    id: attestor.id,
                    pubkey_hex: attestor.pubkey_hex,
                })
            })
            .collect(),
        revoked
            .into_iter()
            .map(|key| tilo_app_package::trust::RevokedKey {
                pubkey_hex: key.pubkey_hex,
                reason: key.reason,
            })
            .collect(),
    )
}

/// Keep the apps the workspaces this device keeps (`workspaces`) run in the cache, from now on:
/// each version a workspace's tree names (`workspace/apps/<app>`) is installed once its package
/// file is here,
/// fetched from the workspace when this device has none, and bound to the package root and
/// publisher the tree names. A Flatpak a tree names is installed into the device's system Flatpak
/// installation, and one this device installed and no workspace names any more is removed. A
/// package the follower installed that no workspace names any more is removed too, and what it
/// ran on collected; one installed some other way (the image, an offline store, a developer's
/// install) stays. What the tree names again after a change is looked at again.
pub(crate) fn follow(
    cache: &Arc<PackageCache>,
    workspaces: Arc<super::workspaces::ServiceWorkspaces>,
) {
    let cache = Arc::clone(cache);
    tilo_rt::app().spawn(async move {
        let mut changes = workspaces.copies().governance_changes();
        loop {
            let mut flatpaks = std::collections::BTreeSet::new();
            let mut apps = std::collections::BTreeSet::new();
            for summary in workspaces.copies().list().await {
                let named = cache.keep_named(&workspaces, &summary.workspace).await;
                flatpaks.extend(named.flatpaks);
                apps.extend(named.apps);
            }
            {
                let cache = Arc::clone(&cache);
                tilo_rt::app().spawn_blocking(move || {
                    #[cfg(target_os = "linux")]
                    cache.drop_flatpaks_not_named(&flatpaks);
                    #[cfg(not(target_os = "linux"))]
                    drop(flatpaks);
                    cache.drop_followed_not_named(&apps);
                });
            }
            if let Err(tokio::sync::broadcast::error::RecvError::Closed) = changes.recv().await {
                return;
            }
        }
    });
}

/// Where the cache keeps the Flatpak references its daemon installed.
const FLATPAKS_FILE: &str = "flatpaks.json";

/// Where the cache keeps the apps the follower installed: the only ones it removes.
const FOLLOWED_FILE: &str = "followed.json";

/// What one workspace's tree names: its Flatpak references and its apps.
#[derive(Default)]
struct Named {
    flatpaks: Vec<String>,
    apps: Vec<String>,
}

/// The apps the follower installed, by id.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Followed {
    #[serde(default)]
    apps: std::collections::BTreeSet<String>,
}

impl Followed {
    fn read(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(path, &bytes)
            .map_err(|error| format!("{}: {error}", path.display()))
    }
}

impl PackageCache {
    /// Install each version `workspace` names that the cache lacks, answering what it named.
    async fn keep_named(
        self: &Arc<Self>,
        workspaces: &Arc<super::workspaces::ServiceWorkspaces>,
        workspace: &str,
    ) -> Named {
        let Ok(versions) = workspaces.copies().app_versions(workspace).await else {
            // A tree this device cannot read now names nothing it may remove: keep every app.
            return Named {
                apps: self.followed().apps.into_iter().collect(),
                ..Named::default()
            };
        };
        let mut named = Named::default();
        // The registries this workspace fetches from, read once for its apps.
        let registries = Arc::new(
            app_cli::config::registries_from_rows(&serde_json::json!({
                "registries": workspaces.copies().registries(workspace).await.unwrap_or_default(),
            }))
            .unwrap_or_default(),
        );
        // Whose packages this workspace trusts, read once for its apps.
        let lists = trust_lists(
            workspaces
                .copies()
                .package_trust(workspace)
                .await
                .unwrap_or_default(),
        );
        for installed in versions {
            let release = installed.release;
            named.apps.push(release.app.clone());
            if let Some(reference) = release.payload.flatpak_reference() {
                // Flatpak is Linux's: elsewhere the record syncs, and another device runs it.
                #[cfg(target_os = "linux")]
                {
                    // Flatpak takes minutes to fetch an app; it is no work for the reactor.
                    let cache = Arc::clone(self);
                    let wanted = reference.to_owned();
                    tilo_rt::app().spawn_blocking(move || cache.keep_flatpak(&wanted));
                }
                named.flatpaks.push(reference.to_owned());
                continue;
            }
            if self.runs(&release.package) {
                continue;
            }
            // A version from a registry names no file: the registries offer it.
            let Some(file) = release.file.clone() else {
                let cache = Arc::clone(self);
                let lists = lists.clone();
                let registries = Arc::clone(&registries);
                tilo_rt::app().spawn_blocking(move || {
                    cache.fetch_offered(&release, &lists, &registries);
                });
                continue;
            };
            if let Some(bytes) = workspaces
                .copies()
                .installed_package(workspace, &release.app)
                .await
            {
                self.install_named(&release, &bytes, &lists, &registries);
                continue;
            }
            let Some(content) = workspaces.record_content_service() else {
                continue;
            };
            let route = tilo_workspace_service::content::CrdtRoute::app(
                release.app.clone(),
                workspace.to_owned(),
            );
            if let Some(bytes) = content.kept(&route, &file) {
                self.install_named(&release, &bytes, &lists, &registries);
                continue;
            }
            // Asked of the workspace; installed when its bytes arrive.
            self.writer.rows.fetching(&release, 0, None);
            let mut news = content.subscribe(&route);
            content.want(&route, &file);
            let cache = Arc::clone(self);
            let lists = lists.clone();
            let registries = Arc::clone(&registries);
            tilo_rt::app().spawn(async move {
                while let Some(arrived) = news.recv().await {
                    if arrived.hash == file
                        && arrived.state == tilo_workspace_service::content::ContentState::Resolved
                    {
                        let cache = Arc::clone(&cache);
                        tilo_rt::app().spawn_blocking(move || {
                            cache.install_named(&release, &arrived.bytes, &lists, &registries);
                        });
                        return;
                    }
                }
            });
        }
        named
    }

    /// The apps the follower installed.
    fn followed(&self) -> Followed {
        let path = self.writer.root.join(FOLLOWED_FILE);
        Followed::read(&path).unwrap_or_else(|error| {
            tracing::warn!(%error, "the apps this device's follower installed are not read");
            Followed::default()
        })
    }

    /// Mark `app` as one the follower installed.
    fn mark_followed(&self, app: &str) {
        let _serial = self.writer.followed.lock_recover();
        let path = self.writer.root.join(FOLLOWED_FILE);
        let mut followed = self.followed();
        if followed.apps.insert(app.to_owned())
            && let Err(error) = followed.save(&path)
        {
            tracing::warn!(%error, "the apps this device's follower installed are not kept");
        }
    }

    /// Remove each app the follower installed that no workspace names any more, then collect
    /// what nothing runs on now. An app installed some other way is never removed here.
    fn drop_followed_not_named(&self, named: &std::collections::BTreeSet<String>) {
        let _serial = self.writer.followed.lock_recover();
        let path = self.writer.root.join(FOLLOWED_FILE);
        let mut followed = self.followed();
        let going: Vec<String> = followed
            .apps
            .iter()
            .filter(|app| !named.contains(*app))
            .cloned()
            .collect();
        if going.is_empty() {
            return;
        }
        for app in going {
            let removed = self.writer.apply(package_actions::Change::Uninstall {
                app: app.clone(),
                // What the person made with it stays: the workspace may name the app again.
                saved_data: package_actions::SavedData::Keep,
                dry_run: false,
            });
            match removed {
                Ok(_) => {
                    followed.apps.remove(&app);
                    tracing::info!(%app, "removed the app no workspace names");
                }
                Err(error) => {
                    tracing::warn!(%app, %error, "the app no workspace names did not remove");
                }
            }
        }
        if let Err(error) = followed.save(&path) {
            tracing::warn!(%error, "the apps this device's follower installed are not kept");
        }
        if let Err(error) = self.writer.apply(package_actions::Change::Collect {
            dry_run: false,
            grace_secs: runtime_bundle::gc::DEFAULT_GRACE_SECS,
            max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
        }) {
            tracing::warn!(%error, "what no app runs on was not collected");
        }
    }

    /// Install the Flatpak `reference` into the device's system installation, unless this device
    /// already installed it or it is already there.
    #[cfg(target_os = "linux")]
    fn keep_flatpak(&self, reference: &str) {
        let flatpak = package_actions::flatpak::Flatpak::from_env();
        if !flatpak.available() {
            // The record syncs all the same: another of the person's devices runs it.
            return;
        }
        let _serial = self.writer.flatpaks.lock_recover();
        let path = self.writer.root.join(FLATPAKS_FILE);
        let mut kept = match package_actions::flatpak::Kept::read(&path) {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(%error, "the Flatpaks this device installed are not read");
                return;
            }
        };
        if kept.references.iter().any(|kept| kept == reference) {
            return;
        }
        // Flathub is where the apps a workspace names come from; adding it is best effort, as a
        // device that already reads it keeps its own configuration.
        if let Err(error) = flatpak.flathub_add() {
            tracing::warn!(%error, "Flathub is not a Flatpak remote on this device");
        }
        match flatpak.install(reference) {
            Ok(()) => {
                kept.references.push(reference.to_owned());
                kept.references.sort();
                kept.references.dedup();
                if let Err(error) = kept.save(&path) {
                    tracing::warn!(%error, "the Flatpaks this device installed are not kept");
                }
                tracing::info!(%reference, "installed the Flatpak a workspace runs");
            }
            Err(error) => {
                tracing::warn!(%reference, %error, "the Flatpak a workspace runs did not install");
            }
        }
    }

    /// Remove the Flatpaks this device installed that no workspace names any more.
    #[cfg(target_os = "linux")]
    fn drop_flatpaks_not_named(&self, named: &std::collections::BTreeSet<String>) {
        let flatpak = package_actions::flatpak::Flatpak::from_env();
        if !flatpak.available() {
            return;
        }
        let _serial = self.writer.flatpaks.lock_recover();
        let path = self.writer.root.join(FLATPAKS_FILE);
        let Ok(mut kept) = package_actions::flatpak::Kept::read(&path) else {
            return;
        };
        let still_named: std::collections::BTreeSet<&str> = named
            .iter()
            .filter_map(|reference| reference.split('/').nth(1))
            .collect();
        let going: Vec<String> = kept
            .references
            .iter()
            .filter(|reference| !named.contains(*reference))
            .filter(|reference| {
                // A different branch of an app another workspace still names stays.
                !reference
                    .split('/')
                    .nth(1)
                    .is_some_and(|app| still_named.contains(app))
            })
            .cloned()
            .collect();
        for reference in going {
            let app = reference.split('/').nth(1).unwrap_or_default();
            match flatpak.remove(app) {
                Ok(()) => {
                    kept.references.retain(|kept| kept != &reference);
                    tracing::info!(%reference, "removed the Flatpak no workspace names");
                }
                Err(error) => {
                    tracing::warn!(%reference, %error, "the Flatpak no workspace names did not remove");
                }
            }
        }
        if let Err(error) = kept.save(&path) {
            tracing::warn!(%error, "the Flatpaks this device installed are not kept");
        }
    }

    /// Fetch `release` from the workspace's `registries` that offer it, checked against the root,
    /// publisher and surface it names, and install it.
    fn fetch_offered(
        &self,
        release: &tilo_peer_state::workspace_membership::AppRelease,
        lists: &TrustLists,
        registries: &app_cli::config::RegistriesFile,
    ) {
        self.writer.rows.fetching(release, 0, None);
        let fetched = app_cli::config::with_registries(registries.clone(), || {
            for registry in registries.enabled() {
                let Ok(offers) = app_cli::version_offers(&release.app, &registry.name) else {
                    continue;
                };
                let Some(offer) = offers.into_iter().find(|offer| {
                    offer
                        .tapp_root_hash_hex
                        .eq_ignore_ascii_case(&release.package)
                        && offer
                            .publisher_pubkey_hex
                            .eq_ignore_ascii_case(&release.publisher)
                }) else {
                    continue;
                };
                if !offer
                    .tapp_surface_hex
                    .eq_ignore_ascii_case(&release.surface)
                {
                    return Err(format!(
                        "{} offers version {} with another surface than the workspace records",
                        registry.name, release.version
                    ));
                }
                let staged = tempfile::NamedTempFile::new_in(self.writer.content.dir())
                    .map_err(|error| error.to_string())?;
                let rows = &self.writer.rows;
                app_cli::install::fetch_verified(&offer, staged.path(), &mut |progress| {
                    rows.fetching(release, progress.received, progress.total);
                })
                .map_err(|error| error.to_string())?;
                return std::fs::read(staged.path()).map_err(|error| error.to_string());
            }
            Err(format!(
                "no repository offers version {} of {}",
                release.version, release.app
            ))
        });
        match fetched {
            Ok(bytes) => self.install_bytes(release, &bytes, lists, registries),
            Err(error) => {
                self.writer.rows.arrived(&release.app);
                tracing::warn!(app = %release.app, %error, "the version a workspace runs is not here");
            }
        }
    }

    /// Whether the cache has the package `package` (its root, hex).
    fn runs(&self, package: &str) -> bool {
        let Ok(id) = package_actions::image_id(package) else {
            return false;
        };
        package_store::Catalog::at(&self.writer.root, package_store::SystemStore::from_env())
            .find_package(id)
            .is_ok_and(|found| found.is_some())
    }

    /// Install `bytes`, the package file the workspace keeps of `release`, bound to the package
    /// root and publisher it names.
    fn install_named(
        &self,
        release: &tilo_peer_state::workspace_membership::AppRelease,
        bytes: &[u8],
        lists: &TrustLists,
        registries: &app_cli::config::RegistriesFile,
    ) {
        if release.check_file(bytes).is_err() {
            self.writer.rows.arrived(&release.app);
            tracing::warn!(app = %release.app, "a workspace's package file is not the version it names");
            return;
        }
        self.install_bytes(release, bytes, lists, registries);
    }

    /// Install `bytes`, a package file of `release`, bound to the package root and publisher it
    /// names: first the runtime it runs on and the engines it requires, from the workspace's
    /// `registries`, so the app never lands without them.
    fn install_bytes(
        &self,
        release: &tilo_peer_state::workspace_membership::AppRelease,
        bytes: &[u8],
        lists: &TrustLists,
        registries: &app_cli::config::RegistriesFile,
    ) {
        if let Err(error) = self.keep_runtimes(release, bytes, lists, registries) {
            self.writer.rows.arrived(&release.app);
            tracing::warn!(app = %release.app, %error, "what the version a workspace runs runs on is not here");
            return;
        }
        if self.install_package(release, bytes, lists) {
            self.mark_followed(&release.app);
        }
    }

    /// Install the runtime `bytes`' app runs on and the engines its manifest requires, each that
    /// this device lacks, from the first of `registries` that offers it.
    fn keep_runtimes(
        &self,
        release: &tilo_peer_state::workspace_membership::AppRelease,
        bytes: &[u8],
        lists: &TrustLists,
        registries: &app_cli::config::RegistriesFile,
    ) -> Result<(), String> {
        let tapp = tilo_app_package::tapp::read(
            std::io::Cursor::new(bytes),
            &mut tilo_app_package::DiscardBlobs,
        )
        .map_err(|error| error.to_string())?;
        let manifest = &tapp.meta.manifest;
        let mut wanted: Vec<String> = manifest
            .target
            .as_ref()
            .map(|target| target.runtime.clone())
            .filter(|runtime| {
                !runtime.is_empty() && !registry_types::is_runtime_package_id(&release.app)
            })
            .into_iter()
            .collect();
        wanted.extend(
            tilo_app_package::desktop::required_runtime_dependencies(manifest)
                .iter()
                .cloned(),
        );
        for runtime in wanted {
            if package_store::runtime_installed(&runtime) {
                continue;
            }
            let offer = app_cli::config::with_registries(registries.clone(), || {
                app_cli::runtime_offer_from_enabled(&runtime)
            })?
            .ok_or_else(|| format!("no repository offers the runtime {runtime}"))?;
            let needed = tilo_peer_state::workspace_membership::AppRelease {
                app: offer.package_id.clone(),
                version: offer.version.clone(),
                surface: offer.tapp_surface_hex.clone(),
                package: offer.tapp_root_hash_hex.clone(),
                publisher: offer.publisher_pubkey_hex.clone(),
                file: None,
                dev: false,
                pinned: false,
                payload: tilo_peer_state::workspace_membership::AppPayload::Tapp,
            };
            let staged = tempfile::NamedTempFile::new_in(self.writer.content.dir())
                .map_err(|error| error.to_string())?;
            let rows = &self.writer.rows;
            app_cli::install::fetch_verified(&offer, staged.path(), &mut |progress| {
                rows.fetching(release, progress.received, progress.total);
            })
            .map_err(|error| error.to_string())?;
            let runtime_bytes = std::fs::read(staged.path()).map_err(|error| error.to_string())?;
            if !self.install_package(&needed, &runtime_bytes, lists) {
                return Err(format!("the runtime {runtime} did not install"));
            }
        }
        Ok(())
    }

    /// Install `bytes`, a package file of `release`, bound to the package root and publisher it
    /// names; whether it installed.
    fn install_package(
        &self,
        release: &tilo_peer_state::workspace_membership::AppRelease,
        bytes: &[u8],
        lists: &TrustLists,
    ) -> bool {
        // The surface the workspace decides the app's changes against is the one it runs.
        let surface = app_registry::publisher::entry::surface_hex(bytes);
        if !surface
            .as_ref()
            .is_ok_and(|surface| surface.eq_ignore_ascii_case(&release.surface))
        {
            self.writer.rows.arrived(&release.app);
            tracing::warn!(app = %release.app, ?surface, "a package's surface is not the one the workspace records");
            return false;
        }
        let hash = blake3::hash(bytes).to_hex();
        let hash = hash.as_str();
        let kept = std::fs::create_dir_all(self.writer.content.dir()).and_then(|()| {
            tilo_files::native::atomic::write(&self.writer.content.dir().join(hash), bytes)
        });
        if let Err(error) = kept {
            self.writer.rows.arrived(&release.app);
            tracing::warn!(app = %release.app, %error, "the device could not keep a workspace's package file");
            return false;
        }
        let change = package_actions::Change::Install {
            file: package_actions::Uploaded {
                source: format!("tilo.content:{hash}"),
            },
            trust: package_actions::Trust {
                require_package_root: Some(release.package.clone()),
                require_publisher: Some(release.publisher.clone()),
                require_attestor: None,
                allow_self_signed: release.dev,
                attestors: lists.0.clone(),
                revoked: lists.1.clone(),
            },
            pinned: false,
            replacement: package_actions::Replacement::Replace,
            ignore_missing_runtime: false,
            dry_run: false,
            payload_from: None,
        };
        let applied = self.writer.apply(change);
        self.writer.rows.arrived(&release.app);
        match applied {
            Ok(_) => {
                tracing::info!(app = %release.app, version = %release.version, "installed the version a workspace runs");
                true
            }
            Err(error) => {
                tracing::warn!(app = %release.app, %error, "the version a workspace runs did not install");
                false
            }
        }
    }
}

#[cfg(test)]
#[path = "packages_follower_tests.rs"]
mod follower_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache whose store has the one package `installed`, named by its root.
    fn cache_with_installed(installed: [u8; 32], root: &Path) -> PackageCache {
        std::fs::write(
            root.join("registry.toml"),
            format!(
                "[[entry]]\nid = \"so.tilo.files\"\nversion = \"1.0.0\"\n\
                 package_id_hex = \"{}\"\npublisher_key_hex = \"{}\"\n\
                 payload_root = \"{}\"\nmanifest_path = \"{}\"\n",
                hex::encode(installed),
                hex::encode([2; 32]),
                root.join("payloads").join(hex::encode(installed)).display(),
                root.join("pkgs")
                    .join(hex::encode(installed))
                    .join("manifest.toml")
                    .display(),
            ),
        )
        .unwrap();
        PackageCache::new(&[7; 32], root, Arc::new(DeviceContent::at(root)))
    }

    #[test]
    fn a_version_the_cache_has_is_run_without_fetching_it() {
        let store = tempfile::tempdir().unwrap();
        let _env = tilo_test_env::ScopedEnv::new();
        // The store is this test's whole answer: the image's own is not read over it.
        _env.remove("TILO_APPS_SYSTEM_DIR")
            .set("TILO_APPS_DATA_DIR", store.path());
        let installed = [9; 32];
        let cache = cache_with_installed(installed, store.path());
        assert!(
            cache.runs(&hex::encode(installed)),
            "a package the cache has is installed, so the follower never fetches it"
        );
        assert!(
            !cache.runs(&hex::encode([4; 32])),
            "a package the cache lacks is the follower's to fetch"
        );
    }
}
