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

use getrandom::{SysRng, rand_core::TryRng as _};
use ui::WakeHandle;

use crate::app::ReportEnvelope;
use crate::config::{RegistriesFile, RegistryConfig};
use crate::flathub_client::{Remote, installed_flatpaks};
use crate::model::{ActionKind, StoreSource};
use crate::search::StoreDirectory;
use crate::sources::{
    flathub_record_to_item, installed_flatpak_to_item, installed_tilo_to_store_item, merge,
    tilo_to_store_item_on_channel,
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
    Refresh,
    Search(String),
    BrowseRemote(String),
    Detail(String),
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
    SubmitReport {
        registry_name: String,
        envelope: ReportEnvelope,
    },
    RefreshModeration {
        registry_name: String,
        case_id: String,
    },
    AppealModeration {
        registry_name: String,
        case_id: String,
        reason: String,
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
    pub case: Option<registry_types::ModerationCase>,
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
    searches: LruCache<String, Vec<StoreItem>>,
    details: LruCache<String, StoreItem>,
    /// The current palette/action directory. Rebuilding it is linear in a
    /// catalogue snapshot, so it belongs on this serialized worker rather
    /// than in the UI's snapshot-drain loop.
    directory: StoreDirectory,
    /// The catalogue's names and descriptions, indexed while they stay the
    /// same.
    item_index: ui::search::ListIndex,
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
        base_items: Vec::new(),
        installed: Vec::new(),
        installed_origins: std::collections::HashMap::new(),
        remotes: Vec::new(),
        flatpak_available: false,
        flatpak_error: None,
        catalogue_error: None,
        searches: LruCache::new(CACHE_CAP),
        details: LruCache::new(CACHE_CAP),
        directory,
        item_index: ui::search::ListIndex::default(),
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
                    refresh = Some(refresh::Refresh::start(&state, done));
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
        })
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
        Request::Search(query) => (search(&query, state, snapshots), None),
        Request::Detail(id) => (detail(&id, state, snapshots), None),
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
            refresh(state);
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
        Request::SubmitReport {
            registry_name,
            envelope,
        } => {
            let registries = crate::config::repositories(state.host.as_ref()).unwrap_or_default();
            let outcome = match submit_report(&registries, &registry_name, &envelope) {
                Ok(case) => Ok(Finished {
                    status: format!(
                        "Report submitted as {}. Status will appear after the registry owner publishes a signed transparency snapshot.",
                        case.case_id
                    ),
                    case: Some(case),
                    launch: None,
                }),
                Err(error) => Err(format!(
                    "Report was not submitted. {error} The collaborative draft is unchanged; check your identity or connection and retry."
                )),
            };
            (
                moderation_snapshot(snapshots, state, &outcome),
                Some(outcome),
            )
        }
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
        Request::AppealModeration {
            registry_name,
            case_id,
            reason,
        } => {
            let registries = crate::config::repositories(state.host.as_ref()).unwrap_or_default();
            let outcome = moderation_registry(&registries, &registry_name)
                .and_then(|registry| {
                    let now = ui::time::unix_now();
                    let mut nonce = [0_u8; 16];
                    SysRng.try_fill_bytes(&mut nonce).map_err(|error| {
                        crate::moderation_client::ModerationClientError::Identity(
                            error.to_string(),
                        )
                    })?;
                    let request = crate::moderation_client::sign_with_runtime_identity(
                        crate::moderation_client::report_request_id(&case_id, &reason, now, &nonce),
                        now,
                        registry_types::ModerationActorRole::Publisher,
                        registry_types::ModerationMutation::PublisherAppeal {
                            case_id,
                            reason,
                            evidence_digest_hex: None,
                        },
                    )?;
                    crate::moderation_client::submit(&registry.base_url, &request)
                })
                .map(|case| {
                    Finished::status(format!(
                        "Appeal submitted for {}. The last verified status stays visible until an owner-signed transparency snapshot includes it.",
                        case.case_id
                    ))
                })
                .map_err(|error| {
                    format!(
                        "Registry request failed: {error} The collaborative draft and last verified status are unchanged; retry when ready."
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
            state.searches.clear();
            state.details.clear();
            sync_authoritative_catalogue(state);
            let label = ui::i18n::I18nManager::format(
                "store-remote-results",
                &[
                    ("name", ui::i18n::ArgValue::Text(title)),
                    ("count", ui::i18n::ArgValue::Text(items.len().to_string())),
                ],
            );
            let alive = send(snapshots, state, items, &label, Some(String::new()), None);
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

fn search(query: &str, state: &mut State, snapshots: &Snapshots) -> bool {
    let query = query.trim().to_string();
    if query.is_empty() {
        return send(
            snapshots,
            state,
            state.base_items.clone(),
            "Search cleared.",
            Some(query),
            None,
        );
    }
    let mut operation_error = None;
    let items = if let Some(cached) = state.searches.get(&query).cloned() {
        cached
    } else {
        let candidates: Vec<&StoreItem> = state.base_items.iter().collect();
        let ranks = crate::model::relevance(&state.item_index, &query, &candidates);
        let tilo: Vec<StoreItem> = candidates
            .into_iter()
            .zip(ranks)
            .filter_map(|(item, rank)| rank.map(|_| item.clone()))
            .collect();
        let records = match crate::flathub_client::search(&query) {
            Ok(records) => records,
            Err(error) => {
                operation_error = Some(format!("Could not search Flathub: {error}"));
                Vec::new()
            }
        };
        let flatpak: Vec<StoreItem> = records
            .iter()
            .map(|record| flathub_record_to_item(record, &state.installed))
            .collect();
        let native_names: std::collections::HashSet<_> = tilo
            .iter()
            .filter(|item| matches!(item.source, StoreSource::Tilo { .. }))
            .map(|item| item.name.trim().to_ascii_lowercase())
            .collect();
        let mut items = tilo;
        for mut item in flatpak {
            crate::sources::apply_flatpak_origin(&mut item, &state.installed_origins);
            if !items.iter().any(|existing| existing.id == item.id)
                && (item.installed.is_installed()
                    || !native_names.contains(&item.name.trim().to_ascii_lowercase()))
            {
                items.push(item);
            }
        }
        if operation_error.is_none() {
            state.searches.put(query.clone(), items.clone());
        }
        items
    };
    let label = format!("{} results for “{}”.", items.len(), query);
    send_tracked(
        snapshots,
        state,
        items,
        &label,
        Some(query),
        None,
        operation_error,
    )
}

fn detail(id: &str, state: &mut State, snapshots: &Snapshots) -> bool {
    let mut operation_error = None;
    let loaded = if let Some(cached) = state.details.get(id).cloned() {
        Some(cached)
    } else if let Some(item) = state
        .base_items
        .iter()
        .find(|item| {
            item.id == id
                && matches!(
                    item.source,
                    StoreSource::Tilo { .. } | StoreSource::FlatpakRemote { .. }
                )
        })
        .cloned()
    {
        state.details.put(id.to_string(), item.clone());
        Some(item)
    } else {
        let loaded = match crate::flathub_client::fetch_appstream(id) {
            Ok(record) => {
                let mut item = flathub_record_to_item(&record, &state.installed);
                crate::sources::apply_flatpak_origin(&mut item, &state.installed_origins);
                Some(item)
            }
            Err(error) => {
                operation_error = Some(format!("Could not load app details: {error}"));
                state.base_items.iter().find(|item| item.id == id).cloned()
            }
        };
        if let Some(item) = loaded.as_ref()
            && operation_error.is_none()
        {
            state.details.put(id.to_string(), item.clone());
        }
        loaded
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
        operation_error,
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
            status_line: format!(
                "Updating {} of {total}: {}…",
                index.saturating_add(1),
                item.name
            ),
            query: None,
            detail: None,
            remotes: state.remotes.clone(),
            flatpak_available: state.flatpak_available,
            flatpak_error: state.flatpak_error.clone(),
            operation_error: None,
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
    refresh(state);
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
    let apps = if total == 1 { "app" } else { "apps" };
    match failures {
        [] => format!("Updated {total} {apps}."),
        [first, rest @ ..] => {
            let updated = total.saturating_sub(failures.len());
            let more = if rest.is_empty() {
                String::new()
            } else {
                format!(" (and {} more failed)", rest.len())
            };
            format!("Updated {updated} of {total} {apps}; {first}{more}")
        }
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
    })
}

fn submit_report(
    registries: &RegistriesFile,
    registry_name: &str,
    envelope: &ReportEnvelope,
) -> Result<registry_types::ModerationCase, crate::moderation_client::ModerationClientError> {
    let registry = moderation_registry(registries, registry_name)?;
    let now = ui::time::unix_now();
    let envelope_bytes = serde_json::to_vec(envelope)
        .map_err(|_| crate::moderation_client::ModerationClientError::InvalidResponse)?;
    let mut nonce = [0_u8; 16];
    SysRng.try_fill_bytes(&mut nonce).map_err(|error| {
        crate::moderation_client::ModerationClientError::Identity(error.to_string())
    })?;
    let request = crate::moderation_client::sign_with_runtime_identity(
        crate::moderation_client::report_request_id(
            &envelope.app_id,
            &envelope.evidence_note,
            now,
            &nonce,
        ),
        now,
        registry_types::ModerationActorRole::Reporter,
        registry_types::ModerationMutation::FileReport {
            package_id: envelope.app_id.clone(),
            category: "policy".into(),
            summary: envelope.evidence_note.chars().take(4_096).collect(),
            evidence_digest_hex: blake3::hash(&envelope_bytes).to_hex().to_string(),
        },
    )?;
    crate::moderation_client::submit(&registry.base_url, &request)
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
            state.remotes = crate::flathub_client::list_remotes(&status.remotes);
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
}

#[cfg(test)]
mod tests {
    #[test]
    fn update_all_reports_what_it_updated_and_the_first_failure() {
        use super::update_all_summary;
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
            searches: LruCache::new(CACHE_CAP),
            details: LruCache::new(CACHE_CAP),
            directory,
            item_index: ui::search::ListIndex::default(),
        }
    }

    struct ImportOfferHost {
        registry: String,
        expected: app_host::install::InstallExpectation,
        installs: Arc<AtomicUsize>,
    }

    impl app_host::app_host_capnp::app_host::Server for ImportOfferHost {
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
    fn package_import_worker_reviews_before_exact_owned_offer_install() {
        check_import_worker(crate::package_import::tests::signed_fixture());
    }

    #[test]
    fn package_import_published_worker_preserves_verified_offer_install_route() {
        check_import_worker(crate::package_import::tests::published_fixture());
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
            assert!(worker.request(Request::Search("editor".into())));
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
