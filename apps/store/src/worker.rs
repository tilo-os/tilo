//! Serialized package actor with cooperative catalogue refreshes. Independent
//! source reads share its authenticated connection; the UI drains snapshots.

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
use crate::sources::{
    installed_flatpak_to_item, installed_tilo_to_store_item, merge, tilo_to_store_item_on_channel,
};
use crate::{FlatpakRef, StoreItem};

mod refresh;

const REFRESH_INTERVAL: Duration = Duration::from_secs(900);
const CACHE_CAP: NonZeroUsize = match NonZeroUsize::new(128) {
    Some(cap) => cap,
    None => NonZeroUsize::MIN,
};

/// What the worker is asked to do. A request someone is waiting on (an
/// effect's job) carries a [`Done`] it is answered on.
#[derive(Clone, Debug)]
pub enum Request {
    ReviewRuntime(crate::runtime_recovery::RuntimeTarget),
    AcquireRuntime(Box<app_host::runtime_transaction::RuntimeReview>),
    /// Refresh every source; `force` refetches Flathub's catalogue even
    /// when the kept one is recent (the person asked for it).
    Refresh,
    BrowseRemote(String),
    Detail(String),
    /// Review a precise runtime through the trusted host.
    RuntimeDetails {
        runtime: String,
        registry: Option<String>,
    },
    /// Show the app a `.flatpakref` describes, ready to install: the file at
    /// `path`, or the reference's `contents` handed over without one.
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
    /// Update each app in turn, reporting progress, then refresh once.
    UpdateAll(Vec<StoreItem>),
    ExternalModeration(registry_types::ExternalModerationOperation),
    RefreshModeration {
        registry_name: String,
        case_id: String,
    },
}

/// How a request someone waits on ended: its status line and, for a
/// moderation request, the case the registry confirmed; or why it failed.
pub type Outcome = Result<Finished, String>;

/// Where the worker answers a request someone waits on.
pub type Done = mpsc::Sender<Outcome>;

/// A request that succeeded.
#[derive(Clone, Debug, Default)]
pub struct Finished {
    pub status: String,
    pub runtime_review: Option<app_host::runtime_transaction::RuntimeReview>,
    pub case: Option<registry_types::ModerationCase>,
    pub launch: Option<crate::actions::ApplicationLaunch>,
}

impl Finished {
    fn status(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            runtime_review: None,
            case: None,
            launch: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum DetailUpdate {
    /// Enrich the app the person already selected; this is not navigation.
    Requested { id: String, item: Option<StoreItem> },
    /// Opening a reference resolves its identity and intentionally opens it.
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
    /// Search responses name their query so stale results can be discarded.
    pub query: Option<String>,
    pub detail: Option<DetailUpdate>,
    pub remotes: Vec<Remote>,
    pub flatpak_available: bool,
    pub flatpak_error: Option<String>,
    pub operation_error: Option<String>,
    /// Enabled native repositories that have not answered the current
    /// refresh, so installed apps they list are described only locally.
    pub unanswered_repositories: Vec<String>,
    /// Who blocked which app, as the worker last read the block list.
    pub blocks: app_catalogue::BlockList,
    /// The app Update all is updating now, so its row shows the progress.
    pub updating: Option<String>,
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
        crate::resource::STORE_CATALOG.set_locale(&self.selected);
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
    pub fn spawn(directory: StoreDirectory, wake: WakeHandle) -> Self {
        Self::spawn_with_host(
            directory,
            wake,
            app_host::client::BlockingClient::from_service,
        )
    }

    #[allow(
        clippy::expect_used,
        reason = "thread spawn failure means the OS is out of resources; the surface attaching the worker has no fallible path to report it on"
    )]
    fn spawn_with_host(
        directory: StoreDirectory,
        wake: WakeHandle,
        connect: impl FnOnce() -> Result<app_host::client::BlockingClient, String> + Send + 'static,
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
                move || {
                    // Fluent managers are thread-local. Worker status must
                    // use the same selected app catalogue as its live view.
                    locale.apply();
                    let snapshots = Snapshots {
                        sender: snapshot_tx,
                        wake,
                        pending,
                    };
                    run(&request_rx, &snapshots, directory, connect());
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

    #[cfg(test)]
    pub(crate) fn with_connected_host(
        &self,
        directory: StoreDirectory,
        fd: std::os::fd::OwnedFd,
    ) -> Self {
        Self::spawn_with_host(directory, self.wake.clone(), move || {
            app_host::client::BlockingClient::from_connected_fd(fd)
        })
    }

    /// Queue `request`; whether the worker is still there to take it.
    pub fn request(&self, request: Request) -> bool {
        self.queue(request, None)
    }

    /// Queue `request` and answer it on `done`; whether the worker is still
    /// there to take it.
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

/// The UI's end of the worker's snapshots, woken for each one.
#[derive(Clone)]
struct Snapshots {
    sender: mpsc::Sender<Snapshot>,
    wake: WakeHandle,
    /// The UI's count of snapshots still to come: a request sends one, and
    /// a progress report adds its own.
    pending: Arc<AtomicUsize>,
}

impl Snapshots {
    fn send(&self, snapshot: Snapshot) -> bool {
        let sent = self.sender.send(snapshot).is_ok();
        self.wake.wake();
        sent
    }

    /// A snapshot beside the one that answers the request.
    fn progress(&self, snapshot: Snapshot) -> bool {
        self.pending.fetch_add(1, Ordering::Relaxed);
        self.send(snapshot)
    }
}

struct State {
    refresh_requested: bool,
    /// Passive source hydration must not erase a completed operation's notice.
    operation_notice: Option<String>,
    host: Option<app_host::client::BlockingClient>,
    opened_ref: Option<FlatpakRef>,
    imported_package: Option<crate::package_import::ImportedPackage>,
    base_items: Vec<StoreItem>,
    installed: Vec<(String, String)>,
    installed_origins: std::collections::HashMap<String, (String, bool)>,
    remotes: Vec<Remote>,
    flatpak_available: bool,
    flatpak_error: Option<String>,
    catalogue_error: Option<String>,
    /// See [`Snapshot::unanswered_repositories`].
    unanswered_repositories: Vec<String>,
    details: LruCache<String, StoreItem>,
    /// The current palette/action directory. Rebuilding it is linear in a
    /// catalogue snapshot, so it belongs on this serialized worker rather
    /// than in the UI's snapshot-drain loop.
    directory: StoreDirectory,
    /// Apps blocked by the person, a workspace or the device's administrator:
    /// the same list desktop search hides them by (`app_catalogue::BlockList`).
    blocks: app_catalogue::BlockList,
    /// The person's repositories as the last refresh read them. Kept here so
    /// publishing the directory never asks the host again: each ask runs
    /// `app repo list` on the host.
    registries: Vec<RegistryConfig>,
    /// Flathub's catalogue as the last refresh read it, so the next refresh
    /// lists Flathub's apps from its first snapshot while it reads again.
    flathub: Option<crate::catalogue::flathub::FlathubFeed>,
    /// The person asked for this refresh: Flathub's catalogue is read again
    /// even when the kept one is recent.
    force_refresh: bool,
}

fn initial_state(
    directory: StoreDirectory,
    host: Result<app_host::client::BlockingClient, String>,
) -> State {
    State {
        refresh_requested: false,
        operation_notice: None,
        host: host
            .map_err(|error| {
                tracing::error!(%error, "required app-host capability unavailable");
                error
            })
            .ok(),
        opened_ref: None,
        imported_package: None,
        // The catalogue the last window kept: a refresh keeps describing
        // every app it lists until that app's own source answers, so the
        // window never empties while the sources are asked again.
        base_items: crate::catalogue::cache::load().map_or_else(Vec::new, |cached| cached.items),
        installed: Vec::new(),
        installed_origins: std::collections::HashMap::new(),
        remotes: Vec::new(),
        flatpak_available: false,
        flatpak_error: None,
        catalogue_error: None,
        unanswered_repositories: Vec::new(),
        details: LruCache::new(CACHE_CAP),
        blocks: app_catalogue::BlockList::load(),
        directory,
        registries: Vec::new(),
        flathub: crate::catalogue::flathub::load(),
        force_refresh: false,
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
    let error = ui::i18n!("store-package-review-busy");
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
    host: Result<app_host::client::BlockingClient, String>,
) {
    let staging = crate::actions::tapp_staging_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("tilo-store-staged"));
    let _ = std::fs::create_dir_all(&staging);
    let mut state = initial_state(directory, host);
    let mut refresh = Some(refresh::Refresh::start(&state, None));
    let mut deferred_imports = std::collections::VecDeque::new();
    loop {
        if !poll_refresh(&mut refresh, &mut state, snapshots) {
            break;
        }
        let wait = if refresh.is_some() {
            Duration::from_millis(25)
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
                done,
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
                    refresh = Some(refresh::Refresh::start(&state, done));
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
                let (alive, outcome) = handle(request, &mut state, snapshots, &staging);
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
                if let (Some(done), Some(outcome)) = (done, outcome) {
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
                        refresh = Some(refresh::Refresh::start(&state, None));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) if refresh.is_none() => {
                snapshots.pending.fetch_add(1, Ordering::Relaxed);
                refresh = Some(refresh::Refresh::start(&state, None));
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
            .unwrap_or_else(|| ui::i18n!("store-catalogue-refreshed"));
        let alive = send(
            snapshots,
            state,
            state.base_items.clone(),
            &label,
            None,
            None,
        );
        if let Some(done) = done {
            // A refresh reports each source's error in the snapshot; it
            // completes once every source has been attempted, independently
            // of whether a particular source is offline.
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
                .unwrap_or_else(|| ui::i18n!("store-catalogue-loading")),
            query: None,
            detail: None,
            remotes: state.remotes.clone(),
            flatpak_available: state.flatpak_available,
            flatpak_error: state.flatpak_error.clone(),
            operation_error: state.catalogue_error.clone(),
            unanswered_repositories: state.unanswered_repositories.clone(),
            blocks: state.blocks.clone(),
            updating: None,
        })
}

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

/// Resolve only enabled repositories through the launcher-issued host capability.
fn runtime_details(
    runtime: &str,
    selected_registry: Option<&str>,
    state: &State,
) -> Result<StoreItem, String> {
    let host = state
        .host
        .as_ref()
        .ok_or_else(|| ui::i18n!("store-runtime-service-unavailable"))?;
    let repositories = crate::config::repositories(Some(host))?;
    if selected_registry
        .is_some_and(|name| !repositories.enabled().any(|registry| registry.name == name))
    {
        return Err(ui::i18n!("store-runtime-unavailable"));
    }
    let mut selected = None;
    for registry in repositories
        .enabled()
        .filter(|registry| selected_registry.is_none_or(|name| registry.name == name))
    {
        let offer = host.runtime_offer(&registry.name, runtime)?;
        let Some(offer) = serde_json::from_str::<Option<app_cli::CatalogueApp>>(&offer)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if offer.runtime.as_deref() != Some(runtime)
            || offer.registry != registry.name
            || offer.trust != "publisher-signed"
            || !registry_types::is_runtime_package_id(&offer.package_id)
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
            return Err(ui::i18n!("store-runtime-offer-mismatch"));
        }
        if selected.is_some() {
            return Err(ui::i18n!("store-runtime-offer-ambiguous"));
        }
        selected = Some(runtime_item(offer));
    }
    selected.ok_or_else(|| ui::i18n!("store-runtime-unavailable"))
}

fn runtime_item(offer: app_cli::CatalogueApp) -> StoreItem {
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
            size_bytes: offer.tapp_size_bytes,
            runtime: offer.runtime,
        }],
        publisher_pubkey_hex: Some(offer.publisher_pubkey_hex),
        installed: crate::model::InstalledState::Not,
    }
}

/// Carry out `request`: whether the UI is still there, and how it ended
/// for whoever waits on it.
#[allow(
    clippy::too_many_lines,
    reason = "the serialized worker owns one exhaustive request dispatch; splitting it would obscure that every blocking Store effect crosses this queue"
)]
fn handle(
    request: Request,
    state: &mut State,
    snapshots: &Snapshots,
    staging: &std::path::Path,
) -> (bool, Option<Outcome>) {
    match request {
        Request::ReviewRuntime(target) => {
            let result =
                crate::runtime_recovery::inspect(state.host.as_ref(), &target).map(|review| {
                    Finished {
                        runtime_review: Some(review),
                        ..Finished::default()
                    }
                });
            (
                send(
                    snapshots,
                    state,
                    state.base_items.clone(),
                    &ui::i18n!("store-runtime-repair-reviewed"),
                    None,
                    None,
                ),
                Some(result),
            )
        }
        Request::AcquireRuntime(review) => {
            let progress_snapshots = snapshots.clone();
            let mut progress_snapshot = Snapshot {
                items: state.base_items.clone(),
                status_line: String::new(),
                query: None,
                detail: None,
                remotes: state.remotes.clone(),
                flatpak_available: state.flatpak_available,
                flatpak_error: state.flatpak_error.clone(),
                operation_error: None,
                unanswered_repositories: state.unanswered_repositories.clone(),
                blocks: state.blocks.clone(),
                updating: None,
            };
            let mut last_report = None::<std::time::Instant>;
            let result = state
                .host
                .as_ref()
                .ok_or_else(|| ui::i18n!("store-runtime-repair-unavailable"))
                .and_then(|host| host.acquire_optional_runtime_reporting(&review, move |progress| {
                    // The host coalesces notifications; the UI additionally limits
                    // expensive catalogue snapshots to four per second.
                    if last_report.is_some_and(|last| last.elapsed() < Duration::from_millis(250))
                        && !progress.installing
                        && progress.total != Some(progress.received)
                    { return; }
                    last_report = Some(std::time::Instant::now());
                    progress_snapshot.status_line = if progress.installing {
                        ui::i18n!("store-runtime-installing")
                    } else if let Some(total) = progress.total {
                        ui::i18n!("store-runtime-downloaded", "received" => progress.received.to_string(), "total" => total.to_string())
                    } else {
                        ui::i18n!("store-runtime-downloaded-unknown", "received" => progress.received.to_string())
                    };
                    let _ = progress_snapshots.progress(progress_snapshot.clone());
                }))
                .map(|()| Finished::status(ui::i18n!("store-runtime-complete")));
            (
                send(
                    snapshots,
                    state,
                    state.base_items.clone(),
                    &ui::i18n!("store-runtime-settled"),
                    None,
                    None,
                ),
                Some(result),
            )
        }
        Request::Refresh => {
            let error = ui::i18n!("store-refresh-in-progress");
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
            let opened = runtime_details(&runtime, registry.as_deref(), state);
            let (label, detail, error) = match &opened {
                Ok(item) => (
                    ui::i18n::I18nManager::format(
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
                        let host = state.host.as_ref().ok_or(catalogue_error)?;
                        crate::package_import::review_local(bytes, host)
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
                    (ui::i18n!("store-package-ready"), Some(detail), None)
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
                .ok_or_else(|| ui::i18n!("store-package-review-required"))
                .and_then(|package| {
                    // Recheck immutable handed bytes against the latest independent offers.
                    let host = state
                        .host
                        .as_ref()
                        .ok_or_else(|| ui::i18n!("store-package-unavailable"))?;
                    if let Some(review) = &package.local {
                        host.install_local(&package.bytes, review)?;
                    } else {
                        let current = crate::package_import::review(
                            package.bytes.to_vec(),
                            &state.base_items,
                        )?;
                        host.install_offer(&current.registry, &current.expected)?;
                    }
                    Ok(Finished::status(ui::i18n!("store-package-installed")))
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
        Request::RemoteAdd { name, url } => {
            let result = state.host.as_ref().map_or_else(
                || Err("App service unavailable.".to_string()),
                |host| host.remote(true, &name, Some(&url)),
            );
            if result.is_ok() {
                refresh(state);
            } else {
                refresh_flatpak(state);
            }
            remote_result(snapshots, state, result)
        }
        Request::RemoteSetEnabled { name, enabled } => {
            let result = state.host.as_ref().map_or_else(
                || Err("App service unavailable.".to_string()),
                |host| host.set_remote_enabled(&name, enabled),
            );
            if result.is_ok() {
                refresh(state);
            } else {
                refresh_flatpak(state);
            }
            remote_result(snapshots, state, result)
        }
        Request::RemoteRemove(name) => {
            let result = state.host.as_ref().map_or_else(
                || Err("App service unavailable.".to_string()),
                |host| host.remote(false, &name, None),
            );
            if result.is_ok() {
                refresh(state);
            } else {
                refresh_flatpak(state);
            }
            remote_result(snapshots, state, result)
        }
        Request::UpdateAll(items) => update_all(&items, state, snapshots, staging),
        Request::Act(item, action) => {
            let outcome =
                crate::actions::execute_with_host(state.host.as_ref(), &item, action, staging);
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
                    runtime_review: None,
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
            let registries = crate::config::repositories(state.host.as_ref()).unwrap_or_default();
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
                    runtime_review: None,
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
    // A prior detail lookup may describe another repository or branch.
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
            let host = state
                .host
                .as_ref()
                .ok_or_else(|| "App service unavailable.".to_string())?;
            let rows = host
                .flatpak_catalogue(&remote.name, remote.user_managed)
                .map_err(|error| crate::flathub_client::remote_error(remote, &error))?;
            let mut items = crate::sources::remote_catalogue_items(&rows, remote, &state.installed);
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
            // Browsing may discover more apps than the first catalogue page.
            // Retain them on the same authoritative action/search surface.
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
            let label = ui::i18n::I18nManager::format(
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
            let label = ui::i18n::I18nManager::format(
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

/// Update every item in turn without opening any, report each step, and
/// answer with what happened.
fn update_all(
    items: &[StoreItem],
    state: &mut State,
    snapshots: &Snapshots,
    staging: &std::path::Path,
) -> (bool, Option<Outcome>) {
    let total = items.len();
    let mut failures = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let progress = Snapshot {
            items: state.base_items.clone(),
            status_line: ui::i18n::I18nManager::format(
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
            blocks: state.blocks.clone(),
            updating: Some(item.id.clone()),
        };
        if !snapshots.progress(progress) {
            return (false, None);
        }
        let action = match item.source {
            StoreSource::Flathub | StoreSource::FlatpakRemote { .. } => ActionKind::FlatpakUpdate,
            StoreSource::Tilo { .. } => ActionKind::Update,
        };
        let outcome = crate::actions::execute_with_host(state.host.as_ref(), item, action, staging);
        if !outcome.ok {
            failures.push(format!("{}: {}", item.name, outcome.status_line));
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

fn update_all_summary(total: usize, failures: &[String]) -> String {
    use ui::i18n::{ArgValue, I18nManager};
    let count = |n: usize| ArgValue::Number(f64::from(u32::try_from(n).unwrap_or(u32::MAX)));
    match failures {
        [] => I18nManager::format("store-update-all-done", &[("total", count(total))]),
        [first, rest @ ..] => I18nManager::format(
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
        blocks: state.blocks.clone(),
        updating: None,
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
    let result = state
        .host
        .as_ref()
        .ok_or_else(|| "App service unavailable.".to_string())
        .and_then(app_host::client::BlockingClient::flatpak_status);
    match result {
        Ok(status) => {
            state.flatpak_error = None;
            state.flatpak_available = status.available;
            state.installed = installed_flatpaks(&status.installed);
            state.installed_origins = crate::flathub_client::installed_origins(&status.installed);
            state.remotes =
                crate::flathub_client::refreshed_remotes(&status.remotes, &state.remotes);
        }
        Err(error) => {
            tracing::error!(%error, "could not observe host Flatpak state");
            state.flatpak_available = false;
            state.flatpak_error = Some(format!("Could not read Flatpak on this device: {error}"));
        }
    }
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
        crate::resource::STORE_CATALOG.ensure();
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
            host: None,
            opened_ref: None,
            imported_package: None,
            base_items: items,
            installed: Vec::new(),
            installed_origins: std::collections::HashMap::new(),
            remotes: Vec::new(),
            flatpak_available: false,
            flatpak_error: None,
            catalogue_error: None,
            unanswered_repositories: Vec::new(),
            details: LruCache::new(CACHE_CAP),
            directory,
            blocks: app_catalogue::BlockList::empty(),
            registries: Vec::new(),
            flathub: None,
            force_refresh: false,
        }
    }

    /// Follow a worker detail response through the item used for installation.
    pub(super) fn detail_installation_item(state: &mut State, id: &str) -> StoreItem {
        let (sender, received) = mpsc::channel();
        let snapshots = Snapshots {
            sender,
            wake: WakeHandle::new(),
            pending: Arc::new(AtomicUsize::new(0)),
        };
        let staging = tempfile::tempdir().unwrap();
        assert!(
            handle(
                Request::Detail(id.into()),
                state,
                &snapshots,
                staging.path(),
            )
            .0
        );
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
        let mut app = crate::StoreApp::new();
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
                directory.path(),
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

    struct ImportOfferHost {
        registry: String,
        expected: app_host::install::InstallExpectation,
        installs: Arc<AtomicUsize>,
        offer: Option<app_cli::CatalogueApp>,
    }

    impl app_host::app_host_capnp::app_host::Server for ImportOfferHost {
        fn repositories(
            self: capnp::capability::Rc<Self>,
            _: app_host::app_host_capnp::app_host::RepositoriesParams,
            mut results: app_host::app_host_capnp::app_host::RepositoriesResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().set_json(
                serde_json::to_string(&crate::config::RegistriesFile {
                    registries: vec![crate::config::RegistryConfig {
                        name: self.registry.clone(),
                        base_url: "https://example.invalid".into(),
                        owner_pubkey_hex: "55".repeat(32),
                        enabled: true,
                        channel: app_cli::ReleaseChannel::Stable,
                    }],
                })
                .unwrap(),
            );
            std::future::ready(Ok(()))
        }
        fn runtime_offer(
            self: capnp::capability::Rc<Self>,
            params: app_host::app_host_capnp::app_host::RuntimeOfferParams,
            mut results: app_host::app_host_capnp::app_host::RuntimeOfferResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let params = params.get().unwrap();
            assert_eq!(
                params.get_registry().unwrap().to_str().unwrap(),
                self.registry
            );
            assert_eq!(
                params.get_runtime().unwrap().to_str().unwrap(),
                "tilo-runtime/2026.10.4-chromium"
            );
            results
                .get()
                .set_offer(serde_json::to_string(&self.offer).unwrap());
            std::future::ready(Ok(()))
        }
        fn install_offer(
            self: capnp::capability::Rc<Self>,
            params: app_host::app_host_capnp::app_host::InstallOfferParams,
            mut results: app_host::app_host_capnp::app_host::InstallOfferResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let params = params.get().unwrap();
            assert_eq!(
                params.get_registry().unwrap().to_str().unwrap(),
                self.registry
            );
            assert_eq!(
                params.get_app_id().unwrap().to_str().unwrap(),
                self.expected.app_id
            );
            assert_eq!(
                params.get_version().unwrap().to_str().unwrap(),
                self.expected.version
            );
            assert_eq!(
                params.get_expected_root().unwrap(),
                self.expected.package_root.as_bytes()
            );
            assert_eq!(
                params.get_expected_publisher().unwrap(),
                self.expected.publisher
            );
            assert_eq!(params.get_expected_size_bytes(), self.expected.size_bytes);
            self.installs.fetch_add(1, Ordering::SeqCst);
            results.get().set_id(self.expected.package_root.as_bytes());
            std::future::ready(Ok(()))
        }
    }

    #[test]
    fn runtime_review_capability_preserves_exact_offer_and_installs_without_launch() {
        let offer: app_cli::CatalogueApp = serde_json::from_value(serde_json::json!({
            "package_id":"so.tilo.runtime.chromium", "display_name":"Chromium", "version":"4.0.0",
            "registry":"main", "download_url":"https://example.invalid/engine.tapp",
            "tapp_root_hash_hex":"11".repeat(32), "tapp_size_bytes":6656,
            "publisher_pubkey_hex":"22".repeat(32), "runtime":"tilo-runtime/2026.10.4-chromium",
            "trust":"publisher-signed"
        }))
        .unwrap();
        let installs = Arc::new(AtomicUsize::new(0));
        let expected = app_host::install::InstallExpectation {
            package_root: format!("tapp:{}", offer.tapp_root_hash_hex)
                .parse()
                .unwrap(),
            publisher: [0x22; 32],
            app_id: offer.package_id.clone(),
            version: offer.version.clone(),
            size_bytes: offer.tapp_size_bytes,
        };
        let (service, connection) = app_host::testing::ConnectedService::start(ImportOfferHost {
            registry: "main".into(),
            expected,
            installs: installs.clone(),
            offer: Some(offer),
        })
        .unwrap();
        let mut state = state_with_items(Vec::new(), StoreDirectory::default());
        state.host = Some(app_host::client::BlockingClient::from_connected_fd(connection).unwrap());
        let (sender, received) = mpsc::channel();
        let snapshots = Snapshots {
            sender,
            wake: WakeHandle::new(),
            pending: Arc::new(AtomicUsize::new(0)),
        };
        let dir = tempfile::tempdir().unwrap();
        let (_, result) = handle(
            Request::RuntimeDetails {
                runtime: "tilo-runtime/2026.10.4-chromium".into(),
                registry: None,
            },
            &mut state,
            &snapshots,
            dir.path(),
        );
        assert!(result.unwrap().is_ok());
        assert_eq!(
            installs.load(Ordering::SeqCst),
            0,
            "review installs nothing"
        );
        let DetailUpdate::OpenedReference(item) = received.recv().unwrap().detail.unwrap() else {
            panic!("runtime details missing")
        };
        assert_eq!(
            item.versions[0].runtime.as_deref(),
            Some("tilo-runtime/2026.10.4-chromium")
        );
        assert_eq!(item.action_kind(true), ActionKind::RuntimeInstall);
        let outcome = crate::actions::execute_with_host(
            state.host.as_ref(),
            &item,
            ActionKind::RuntimeInstall,
            dir.path(),
        );
        assert!(outcome.ok);
        assert!(
            outcome.launch.is_none(),
            "a runtime must never launch as an application"
        );
        assert_eq!(installs.load(Ordering::SeqCst), 1);
        drop(state);
        service.shutdown().unwrap();
    }

    #[test]
    fn runtime_review_capability_refuses_missing_foreign_or_malformed_offers() {
        let valid: app_cli::CatalogueApp = serde_json::from_value(serde_json::json!({
            "package_id":"so.tilo.runtime.chromium", "display_name":"Chromium", "version":"4.0.0",
            "registry":"main", "download_url":"https://example.invalid/engine.tapp",
            "tapp_root_hash_hex":"11".repeat(32), "tapp_size_bytes":6656,
            "publisher_pubkey_hex":"22".repeat(32), "runtime":"tilo-runtime/2026.10.4-chromium",
            "trust":"publisher-signed"
        }))
        .unwrap();
        let mut wrong_runtime = valid.clone();
        wrong_runtime.runtime = Some("tilo-runtime/2026.10.5-chromium".into());
        let mut untrusted = valid.clone();
        untrusted.trust = "untrusted".into();
        let mut wrong_registry = valid.clone();
        wrong_registry.registry = "foreign".into();
        let mut zero_size = valid.clone();
        zero_size.tapp_size_bytes = 0;
        let mut malformed_root = valid.clone();
        malformed_root.tapp_root_hash_hex = "xx".repeat(32);
        let mut malformed_publisher = valid;
        malformed_publisher.publisher_pubkey_hex = "x".into();
        for offer in [
            None,
            Some(wrong_runtime),
            Some(untrusted),
            Some(wrong_registry),
            Some(zero_size),
            Some(malformed_root),
            Some(malformed_publisher),
        ] {
            let installs = Arc::new(AtomicUsize::new(0));
            let (service, connection) =
                app_host::testing::ConnectedService::start(ImportOfferHost {
                    registry: "main".into(),
                    expected: app_host::install::InstallExpectation {
                        package_root: format!("tapp:{}", "11".repeat(32)).parse().unwrap(),
                        publisher: [0x22; 32],
                        app_id: "so.tilo.runtime.chromium".into(),
                        version: "4.0.0".into(),
                        size_bytes: 6656,
                    },
                    installs: installs.clone(),
                    offer,
                })
                .unwrap();
            let mut state = state_with_items(Vec::new(), StoreDirectory::default());
            state.host =
                Some(app_host::client::BlockingClient::from_connected_fd(connection).unwrap());
            assert!(runtime_details("tilo-runtime/2026.10.4-chromium", None, &state).is_err());
            assert_eq!(installs.load(Ordering::SeqCst), 0);
            drop(state);
            service.shutdown().unwrap();
        }
    }

    #[test]
    fn package_import_worker_reviews_before_exact_owned_offer_install() {
        check_import_worker(crate::package_import::tests::signed_fixture());
    }

    #[test]
    fn package_import_offered_worker_preserves_verified_offer_install_route() {
        check_import_worker(crate::package_import::tests::catalogue_fixture());
    }

    fn check_import_worker((bytes, item): (Vec<u8>, StoreItem)) {
        use super::*;
        let reviewed =
            crate::package_import::review(bytes.clone(), std::slice::from_ref(&item)).unwrap();
        let installs = Arc::new(AtomicUsize::new(0));
        let (service, connection) = app_host::testing::ConnectedService::start(ImportOfferHost {
            registry: reviewed.registry.clone(),
            expected: reviewed.expected.clone(),
            installs: installs.clone(),
            offer: None,
        })
        .unwrap();
        let mut state = state_with_items(vec![item.clone()], StoreDirectory::default());
        state.host = Some(app_host::client::BlockingClient::from_connected_fd(connection).unwrap());
        let (sender, received) = mpsc::channel();
        let snapshots = Snapshots {
            sender,
            wake: WakeHandle::new(),
            pending: Arc::new(AtomicUsize::new(0)),
        };
        let dir = tempfile::tempdir().unwrap();
        let (alive, outcome) = handle(
            Request::ImportPackage(crate::package_import::Input::Bytes(bytes.clone())),
            &mut state,
            &snapshots,
            dir.path(),
        );
        assert!(alive && outcome.unwrap().is_ok());
        assert_eq!(installs.load(Ordering::SeqCst), 0, "opening never installs");
        assert!(
            matches!(received.recv().unwrap().detail, Some(DetailUpdate::ImportedPackage { item: shown, .. }) if shown.id == item.id)
        );
        let (_, outcome) = handle(
            Request::InstallImported {
                app: item.id.clone(),
                root: "00".repeat(32),
            },
            &mut state,
            &snapshots,
            dir.path(),
        );
        assert!(outcome.unwrap().is_err());
        assert_eq!(
            installs.load(Ordering::SeqCst),
            0,
            "stale reviewed identity cannot install"
        );
        let (_, outcome) = handle(
            Request::InstallImported {
                app: item.id.clone(),
                root: reviewed.expected.package_root.to_hex(),
            },
            &mut state,
            &snapshots,
            dir.path(),
        );
        assert!(outcome.unwrap().is_ok());
        assert_eq!(installs.load(Ordering::SeqCst), 1);
        assert!(state.imported_package.is_none() && state.refresh_requested);
        // A later source change revokes the previously reviewed file.
        handle(
            Request::ImportPackage(crate::package_import::Input::Bytes(bytes)),
            &mut state,
            &snapshots,
            dir.path(),
        );
        state.base_items.clear();
        let (_, outcome) = handle(
            Request::InstallImported {
                app: item.id,
                root: reviewed.expected.package_root.to_hex(),
            },
            &mut state,
            &snapshots,
            dir.path(),
        );
        assert!(outcome.unwrap().is_err());
        assert_eq!(installs.load(Ordering::SeqCst), 1);
        drop(state);
        service.shutdown().unwrap();
    }

    #[test]
    fn flatpak_observation_failure_reaches_worker_snapshot_separately_from_missing() {
        let mut state = state_with_items(Vec::new(), StoreDirectory::default());
        state.flatpak_available = true;
        refresh_flatpak(&mut state);
        assert!(!state.flatpak_available);
        assert!(
            state
                .flatpak_error
                .as_deref()
                .unwrap()
                .contains("App service unavailable")
        );
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

    /// LRU eviction, non-blocking requests and the authoritative action directory.
    #[test]
    fn worker_cache_is_non_blocking_and_keeps_the_authoritative_directory() {
        // Case `request_and_drain_are_non_blocking`.
        {
            let worker = StoreWorker::spawn(StoreDirectory::default(), WakeHandle::new());
            let started = ui::time::now();
            assert!(worker.request(Request::Detail("editor".into())));
            assert!(started.elapsed() < Duration::from_millis(50));
            let started = ui::time::now();
            let _ = worker.try_snapshot();
            assert!(started.elapsed() < Duration::from_millis(20));
        }

        // Case `filtered_snapshot_never_truncates_authoritative_action_directory`.
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

        // Case `authoritative_catalogue_replacement_updates_and_removes_directory_rows`.
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
