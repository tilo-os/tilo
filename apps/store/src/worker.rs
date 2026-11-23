//! The Store's serialized worker: catalogue and package-source reads, and the workspace's apps,
//! this device's packages and the changes to them, over the session's action API (`packages`).
//! This device's Flatpak metadata and Flathub's feed are read on threads of their own; the
//! workspace's Tilo repositories are read by this device's package cache. The UI drains
//! snapshots.

use crate::resource::CATALOG;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use lru::LruCache;
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use ui::WakeHandle;

use crate::config::{RegistriesFile, RegistryConfig};
use crate::flathub_client::{Remote, installed_flatpaks};
use crate::model::{ActionKind, StoreSource};
use crate::search::StoreDirectory;
use crate::{FlatpakRef, StoreItem};

mod packages;
#[cfg(test)]
mod packages_tests;
mod refresh;

const REFRESH_INTERVAL: Duration = Duration::from_secs(900);
const CACHE_CAP: NonZeroUsize = match NonZeroUsize::new(128) {
    Some(cap) => cap,
    None => NonZeroUsize::MIN,
};

#[derive(Clone, Debug)]
pub enum Request {
    Refresh,
    BrowseRemote(String),
    Detail(String),
    RuntimeDetails {
        runtime: String,
        registry: Option<String>,
    },
    OpenReference {
        path: Option<PathBuf>,
        contents: Option<String>,
    },
    ImportPackage(crate::package_import::Input),
    InstallImported {
        app: String,
        root: String,
    },
    RefreshRemotes,
    RemoteAdd {
        name: String,
        url: String,
    },
    RemoteRemove(String),
    RemoteSetEnabled {
        name: String,
        enabled: bool,
    },
    /// Boxed: [`StoreItem`] is far larger than every other variant's
    /// payload, and this is a queued message — boxing keeps every
    /// `Request` (and the channel that carries them) sized to the common
    /// case instead of the rare, heavy one.
    Act(Box<StoreItem>, ActionKind),
    UpdateAll(Vec<StoreItem>),
    ExternalModeration(ui::registry::ExternalModerationOperation),
    RefreshModeration {
        registry_name: String,
        case_id: String,
    },
}

pub type Outcome = Result<Finished, String>;

pub type Done = mpsc::Sender<Outcome>;

#[derive(Clone, Debug, Default)]
pub struct Finished {
    pub status: String,
    pub case: Option<ui::registry::ModerationCase>,
    pub launch: Option<crate::actions::ApplicationLaunch>,
}

impl Finished {
    fn status(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            case: None,
            launch: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum DetailUpdate {
    /// Enrich the app the person already selected; this is not navigation.
    Requested {
        id: String,
        item: Option<StoreItem>,
    },
    OpenedReference(StoreItem),
    ImportedPackage {
        item: StoreItem,
        root: String,
        version: String,
    },
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub items: Vec<StoreItem>,
    pub status_line: String,
    pub query: Option<String>,
    pub detail: Option<DetailUpdate>,
    pub remotes: Vec<Remote>,
    pub flatpak_available: bool,
    pub flatpak_error: Option<String>,
    pub operation_error: Option<String>,
    /// Enabled native repositories that have not answered the current
    /// refresh, so installed apps they list are described only locally.
    pub unanswered_repositories: Vec<String>,
    /// Enabled native repositories whose catalogue is authentic but expired,
    /// so the Store says so instead of calling them unreachable.
    pub expired_repositories: Vec<String>,
    pub blocks: ui::registry::BlockList,
    pub updating: Option<String>,
    /// The apps this device is fetching, by app id, with the bytes it has.
    pub installs: std::collections::BTreeMap<String, ui::arriving::Progress>,
}

struct QueuedRequest {
    request: Request,
    done: Option<Done>,
    locale: LocaleContext,
}

struct LocaleContext {
    selected: String,
    pseudo: ui::i18n::PseudoLocale,
}

impl LocaleContext {
    fn capture() -> Self {
        Self {
            selected: ui::i18n::I18nManager::current_locale(),
            pseudo: ui::i18n::I18nManager::pseudo_locale(),
        }
    }

    fn apply(&self) {
        crate::resource::CATALOG.set_locale(&self.selected);
        ui::i18n::I18nManager::set_pseudo_locale(self.pseudo);
    }
}

pub struct StoreWorker {
    requests: mpsc::Sender<QueuedRequest>,
    snapshots: mpsc::Receiver<Snapshot>,
    pending: Arc<AtomicUsize>,
    #[cfg(test)]
    wake: WakeHandle,
}

impl StoreWorker {
    /// Start the worker; `wake` is woken whenever a snapshot is ready.
    ///
    /// # Panics
    /// Panics if the OS refuses to spawn a thread (extreme resource
    /// exhaustion): a live Store without its worker has nothing to show.
    pub fn spawn(
        directory: StoreDirectory,
        wake: WakeHandle,
        actions: ui::RemoteActions,
        workspace: Option<String>,
    ) -> Self {
        Self::spawn_with(directory, wake, actions, workspace, Vec::new())
    }

    #[allow(
        clippy::expect_used,
        reason = "thread spawn failure means the OS is out of resources; the surface attaching the worker has no fallible path to report it on"
    )]
    fn spawn_with(
        directory: StoreDirectory,
        wake: WakeHandle,
        actions: ui::RemoteActions,
        workspace: Option<String>,
        registries: Vec<RegistryConfig>,
    ) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(1));
        let locale = LocaleContext::capture();
        #[cfg(test)]
        let retained_wake = wake.clone();
        std::thread::Builder::new()
            .name("store-worker".into())
            .spawn({
                let pending = Arc::clone(&pending);
                let registries = registries.clone();
                move || {
                    // Fluent managers are thread-local. Worker status must
                    // use the same selected app catalogue as its live view.
                    locale.apply();
                    let snapshots = Snapshots {
                        sender: snapshot_tx,
                        wake,
                        pending,
                    };
                    run(
                        &request_rx,
                        &snapshots,
                        directory,
                        actions,
                        workspace,
                        registries,
                    );
                }
            })
            .expect("spawn store worker");
        Self {
            requests: request_tx,
            snapshots: snapshot_rx,
            pending,
            #[cfg(test)]
            wake: retained_wake,
        }
    }

    /// A worker like this one that reads through `actions` and the repositories `registries`.
    #[cfg(test)]
    pub(crate) fn with_actions(
        &self,
        directory: StoreDirectory,
        actions: ui::RemoteActions,
        registries: Vec<RegistryConfig>,
    ) -> Self {
        Self::spawn_with(directory, self.wake.clone(), actions, None, registries)
    }

    pub fn request(&self, request: Request) -> bool {
        self.queue(request, None)
    }

    pub fn request_job(&self, request: Request, done: Done) -> bool {
        self.queue(request, Some(done))
    }

    fn queue(&self, request: Request, done: Option<Done>) -> bool {
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self
            .requests
            .send(QueuedRequest {
                request,
                done,
                locale: LocaleContext::capture(),
            })
            .is_ok()
        {
            true
        } else {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            false
        }
    }

    pub fn try_snapshot(&self) -> Option<Snapshot> {
        let snapshot = self.snapshots.try_recv().ok()?;
        let _ = self
            .pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
        Some(snapshot)
    }

    pub fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed) > 0
    }
}

#[derive(Clone)]
struct Snapshots {
    sender: mpsc::Sender<Snapshot>,
    wake: WakeHandle,
    pending: Arc<AtomicUsize>,
}

impl Snapshots {
    fn send(&self, snapshot: Snapshot) -> bool {
        let sent = self.sender.send(snapshot).is_ok();
        self.wake.wake();
        sent
    }

    fn progress(&self, snapshot: Snapshot) -> bool {
        self.pending.fetch_add(1, Ordering::Relaxed);
        self.send(snapshot)
    }
}

struct State {
    refresh_requested: bool,
    /// Passive source hydration must not erase a completed operation's notice.
    operation_notice: Option<String>,
    /// Whether a refresh reads this device's Flatpak metadata and Flathub's feed on threads of
    /// their own. A state built to check one response at a time reads neither.
    background_reads: bool,
    /// The workspace's apps and this device's packages, through the session's action API.
    packages: packages::AppPackages,
    opened_ref: Option<FlatpakRef>,
    imported_package: Option<crate::package_import::ImportedPackage>,
    base_items: Vec<StoreItem>,
    flatpak_installed: Vec<(String, String)>,
    installed_origins: std::collections::HashMap<String, (String, bool)>,
    remotes: Vec<Remote>,
    flatpak_available: bool,
    flatpak_error: Option<String>,
    catalogue_error: Option<String>,
    unanswered_repositories: Vec<String>,
    expired_repositories: Vec<String>,
    details: LruCache<String, StoreItem>,
    directory: StoreDirectory,
    blocks: ui::registry::BlockList,
    /// The person's repositories as the last refresh read them. Kept here so
    /// publishing the directory never asks the workspace again.
    registries: Vec<RegistryConfig>,
    /// Flathub's catalogue as the last refresh read it, so the next refresh
    /// lists Flathub's apps from its first snapshot while it reads again.
    flathub: Option<crate::catalogue::flathub::FlathubFeed>,
    force_refresh: bool,
    /// The change the worker started and has not finished, if any.
    running: Option<Running>,
    /// A press on Open waiting for this device to finish installing the app.
    opens: Vec<OpenWait>,
}

/// One change on its way to the workspace, and the caller waiting for it.
struct Running {
    started: packages::Started,
    done: Option<Done>,
}

/// A press on Open that waits for the app's package to reach this device.
struct OpenWait {
    app: String,
    name: String,
    done: Option<Done>,
    /// When the wait began, so one that never ends is said rather than kept.
    since: std::time::Instant,
}

fn initial_state(
    directory: StoreDirectory,
    actions: ui::RemoteActions,
    workspace: Option<String>,
    registries: Vec<RegistryConfig>,
) -> State {
    State {
        refresh_requested: false,
        operation_notice: None,
        background_reads: true,
        packages: packages::AppPackages::new(actions, workspace),
        opened_ref: None,
        imported_package: None,
        // The catalogue the last window kept: a refresh keeps describing
        // every app it lists until that app's own source answers, so the
        // window never empties while the sources are asked again.
        base_items: crate::catalogue::cache::load().map_or_else(Vec::new, |cached| cached.items),
        flatpak_installed: Vec::new(),
        installed_origins: std::collections::HashMap::new(),
        remotes: Vec::new(),
        flatpak_available: false,
        flatpak_error: None,
        catalogue_error: None,
        unanswered_repositories: Vec::new(),
        expired_repositories: Vec::new(),
        details: LruCache::new(CACHE_CAP),
        blocks: ui::registry::BlockList::load(),
        directory,
        registries,
        flathub: crate::catalogue::flathub::load(),
        force_refresh: false,
        running: None,
        opens: Vec::new(),
    }
}

fn defer_import(
    queued: QueuedRequest,
    deferred: &mut std::collections::VecDeque<QueuedRequest>,
    state: &State,
    snapshots: &Snapshots,
) -> bool {
    if deferred.len() < 16 {
        deferred.push_back(queued);
        return true;
    }
    let error = CATALOG.t("store-package-review-busy");
    let alive = send_tracked(
        snapshots,
        state,
        state.base_items.clone(),
        &error,
        None,
        None,
        Some(error.clone()),
    );
    if let Some(done) = queued.done {
        let _ = done.send(Err(error));
    }
    alive
}

fn run(
    requests: &mpsc::Receiver<QueuedRequest>,
    snapshots: &Snapshots,
    directory: StoreDirectory,
    actions: ui::RemoteActions,
    workspace: Option<String>,
    registries: Vec<RegistryConfig>,
) {
    let mut state = initial_state(directory, actions, workspace, registries);
    let mut refresh = Some(refresh::Refresh::start(&mut state, None));
    let mut deferred_imports = std::collections::VecDeque::new();
    loop {
        if !poll_refresh(&mut refresh, &mut state, snapshots) {
            break;
        }
        if !advance_packages(&mut state, snapshots) {
            break;
        }
        let wait = if refresh.is_some() {
            Duration::from_millis(25)
        } else if state.running.is_some() || !state.opens.is_empty() || state.packages.reading() {
            Duration::from_millis(50)
        } else if state.packages.waiting() {
            Duration::from_secs(1)
        } else {
            REFRESH_INTERVAL
        };
        let next = if refresh.is_none() && !deferred_imports.is_empty() {
            deferred_imports
                .pop_front()
                .ok_or(mpsc::RecvTimeoutError::Disconnected)
        } else {
            requests.recv_timeout(wait)
        };
        match next {
            Ok(QueuedRequest {
                request,
                mut done,
                locale,
            }) => {
                locale.apply();
                // A freshly opened Store has no verified offers until its initial
                // source reads finish. Do not mislabel a valid handed file as
                // unlisted merely because those same normal reads are pending.
                if matches!(request, Request::ImportPackage(_)) && refresh.is_some() {
                    if !defer_import(
                        QueuedRequest {
                            request,
                            done,
                            locale,
                        },
                        &mut deferred_imports,
                        &state,
                        snapshots,
                    ) {
                        break;
                    }
                    continue;
                }
                if !matches!(request, Request::Detail(_)) {
                    state.operation_notice = None;
                }
                if matches!(request, Request::Refresh) && refresh.is_none() {
                    // Only a refresh someone asked for (an effect's job)
                    // reads Flathub's whole catalogue again.
                    state.force_refresh = done.is_some();
                    state.packages.retry();
                    refresh = Some(refresh::Refresh::start(&mut state, done));
                    state.force_refresh = false;
                    continue;
                }
                let package_operation = matches!(
                    request,
                    Request::Act(..)
                        | Request::UpdateAll(_)
                        | Request::InstallImported { .. }
                        | Request::ImportPackage(_)
                );
                let (alive, outcome) = handle(request, &mut state, snapshots, &mut done);
                if package_operation
                    && let Some(outcome) = &outcome
                    && !outcome
                        .as_ref()
                        .is_ok_and(|finished| finished.launch.is_some())
                {
                    state.operation_notice = Some(match outcome {
                        Ok(finished) => finished.status.clone(),
                        Err(error) => error.clone(),
                    });
                }
                if let Some(outcome) = outcome
                    && let Some(done) = done
                {
                    let _ = done.send(outcome);
                }
                if !alive {
                    break;
                }
                if state.refresh_requested {
                    state.refresh_requested = false;
                    if let Some(previous) = refresh.as_mut() {
                        previous.retire();
                    } else {
                        snapshots.pending.fetch_add(1, Ordering::Relaxed);
                        refresh = Some(refresh::Refresh::start(&mut state, None));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) if refresh.is_none() => {
                snapshots.pending.fetch_add(1, Ordering::Relaxed);
                state.packages.retry();
                refresh = Some(refresh::Refresh::start(&mut state, None));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn poll_refresh(
    active: &mut Option<refresh::Refresh>,
    state: &mut State,
    snapshots: &Snapshots,
) -> bool {
    let Some(refresh) = active.as_mut() else {
        return true;
    };
    let (changed, finished) = refresh.poll(state);
    if finished && refresh.is_retired() {
        if let Some(previous) = active.take() {
            *active = Some(previous.restart(state));
        }
        return true;
    }
    if finished {
        let done = refresh.done.take();
        *active = None;
        keep_catalogue(state);
        let label = state
            .operation_notice
            .clone()
            .unwrap_or_else(|| CATALOG.t("store-catalogue-refreshed"));
        let alive = send(
            snapshots,
            state,
            state.base_items.clone(),
            &label,
            None,
            None,
        );
        if let Some(done) = done {
            let outcome = Ok(Finished::status(label));
            let _ = done.send(outcome);
        }
        return alive;
    }
    !changed
        || snapshots.progress(Snapshot {
            items: state.base_items.clone(),
            status_line: state
                .operation_notice
                .clone()
                .unwrap_or_else(|| CATALOG.t("store-catalogue-loading")),
            query: None,
            detail: None,
            remotes: state.remotes.clone(),
            flatpak_available: state.flatpak_available,
            flatpak_error: state.flatpak_error.clone(),
            operation_error: state.catalogue_error.clone(),
            unanswered_repositories: state.unanswered_repositories.clone(),
            expired_repositories: state.expired_repositories.clone(),
            blocks: state.blocks.clone(),
            updating: None,
            installs: installs_of(state),
        })
}

/// Take in what the session answered about the workspace's apps and this device's packages, and
/// finish what waited for them: a change that ended, a press on Open whose package arrived.
fn advance_packages(state: &mut State, snapshots: &Snapshots) -> bool {
    if state.running.is_some() {
        state.packages.poll();
    }
    let advanced = state.packages.advance();
    let mut changed = advanced.changed;
    let mut error = None;
    if let Some(code) = advanced.error {
        let reason = match code {
            "the_packages" => CATALOG.t("store-packages-unavailable"),
            _ => CATALOG.t("store-package-rows-unavailable"),
        };
        state.operation_notice = Some(reason.clone());
        error = Some(reason);
        changed = true;
    }
    if advanced.changed {
        apply_installed(state);
    }
    let ended = state.running.as_ref().and_then(|running| {
        packages::settled(&running.started.call)
            .map(|ended| (ended, running.started.app.clone(), running.started.change))
    });
    if let Some((ended, app, change)) = ended
        && let Some(started) = state.running.take()
    {
        let result = match ended {
            Ok(()) => Ok(Finished::status(status_of(change, &app))),
            Err(reason) => Err(reason),
        };
        state.operation_notice = Some(match &result {
            Ok(finished) => finished.status.clone(),
            Err(reason) => reason.clone(),
        });
        if change != packages::Change::Get {
            state.refresh_requested = true;
        }
        if let Some(done) = started.done {
            let _ = done.send(result);
        }
        changed = true;
    }
    changed |= advance_opens(state);
    if changed {
        let status = state
            .operation_notice
            .clone()
            .unwrap_or_else(|| CATALOG.t("store-catalogue-refreshed"));
        return publish(snapshots, state, &status, error);
    }
    true
}

/// Put what the workspace runs and what this device holds into each catalogue row.
fn apply_installed(state: &mut State) {
    let installed = state.packages.installed().clone();
    for item in &mut state.base_items {
        if !matches!(item.source, StoreSource::Tilo { .. }) {
            continue;
        }
        item.installed = installed.state(&item.id, item.latest_version());
    }
}

/// Answer each press on Open whose package reached this device.
fn advance_opens(state: &mut State) -> bool {
    let roots: std::collections::BTreeMap<String, String> = state
        .packages
        .installed()
        .device
        .iter()
        .filter(|(_, row)| row.installed())
        .map(|(app, row)| (app.clone(), row.root.clone()))
        .collect();
    let opens = std::mem::take(&mut state.opens);
    let (arrived, waiting): (Vec<OpenWait>, Vec<OpenWait>) = opens
        .into_iter()
        .partition(|wait| roots.contains_key(&wait.app));
    let (expired, waiting): (Vec<OpenWait>, Vec<OpenWait>) = waiting
        .into_iter()
        .partition(|wait| wait.since.elapsed() >= OPEN_WAIT);
    state.opens = waiting;
    let mut changed = false;
    for press in arrived {
        let outcome = Ok(Finished {
            status: CATALOG.t_args(
                "store-progress-opening",
                &[("name", ui::i18n::ArgValue::Text(press.name.clone()))],
            ),
            case: None,
            launch: Some(crate::actions::ApplicationLaunch {
                app_id: press.app.clone(),
                package_root: roots.get(&press.app).cloned(),
            }),
        });
        if let Some(done) = press.done {
            let _ = done.send(outcome);
        }
        changed = true;
    }
    for press in expired {
        if let Some(done) = press.done {
            let _ = done.send(Err(CATALOG.t_args(
                "store-open-not-installed",
                &[("name", ui::i18n::ArgValue::Text(press.name.clone()))],
            )));
        }
        changed = true;
    }
    changed
}

/// The window's line for a change that ended.
fn status_of(change: packages::Change, app: &str) -> String {
    let name = ui::i18n::ArgValue::Text(app.to_owned());
    CATALOG.t_args(
        match change {
            packages::Change::Get => "store-app-got",
            packages::Change::Update => "store-app-updated",
            packages::Change::Uninstall => "store-app-removed",
            packages::Change::Pin => "store-kept",
            packages::Change::Unpin => "store-released",
        },
        &[("name", name)],
    )
}

/// Publish what the window shows, with the apps this device is fetching.
fn publish(
    snapshots: &Snapshots,
    state: &State,
    status_line: &str,
    operation_error: Option<String>,
) -> bool {
    snapshots.send(Snapshot {
        items: state.base_items.clone(),
        status_line: status_line.to_owned(),
        query: None,
        detail: None,
        remotes: state.remotes.clone(),
        flatpak_available: state.flatpak_available,
        flatpak_error: state.flatpak_error.clone(),
        operation_error: operation_error
            .or_else(|| state.catalogue_error.clone())
            .or_else(|| state.flatpak_error.clone()),
        unanswered_repositories: state.unanswered_repositories.clone(),
        expired_repositories: state.expired_repositories.clone(),
        blocks: state.blocks.clone(),
        updating: None,
        installs: installs_of(state),
    })
}

/// The apps this device is fetching, by app id, with the bytes it has.
fn installs_of(state: &State) -> std::collections::BTreeMap<String, ui::arriving::Progress> {
    let installed = state.packages.installed();
    let mut progress = std::collections::BTreeMap::new();
    for item in &state.base_items {
        if let Some(progress_of) = installed.progress(&item.id) {
            progress.insert(item.id.clone(), progress_of);
        }
    }
    progress
}

/// How long a press on Open waits for the app's package to reach this device.
const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Keep the catalogue the refresh settled on for the next window's first
/// frame. Rows a handed file added (an opened `.flatpakref`, a reviewed
/// local package) belong to this window only and are left out.
fn keep_catalogue(state: &State) {
    let opened = state
        .opened_ref
        .as_ref()
        .map(|reference| reference.app_id.as_str());
    let imported = state
        .imported_package
        .as_ref()
        .map(|package| package.item.id.as_str());
    let items: Vec<StoreItem> = state
        .base_items
        .iter()
        .filter(|item| Some(item.id.as_str()) != opened && Some(item.id.as_str()) != imported)
        .cloned()
        .collect();
    if items.is_empty() {
        return;
    }
    if let Err(error) = crate::catalogue::cache::save(&items, &crate::catalogue::desktop_locale()) {
        tracing::warn!(%error, "could not keep the catalogue for the next window");
    }
}

/// How long the Store waits for this device's package cache to read a repository.
const CACHE_READ_WAIT: Duration = Duration::from_secs(60);

/// How long a change the worker waits for may take: a runtime fetched, a package file handed to
/// the workspace.
const CHANGE_WAIT: Duration = Duration::from_secs(30 * 60);

/// Review the runtime build `runtime` that the enabled repositories offer (only
/// `selected_registry` when one is named), as this device's package cache reads their offers.
fn runtime_details(
    runtime: &str,
    selected_registry: Option<&str>,
    state: &mut State,
) -> Result<StoreItem, String> {
    let repositories = crate::config::repositories()?;
    if selected_registry
        .is_some_and(|name| !repositories.enabled().any(|registry| registry.name == name))
    {
        return Err(CATALOG.t("store-runtime-unavailable"));
    }
    let mut selected = None;
    for registry in repositories
        .enabled()
        .filter(|registry| selected_registry.is_none_or(|name| registry.name == name))
    {
        let mut arguments = serde_json::Map::new();
        arguments.insert("registry".into(), registry.name.clone().into());
        arguments.insert("runtime".into(), runtime.to_owned().into());
        let answer = state
            .packages
            .cache_call_now("runtime-offer", arguments, CACHE_READ_WAIT)?;
        let offer = answer
            .get("offer")
            .ok_or_else(|| CATALOG.t("store-runtime-offer-mismatch"))?;
        let Some(offer) = serde_json::from_value::<Option<app_cli::CatalogueApp>>(offer.clone())
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if offer.runtime.as_deref() != Some(runtime)
            || offer.registry != registry.name
            || offer.trust != "publisher-signed"
            || !ui::registry::is_runtime_package_id(&offer.package_id)
            || offer.version.is_empty()
            || offer.publisher_pubkey_hex.len() != 64
            || !offer
                .publisher_pubkey_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || offer.tapp_size_bytes == 0
            || offer.tapp_root_hash_hex.len() != 64
            || !offer
                .tapp_root_hash_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CATALOG.t("store-runtime-offer-mismatch"));
        }
        if selected.is_some() {
            return Err(CATALOG.t("store-runtime-offer-ambiguous"));
        }
        selected = Some(runtime_item(offer, state.packages.installed()));
    }
    selected.ok_or_else(|| CATALOG.t("store-runtime-unavailable"))
}

/// The Store's row for a runtime `offer`: installed only when this device has exactly the
/// package it offers.
fn runtime_item(offer: app_cli::CatalogueApp, installed: &packages::Installed) -> StoreItem {
    let installed = installed
        .device
        .get(&offer.package_id)
        .filter(|row| {
            row.installed()
                && row.version == offer.version
                && row.root.eq_ignore_ascii_case(&offer.tapp_root_hash_hex)
                && row
                    .publisher
                    .eq_ignore_ascii_case(&offer.publisher_pubkey_hex)
        })
        .map_or(crate::model::InstalledState::Not, |row| {
            crate::model::InstalledState::Installed {
                version: row.version.clone(),
                available: offer.version.clone(),
                pinned: installed
                    .workspace
                    .get(&offer.package_id)
                    .is_some_and(|app| app.pinned),
                user_managed: true,
            }
        });
    StoreItem {
        bot: None,
        awaiting_developer: false,
        listing: crate::model::Listing::default(),
        id: offer.package_id,
        name: offer.display_name,
        description: offer.description,
        tagline: offer.tagline,
        supported_languages: offer.supported_languages,
        categories: offer.category.into_iter().collect(),
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: offer.registry,
        },
        trust: crate::model::StoreTrust::PublisherSigned,
        versions: vec![crate::model::StoreItemVersion {
            version: offer.version,
            released_at_unix: 0,
            download_url: Some(offer.download_url),
            root_hash_hex: Some(offer.tapp_root_hash_hex),
            surface_hex: (!offer.tapp_surface_hex.is_empty()).then_some(offer.tapp_surface_hex),
            size_bytes: offer.tapp_size_bytes,
            runtime: offer.runtime,
        }],
        publisher_pubkey_hex: Some(offer.publisher_pubkey_hex),
        installed,
    }
}

/// Fetch the runtime `item` names into this computer's package cache (`runtime-fetch`), bound to
/// the package root and publisher its review named: a runtime is a payload of this computer, not
/// a version a workspace runs.
fn install_runtime(item: &StoreItem, state: &mut State) -> crate::actions::ActionOutcome {
    use crate::actions::ActionOutcome;
    if let Some((_, reason)) = item.unavailable_action(ActionKind::RuntimeInstall) {
        return ActionOutcome::fail(reason);
    }
    let StoreSource::Tilo { registry_name } = &item.source else {
        return ActionOutcome::fail("This package has no Tilo repository.");
    };
    let Some(version) = item.versions.first() else {
        return ActionOutcome::fail("registry entry has no version");
    };
    let (Some(runtime), Some(package), Some(publisher)) = (
        version.runtime.as_deref(),
        version.root_hash_hex.as_deref(),
        item.publisher_pubkey_hex.as_deref(),
    ) else {
        return ActionOutcome::fail(CATALOG.t("store-runtime-offer-mismatch"));
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert("registry".into(), registry_name.clone().into());
    arguments.insert("runtime".into(), runtime.to_owned().into());
    arguments.insert("package".into(), package.to_owned().into());
    arguments.insert("publisher".into(), publisher.to_owned().into());
    match state
        .packages
        .cache_call_now("runtime-fetch", arguments, CHANGE_WAIT)
    {
        Ok(_) => ActionOutcome::ok(CATALOG.t_args(
            "store-runtime-installed",
            &[
                ("name", ui::i18n::ArgValue::Text(item.name.clone())),
                ("version", ui::i18n::ArgValue::Text(version.version.clone())),
            ],
        )),
        Err(error) => ActionOutcome::fail(format!("{}: install refused: {error}", item.name)),
    }
}

/// Install the reviewed `package`: hand its file to the workspace, then name that version on the
/// workspace's tree, which every device installs, checking it again.
fn install_imported(package: &crate::package_import::ImportedPackage, state: &State) -> Outcome {
    let file = state.packages.upload(&package.bytes, CHANGE_WAIT)?;
    let release = package.release(&file)?;
    let started = state.packages.start(
        packages::Change::Get,
        &package.expected.app_id,
        Some(serde_json::Value::Object(release)),
    )?;
    state.packages.await_call(&started.call, CHANGE_WAIT)?;
    Ok(Finished::status(CATALOG.t("store-package-installed")))
}

#[allow(
    clippy::too_many_lines,
    reason = "the serialized worker owns one exhaustive request dispatch; splitting it would obscure that every blocking Store effect crosses this queue"
)]
fn handle(
    request: Request,
    state: &mut State,
    snapshots: &Snapshots,
    done: &mut Option<Done>,
) -> (bool, Option<Outcome>) {
    match request {
        Request::Refresh => {
            let error = CATALOG.t("store-refresh-in-progress");
            let alive = send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &error,
                None,
                None,
                Some(error.clone()),
            );
            (alive, Some(Err(error)))
        }
        Request::BrowseRemote(id) => browse_remote(&id, state, snapshots),
        Request::Detail(id) => (detail(&id, state, snapshots), None),
        Request::RuntimeDetails { runtime, registry } => {
            let opened = runtime_details(&runtime, registry.as_deref(), &mut *state);
            let (label, detail, error) = match &opened {
                Ok(item) => (
                    crate::resource::CATALOG.t_args(
                        "store-runtime-reviewed",
                        &[("name", ui::i18n::ArgValue::Text(item.name.clone()))],
                    ),
                    Some(DetailUpdate::OpenedReference(item.clone())),
                    None,
                ),
                Err(error) => (error.clone(), None, Some(error.clone())),
            };
            let alive = send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                detail,
                error,
            );
            (alive, Some(opened.map(|_| Finished::status(label))))
        }
        Request::OpenReference { path, contents } => {
            let opened = open_reference(path, contents, state);
            let (label, detail, error) = match &opened {
                Ok(item) => (
                    format!("Opened {} from its reference.", item.name),
                    Some(DetailUpdate::OpenedReference(item.clone())),
                    None,
                ),
                Err(error) => (error.clone(), None, Some(error.clone())),
            };
            let alive = send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                detail,
                error,
            );
            (alive, Some(opened.map(|_| Finished::status(label))))
        }
        Request::ImportPackage(input) => {
            state.imported_package = None;
            let reviewed = input.read().and_then(|bytes| {
                crate::package_import::review(bytes.clone(), &state.base_items).or_else(
                    |catalogue_error| {
                        crate::package_import::review_local(
                            bytes,
                            &app_cli::config::trust(),
                            state.packages.installed(),
                            catalogue_error,
                        )
                    },
                )
            });
            let (status, detail, error) = match reviewed {
                Ok(package) => {
                    let detail = DetailUpdate::ImportedPackage {
                        item: package.item.clone(),
                        root: package.expected.package_root.to_hex(),
                        version: package.expected.version.clone(),
                    };
                    state.imported_package = Some(package);
                    (CATALOG.t("store-package-ready"), Some(detail), None)
                }
                Err(error) => (error.clone(), None, Some(error)),
            };
            let alive = send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &status,
                None,
                detail,
                error.clone(),
            );
            (
                alive,
                Some(error.map_or_else(|| Ok(Finished::status(status)), Err)),
            )
        }
        Request::InstallImported { app, root } => {
            let result = state
                .imported_package
                .as_ref()
                .filter(|package| {
                    package.expected.app_id == app && package.expected.package_root.to_hex() == root
                })
                .ok_or_else(|| CATALOG.t("store-package-review-required"))
                .and_then(|package| {
                    // Recheck the immutable handed bytes against the latest independent offers,
                    // or the workspace's trust as it is now.
                    let current = if package.local.is_some() {
                        crate::package_import::review_local(
                            package.bytes.to_vec(),
                            &app_cli::config::trust(),
                            state.packages.installed(),
                            CATALOG.t("store-package-offer-required"),
                        )?
                    } else {
                        crate::package_import::review(package.bytes.to_vec(), &state.base_items)?
                    };
                    if current.expected != package.expected {
                        return Err(CATALOG.t("store-package-review-required"));
                    }
                    install_imported(&current, state)
                });
            if result.is_ok() {
                state.imported_package = None;
                state.refresh_requested = true;
            }
            let status = result
                .as_ref()
                .map_or_else(Clone::clone, |finished| finished.status.clone());
            let alive = send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &status,
                None,
                None,
                result.as_ref().err().cloned(),
            );
            (alive, Some(result))
        }
        Request::RefreshRemotes => {
            refresh_flatpak(state);
            let outcome = state.flatpak_error.as_ref().map_or_else(
                || {
                    Ok(Finished::status(format!(
                        "Loaded {} Flatpak remotes.",
                        state.remotes.len()
                    )))
                },
                |error| Err(error.clone()),
            );
            let label = outcome
                .as_ref()
                .map_or_else(Clone::clone, |finished| finished.status.clone());
            let alive = send(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
            );
            (alive, Some(outcome))
        }
        // The device's Flatpak repositories are the device's: a person adds or removes one
        // through its daemon, never from the Store's sandbox.
        Request::RemoteAdd { .. } | Request::RemoteSetEnabled { .. } | Request::RemoteRemove(_) => {
            refresh_flatpak(state);
            remote_result(
                snapshots,
                state,
                Err("This device's Flatpak repositories are managed on the device.".to_string()),
            )
        }
        Request::UpdateAll(items) => update_all(&items, state, snapshots),
        Request::Act(item, action) => {
            if action == ActionKind::Open && matches!(item.source, StoreSource::Tilo { .. }) {
                return open_app(&item, state, snapshots, done);
            }
            if let Some(change) = tree_change(&item, action) {
                return activate_tree_change(&item, action, change, state, snapshots, done);
            }
            let outcome = if action == ActionKind::RuntimeInstall {
                install_runtime(&item, state)
            } else {
                crate::actions::execute(&item, action)
            };
            // The refresh this asks for reads the device again in the
            // background; the answer does not wait for Flatpak to be asked.
            state.refresh_requested = true;
            let operation_error = (!outcome.ok).then(|| outcome.status_line.clone());
            let alive = send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &outcome.status_line,
                None,
                None,
                operation_error,
            );
            let finished = if outcome.ok {
                Ok(Finished {
                    status: outcome.status_line,
                    case: None,
                    launch: outcome.launch,
                })
            } else {
                Err(outcome.status_line)
            };
            (alive, Some(finished))
        }
        Request::ExternalModeration(_) => (
            true,
            Some(Err(
                "External moderation requires the admitted action transport.".into(),
            )),
        ),
        Request::RefreshModeration {
            registry_name,
            case_id,
        } => {
            let registries = crate::config::repositories().unwrap_or_default();
            let outcome = moderation_registry(&registries, &registry_name)
                .and_then(|registry| {
                    crate::moderation_client::fetch_verified_case(&registry, &case_id)
                })
                .map(|case| Finished {
                    status: format!(
                        "Moderation status refreshed: {} is {}.",
                        case.case_id,
                        crate::app::moderation_status_label(case.status)
                    ),
                            case: Some(case),
                    launch: None,
                })
                .map_err(|error| {
                    format!(
                        "Registry request failed: {error} The collaborative draft and last confirmed status are unchanged; retry when ready."
                    )
                });
            (
                moderation_snapshot(snapshots, state, &outcome),
                Some(outcome),
            )
        }
    }
}

/// The change of the workspace's tree `action` on `item` asks for, when it is one: a Tilo app's
/// version, or a Flatpak named on the tree for the device's daemon to install.
fn tree_change(item: &StoreItem, action: ActionKind) -> Option<packages::Change> {
    let flatpak = matches!(
        item.source,
        StoreSource::Flathub | StoreSource::FlatpakRemote { .. }
    );
    match action {
        ActionKind::Get if !flatpak => Some(packages::Change::Get),
        ActionKind::Update if !flatpak => Some(packages::Change::Update),
        ActionKind::Remove if !flatpak => Some(packages::Change::Uninstall),
        ActionKind::MarkInstalled if !flatpak => Some(packages::Change::Pin),
        ActionKind::Unmark if !flatpak => Some(packages::Change::Unpin),
        // A Flatpak's update names its reference again: the daemon installs what the tree names.
        ActionKind::FlatpakInstall | ActionKind::FlatpakUpdate if flatpak => {
            Some(packages::Change::Get)
        }
        ActionKind::FlatpakRemove if flatpak => Some(packages::Change::Uninstall),
        _ => None,
    }
}

/// The app id `change` of `item` names on the workspace's tree, and the release it records: a
/// Tilo app's newest version, or a Flatpak's reference.
fn tree_release(
    item: &StoreItem,
    change: packages::Change,
) -> Result<(String, Option<serde_json::Value>), String> {
    if matches!(
        item.source,
        StoreSource::Flathub | StoreSource::FlatpakRemote { .. }
    ) {
        let (app, release) = crate::actions::flatpak_release(item);
        let release =
            (change == packages::Change::Get).then_some(serde_json::Value::Object(release));
        return Ok((app, release));
    }
    let release = match change {
        packages::Change::Get | packages::Change::Update => Some(release_of(item)?),
        packages::Change::Uninstall | packages::Change::Pin | packages::Change::Unpin => None,
    };
    Ok((item.id.clone(), release))
}

/// A Tilo app's Get, update, pin, unpin or uninstall, or a Flatpak's install, update or removal:
/// each starts the workspace's change and is answered as it ends, the device's fetch going on
/// after a Get.
fn activate_tree_change(
    item: &StoreItem,
    action: ActionKind,
    change: packages::Change,
    state: &mut State,
    snapshots: &Snapshots,
    done: &mut Option<Done>,
) -> (bool, Option<Outcome>) {
    if state.running.is_some() {
        return (
            true,
            Some(Err(
                "Another change to this workspace's apps is still under way.".to_owned(),
            )),
        );
    }
    let name = item.name.clone();
    if matches!(action, ActionKind::Get | ActionKind::FlatpakInstall)
        && let Some((_, reason)) = item.unavailable_action(action)
    {
        return (true, Some(Err(reason)));
    }
    if action == ActionKind::Get
        && !state.packages.installed().workspace.contains_key(&item.id)
        && item.versions.is_empty()
    {
        return (
            true,
            Some(Err("This app has no version its source offers.".to_owned())),
        );
    }
    let (id, release) = match tree_release(item, change) {
        Ok(found) => found,
        Err(reason) => return (true, Some(Err(reason))),
    };
    let started = match state.packages.start(change, &id, release) {
        Ok(started) => started,
        Err(reason) => return (true, Some(Err(reason))),
    };
    let progress = crate::app::progress_label(change.progress(), &name);
    state.operation_notice = Some(progress.clone());
    state.running = Some(Running {
        started,
        done: done.take(),
    });
    (publish(snapshots, state, &progress, None), None)
}

/// A press on Open: answered at once from this device's package, or kept until it arrives.
fn open_app(
    item: &StoreItem,
    state: &mut State,
    snapshots: &Snapshots,
    done: &mut Option<Done>,
) -> (bool, Option<Outcome>) {
    if !state.packages.installed().workspace.contains_key(&item.id) {
        return (
            true,
            Some(Err(
                "This workspace runs no version of the app, so there is nothing to open."
                    .to_owned(),
            )),
        );
    }
    if let Some(root) = state.packages.installed().root(&item.id) {
        return (
            true,
            Some(Ok(Finished {
                status: CATALOG.t_args(
                    "store-progress-opening",
                    &[("name", ui::i18n::ArgValue::Text(item.name.clone()))],
                ),
                case: None,
                launch: Some(crate::actions::ApplicationLaunch {
                    app_id: item.id.clone(),
                    package_root: Some(root.to_owned()),
                }),
            })),
        );
    }
    state.opens.push(OpenWait {
        app: item.id.clone(),
        name: item.name.clone(),
        done: done.take(),
        since: std::time::Instant::now(),
    });
    let status = CATALOG.t_args(
        "store-progress-installing",
        &[("name", ui::i18n::ArgValue::Text(item.name.clone()))],
    );
    (publish(snapshots, state, &status, None), None)
}

/// The release `item`'s newest version names, as the workspace's install records it.
fn release_of(item: &StoreItem) -> Result<serde_json::Value, String> {
    let version = item
        .versions
        .first()
        .ok_or_else(|| "This app has no version its source offers.".to_owned())?;
    let package = version
        .root_hash_hex
        .as_deref()
        .ok_or_else(|| "This app's version names no signed package.".to_owned())?;
    let publisher = item
        .publisher_pubkey_hex
        .as_deref()
        .ok_or_else(|| "This app's version names no publisher.".to_owned())?;
    Ok(app_cli::device::release(
        &item.id,
        &version.version,
        version.surface_hex.as_deref().unwrap_or_default(),
        package,
        publisher,
    ))
}

/// The reference `path` names, or its `contents` kept in the Store's cache
/// so a confirmed install can name the file; its app joins the catalogue.
fn open_reference(
    path: Option<PathBuf>,
    contents: Option<String>,
    state: &mut State,
) -> Result<StoreItem, String> {
    let reference = match (path, contents) {
        (Some(path), _) => FlatpakRef::from_path(&path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?,
        (None, Some(contents)) => {
            let directory = crate::config::cache_dir()
                .map(|cache| cache.join("opened-references"))
                .ok_or_else(|| "There is no cache to keep the reference in.".to_string())?;
            std::fs::create_dir_all(&directory)
                .map_err(|error| format!("Could not keep the reference: {error}"))?;
            let parsed = FlatpakRef::parse(&contents, PathBuf::new());
            match parsed {
                Some(reference) if crate::reports::valid_app_id(&reference.app_id) => {
                    let path = directory.join(format!("{}.flatpakref", reference.app_id));
                    tilo_files::native::atomic::write(&path, contents.as_bytes())
                        .map_err(|error| format!("Could not keep the reference: {error}"))?;
                    Some(FlatpakRef {
                        source_path: path,
                        ..reference
                    })
                }
                _ => None,
            }
        }
        (None, None) => return Err("Name a Flatpak reference to open.".into()),
    }
    .ok_or_else(|| "This is not a Flatpak reference.".to_string())?;
    let version = state
        .installed
        .iter()
        .find_map(|(id, version)| (id == &reference.app_id).then(|| version.clone()));
    let item = reference.clone().into_store_item(version);
    state.details.pop(&item.id);
    state.base_items.retain(|known| known.id != item.id);
    state.base_items.insert(0, item.clone());
    state.opened_ref = Some(reference);
    sync_authoritative_catalogue(state);
    Ok(item)
}

fn remote_result(
    snapshots: &Snapshots,
    state: &State,
    result: Result<String, String>,
) -> (bool, Option<Outcome>) {
    let (label, error) = match &result {
        Ok(label) => (label.clone(), None),
        Err(error) => (error.clone(), Some(error.clone())),
    };
    let alive = send_tracked(
        snapshots,
        state,
        state.base_items.clone(),
        &label,
        None,
        None,
        error,
    );
    (alive, Some(result.map(Finished::status)))
}

fn moderation_snapshot(snapshots: &Snapshots, state: &State, outcome: &Outcome) -> bool {
    let (label, error) = match outcome {
        Ok(finished) => (finished.status.clone(), None),
        Err(error) => (error.clone(), Some(error.clone())),
    };
    send_tracked(
        snapshots,
        state,
        state.base_items.clone(),
        &label,
        None,
        None,
        error,
    )
}

fn browse_remote(id: &str, state: &mut State, snapshots: &Snapshots) -> (bool, Option<Outcome>) {
    let result = state
        .remotes
        .iter()
        .find(|remote| remote.resource_id() == id && !remote.disabled)
        .ok_or_else(|| {
            "The configured repository is no longer available. Refresh and try again.".to_string()
        })
        .and_then(|remote| {
            let rows = crate::device::catalogue(remote);
            let mut items =
                crate::sources::remote_catalogue_items(&rows, remote, &state.flatpak_installed);
            for item in &mut items {
                crate::sources::apply_flatpak_origin(item, &state.installed_origins);
            }
            Ok((items, remote.title.clone()))
        });
    if let Some(remote) = state
        .remotes
        .iter_mut()
        .find(|remote| remote.resource_id() == id)
    {
        remote.catalogue_error = result.as_ref().err().cloned();
    }
    match result {
        Ok((items, title)) => {
            for item in &items {
                if let Some(existing) = state
                    .base_items
                    .iter_mut()
                    .find(|existing| existing.id == item.id)
                {
                    *existing = item.clone();
                } else {
                    state.base_items.push(item.clone());
                }
            }
            state.details.clear();
            sync_authoritative_catalogue(state);
            let label = crate::resource::CATALOG.t_args(
                "store-remote-results",
                &[
                    ("name", ui::i18n::ArgValue::Text(title)),
                    ("count", ui::i18n::ArgValue::Text(items.len().to_string())),
                ],
            );
            let alive = send(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
            );
            (alive, Some(Ok(Finished::status(label))))
        }
        Err(error) => {
            let label = crate::resource::CATALOG.t_args(
                "store-remote-error",
                &[("error", ui::i18n::ArgValue::Text(error))],
            );
            let alive = send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
                Some(label.clone()),
            );
            (alive, Some(Err(label)))
        }
    }
}

fn detail(id: &str, state: &mut State, snapshots: &Snapshots) -> bool {
    let loaded = if let Some(cached) = state.details.get(id).cloned() {
        Some(cached)
    } else if let Some(item) = state
        .base_items
        .iter()
        .find(|item| {
            item.id == id
                && (item.install_ref_path.is_some()
                    || matches!(
                        item.source,
                        StoreSource::Tilo { .. } | StoreSource::FlatpakRemote { .. }
                    ))
        })
        .cloned()
    {
        state.details.put(id.to_string(), item.clone());
        Some(item)
    } else {
        // A Flathub app's page fetches its details on the Store's fetch
        // thread (`catalogue::fetcher`): the serialized package worker never
        // waits on the network for a page.
        state.base_items.iter().find(|item| item.id == id).cloned()
    };
    let label = loaded.as_ref().map_or_else(
        || format!("Could not load details for {id}."),
        |item| format!("Loaded details for {}.", item.name),
    );
    send_tracked(
        snapshots,
        state,
        state.base_items.clone(),
        &label,
        None,
        Some(DetailUpdate::Requested {
            id: id.to_string(),
            item: loaded,
        }),
        None,
    )
}

fn send(
    snapshots: &Snapshots,
    state: &State,
    items: Vec<StoreItem>,
    status_line: &str,
    query: Option<String>,
    detail: Option<StoreItem>,
) -> bool {
    let detail = detail.map(|item| DetailUpdate::Requested {
        id: item.id.clone(),
        item: Some(item),
    });
    send_tracked(snapshots, state, items, status_line, query, detail, None)
}

fn update_all(
    items: &[StoreItem],
    state: &mut State,
    snapshots: &Snapshots,
) -> (bool, Option<Outcome>) {
    let total = items.len();
    let mut failures = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let progress = Snapshot {
            items: state.base_items.clone(),
            status_line: crate::resource::CATALOG.t_args(
                "store-update-all-progress",
                &[
                    (
                        "index",
                        ui::i18n::ArgValue::Text(index.saturating_add(1).to_string()),
                    ),
                    ("total", ui::i18n::ArgValue::Text(total.to_string())),
                    ("name", ui::i18n::ArgValue::Text(item.name.clone())),
                ],
            ),
            query: None,
            detail: None,
            remotes: state.remotes.clone(),
            flatpak_available: state.flatpak_available,
            flatpak_error: state.flatpak_error.clone(),
            operation_error: None,
            unanswered_repositories: state.unanswered_repositories.clone(),
            expired_repositories: state.expired_repositories.clone(),
            blocks: state.blocks.clone(),
            updating: Some(item.id.clone()),
            installs: installs_of(state),
        };
        if !snapshots.progress(progress) {
            return (false, None);
        }
        let refusal = update_tree(item, state);
        if let Some(reason) = refusal {
            failures.push(format!("{}: {reason}", item.name));
        }
    }
    state.refresh_requested = true;
    let summary = update_all_summary(total, &failures);
    let operation_error = (!failures.is_empty()).then(|| summary.clone());
    let alive = send_tracked(
        snapshots,
        state,
        state.base_items.clone(),
        &summary,
        None,
        None,
        operation_error,
    );
    let finished = if failures.is_empty() {
        Ok(Finished::status(summary))
    } else {
        Err(summary)
    };
    (alive, Some(finished))
}

/// Move one app the workspace runs to the newest version its source offers, or name a Flatpak's
/// reference again, waiting for the workspace to take the change.
fn update_tree(item: &StoreItem, state: &State) -> Option<String> {
    let change = match item.source {
        StoreSource::Flathub | StoreSource::FlatpakRemote { .. } => packages::Change::Get,
        StoreSource::Tilo { .. } => packages::Change::Update,
    };
    let (app, release) = match tree_release(item, change) {
        Ok(found) => found,
        Err(reason) => return Some(reason),
    };
    let started = match state.packages.start(change, &app, release) {
        Ok(started) => started,
        Err(reason) => return Some(reason),
    };
    state
        .packages
        .await_call(&started.call, std::time::Duration::from_secs(120))
        .err()
}

fn update_all_summary(total: usize, failures: &[String]) -> String {
    use ui::i18n::ArgValue;
    let count = |n: usize| ArgValue::Number(f64::from(u32::try_from(n).unwrap_or(u32::MAX)));
    match failures {
        [] => crate::resource::CATALOG.t_args("store-update-all-done", &[("total", count(total))]),
        [first, rest @ ..] => crate::resource::CATALOG.t_args(
            if rest.is_empty() {
                "store-update-all-failed"
            } else {
                "store-update-all-failed-more"
            },
            &[
                ("updated", count(total.saturating_sub(failures.len()))),
                ("total", count(total)),
                ("first", ArgValue::Text(first.clone())),
                ("more", count(rest.len())),
            ],
        ),
    }
}

fn send_tracked(
    snapshots: &Snapshots,
    state: &State,
    items: Vec<StoreItem>,
    status_line: &str,
    query: Option<String>,
    detail: Option<DetailUpdate>,
    operation_error: Option<String>,
) -> bool {
    snapshots.send(Snapshot {
        items,
        status_line: status_line.to_string(),
        query,
        detail,
        remotes: state.remotes.clone(),
        flatpak_available: state.flatpak_available,
        flatpak_error: state.flatpak_error.clone(),
        operation_error: operation_error
            .or_else(|| state.catalogue_error.clone())
            .or_else(|| state.flatpak_error.clone()),
        unanswered_repositories: state.unanswered_repositories.clone(),
        expired_repositories: state.expired_repositories.clone(),
        blocks: state.blocks.clone(),
        updating: None,
        installs: installs_of(state),
    })
}

fn moderation_registry(
    registries: &RegistriesFile,
    registry_name: &str,
) -> Result<RegistryConfig, crate::moderation_client::ModerationClientError> {
    registries
        .registries
        .iter()
        .find(|registry| registry.enabled && registry.name == registry_name)
        .cloned()
        .ok_or_else(|| {
            crate::moderation_client::ModerationClientError::Offline(
                "the app's registry is disabled or no longer configured".into(),
            )
        })
}

fn refresh_flatpak(state: &mut State) {
    let status = crate::device::status();
    state.flatpak_error = None;
    state.flatpak_available = status.available;
    state.flatpak_installed = installed_flatpaks(&status.installed);
    state.installed_origins = crate::flathub_client::installed_origins(&status.installed);
    state.remotes = crate::flathub_client::refreshed_remotes(&status.remotes, &state.remotes);
}

fn refresh(state: &mut State) {
    refresh_flatpak(state);
    state.refresh_requested = true;
}

/// Publish the one authoritative catalogue directory. Presentation snapshots
/// may contain a local filtered search result or a detail projection, neither
/// of which is allowed to remove unrelated apps from Ctrl+K, agents, history,
/// or resource resolution. This executes on the serialized worker directly
/// after a true `base_items` refresh, never on the UI thread.
fn sync_authoritative_catalogue(state: &State) {
    state.directory.sync(&state.base_items);
    state.directory.set_registries(state.registries.clone());
}

#[cfg(test)]
mod tests {
    #[test]
    fn update_all_reports_what_it_updated_and_the_first_failure() {
        use super::update_all_summary;
        crate::resource::CATALOG.ensure();
        assert_eq!(update_all_summary(1, &[]), "Updated 1 app.");
        assert_eq!(update_all_summary(3, &[]), "Updated 3 apps.");
        assert_eq!(
            update_all_summary(3, &["Notes: install refused".into()]),
            "Updated 2 of 3 apps; Notes: install refused"
        );
        assert_eq!(
            update_all_summary(3, &["Notes: offline".into(), "Clocks: offline".into()]),
            "Updated 1 of 3 apps; Notes: offline (and 1 more failed)"
        );
    }

    use super::*;
    use crate::{InstalledState, StoreItemVersion, StoreTrust};

    pub(super) fn item(id: &str, name: &str) -> StoreItem {
        StoreItem {
            bot: None,
            awaiting_developer: false,
            listing: crate::model::Listing::default(),
            id: id.into(),
            name: name.into(),
            description: format!("{name} Store test row"),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec!["Utilities".into()],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "main".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![StoreItemVersion {
                version: "1.0.0".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                surface_hex: None,
                size_bytes: 0,
                runtime: None,
            }],
            publisher_pubkey_hex: None,
            installed: InstalledState::Not,
        }
    }

    pub(super) fn state_with_items(items: Vec<StoreItem>, directory: StoreDirectory) -> State {
        State {
            operation_notice: None,
            refresh_requested: false,
            background_reads: false,
            packages: packages::AppPackages::new(ui::RemoteActions::current(), None),
            opened_ref: None,
            imported_package: None,
            base_items: items,
            flatpak_installed: Vec::new(),
            installed_origins: std::collections::HashMap::new(),
            remotes: Vec::new(),
            flatpak_available: false,
            flatpak_error: None,
            catalogue_error: None,
            unanswered_repositories: Vec::new(),
            expired_repositories: Vec::new(),
            details: LruCache::new(CACHE_CAP),
            directory,
            blocks: ui::registry::BlockList::empty(),
            registries: Vec::new(),
            flathub: None,
            force_refresh: false,
            running: None,
            opens: Vec::new(),
        }
    }

    pub(super) fn detail_installation_item(state: &mut State, id: &str) -> StoreItem {
        let (sender, received) = mpsc::channel();
        let snapshots = Snapshots {
            sender,
            wake: WakeHandle::new(),
            pending: Arc::new(AtomicUsize::new(0)),
        };
        assert!(handle(Request::Detail(id.into()), state, &snapshots, &mut None).0);
        let snapshot = received.recv().unwrap();
        assert!(snapshot.operation_error.is_none());
        let Some(DetailUpdate::Requested {
            id: requested,
            item: Some(item),
        }) = snapshot.detail
        else {
            panic!("the worker must answer the detail request");
        };
        assert_eq!(requested, id);
        let mut app = crate::app::StoreState::new();
        app.set_items(snapshot.items);
        app.set_detail(item);
        app.set_flatpak_available(true);
        app.package_operation(id, ActionKind::FlatpakInstall)
            .unwrap()
    }

    #[test]
    fn opening_a_reference_invalidates_conflicting_details_before_installation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("editor.flatpakref");
        let id = "org.example.Editor";
        let mut cached = item(id, "Editor");
        cached.source = StoreSource::Flathub;
        let mut state = state_with_items(vec![cached.clone()], StoreDirectory::default());
        state.details.put(id.into(), cached);
        state.details.put("other".into(), item("other", "Other"));
        for (branch, url) in [
            ("beta", "https://example.org/repo/"),
            ("nightly", "https://example.org/nightly/"),
        ] {
            std::fs::write(
                &path,
                format!("[Flatpak Ref]\nName={id}\nTitle=Editor\nBranch={branch}\nUrl={url}\n"),
            )
            .unwrap();
            let (sender, received) = mpsc::channel();
            let snapshots = Snapshots {
                sender,
                wake: WakeHandle::new(),
                pending: Arc::new(AtomicUsize::new(0)),
            };
            let (alive, outcome) = handle(
                Request::OpenReference {
                    path: Some(path.clone()),
                    contents: None,
                },
                &mut state,
                &snapshots,
                &mut None,
            );
            assert!(alive && outcome.unwrap().is_ok());
            assert!(matches!(
                received.recv().unwrap().detail,
                Some(DetailUpdate::OpenedReference(_))
            ));
            assert!(state.details.peek(id).is_none());
            assert!(state.details.peek("other").is_some());
            // The second pass also replaces a cached, previously opened reference.
            for _ in 0..2 {
                let installed = detail_installation_item(&mut state, id);
                assert_eq!(installed.install_ref_path.as_deref(), path.to_str());
                assert_eq!(installed.latest_version(), branch);
                assert_eq!(installed.source_url.as_deref(), Some(url));
            }
        }
    }

    /// A Flatpak installation that cannot be read reaches the window as its own error, apart
    /// from a device without Flatpak.
    #[test]
    fn flatpak_observation_failure_reaches_worker_snapshot_separately_from_missing() {
        let mut state = state_with_items(Vec::new(), StoreDirectory::default());
        state.flatpak_available = false;
        state.flatpak_error = Some("This device's Flatpak installation cannot be read.".into());
        let (sender, received) = mpsc::channel();
        let snapshots = Snapshots {
            sender,
            wake: WakeHandle::new(),
            pending: Arc::new(AtomicUsize::new(0)),
        };
        assert!(send(&snapshots, &state, Vec::new(), "", None, None));
        let snapshot = received.recv().unwrap();
        assert_eq!(snapshot.flatpak_error, state.flatpak_error);
        assert_eq!(snapshot.operation_error, state.flatpak_error);
        assert!(!snapshot.flatpak_available);
    }

    #[test]
    fn worker_cache_keeps_the_authoritative_directory() {
        {
            let directory = StoreDirectory::default();
            let state = state_with_items(
                vec![
                    item("dev.example.alpha", "Alpha"),
                    item("dev.example.beta", "Beta"),
                ],
                directory.clone(),
            );
            sync_authoritative_catalogue(&state);
            let (sender, received) = mpsc::channel();
            let snapshots = Snapshots {
                sender,
                wake: WakeHandle::new(),
                pending: Arc::new(AtomicUsize::new(0)),
            };

            assert!(send_tracked(
                &snapshots,
                &state,
                vec![item("dev.example.alpha", "Alpha")],
                "1 result for Alpha.",
                Some("Alpha".into()),
                None,
                None,
            ));
            assert_eq!(received.recv().expect("filtered snapshot").items.len(), 1);

            assert!(
                directory
                    .entries()
                    .iter()
                    .any(|entry| entry.title == "Beta"),
                "the directory keeps the app the filtered snapshot left out"
            );
        }

        {
            let directory = StoreDirectory::default();
            let mut state = state_with_items(
                vec![
                    item("dev.example.alpha", "Alpha"),
                    item("dev.example.beta", "Beta"),
                ],
                directory.clone(),
            );
            sync_authoritative_catalogue(&state);
            assert!(directory.app("dev.example.beta").is_some());

            state.base_items = vec![item("dev.example.gamma", "Gamma")];
            sync_authoritative_catalogue(&state);

            assert!(directory.app("dev.example.beta").is_none());
            assert!(directory.app("dev.example.gamma").is_some());
            let titles: Vec<String> = directory
                .entries()
                .into_iter()
                .map(|entry| entry.title)
                .collect();
            assert_eq!(titles, ["Gamma"]);
        }
    }
}
