//! Serialized package/catalog actor. All blocking package and network work
//! crosses this one queue; the UI only performs non-blocking snapshot drains.

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
use crate::config::{RegistryConfig, load_registries};
use crate::flathub_client::{Remote, installed_flatpaks};
use crate::model::{ActionKind, StoreSource};
use crate::search_provider::StoreSearchIndex;
use crate::sources::{
    flathub_placeholder_to_item, flathub_record_to_item, installed_flatpak_to_item,
    installed_tilo_to_store_item, merge, tilo_to_store_item,
};
use crate::{FlatpakRef, StoreItem};

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
    Detail(String),
    /// Show the app a `.flatpakref` describes, ready to install: the file at
    /// `path`, or the reference's `contents` handed over without one.
    OpenReference {
        path: Option<PathBuf>,
        contents: Option<String>,
    },
    RefreshRemotes,
    RemoteAdd {
        name: String,
        url: String,
    },
    RemoteRemove(String),
    /// Boxed: [`StoreItem`] is far larger than every other variant's
    /// payload, and this is a queued message — boxing keeps every
    /// `Request` (and the channel that carries them) sized to the common
    /// case instead of the rare, heavy one.
    Act(Box<StoreItem>, ActionKind),
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
}

impl Finished {
    fn status(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            case: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub items: Vec<StoreItem>,
    pub status_line: String,
    /// Search responses name their query so stale results can be discarded.
    pub query: Option<String>,
    pub detail: Option<StoreItem>,
    pub remotes: Vec<Remote>,
    pub operation_error: Option<String>,
}

struct QueuedRequest {
    request: Request,
    done: Option<Done>,
}

pub struct StoreWorker {
    requests: mpsc::Sender<QueuedRequest>,
    snapshots: mpsc::Receiver<Snapshot>,
    pending: Arc<AtomicUsize>,
}

impl StoreWorker {
    /// Start the worker; `wake` is woken whenever a snapshot is ready.
    ///
    /// # Panics
    /// Panics if the OS refuses to spawn a thread (extreme resource
    /// exhaustion): a live Store without its worker has nothing to show.
    #[allow(
        clippy::expect_used,
        reason = "thread spawn failure means the OS is out of resources; the surface attaching the worker has no fallible path to report it on"
    )]
    pub fn spawn(search_index: StoreSearchIndex, wake: WakeHandle) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(1));
        std::thread::Builder::new()
            .name("store-worker".into())
            .spawn(move || {
                let snapshots = Snapshots {
                    sender: snapshot_tx,
                    wake,
                };
                run(&request_rx, &snapshots, search_index);
            })
            .expect("spawn store worker");
        Self {
            requests: request_tx,
            snapshots: snapshot_rx,
            pending,
        }
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
        if self.requests.send(QueuedRequest { request, done }).is_ok() {
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
}

impl Snapshots {
    fn send(&self, snapshot: Snapshot) -> bool {
        let sent = self.sender.send(snapshot).is_ok();
        self.wake.wake();
        sent
    }
}

struct State {
    host: Option<app_host::client::BlockingClient>,
    opened_ref: Option<FlatpakRef>,
    base_items: Vec<StoreItem>,
    installed: Vec<(String, String)>,
    remotes: Vec<Remote>,
    searches: LruCache<String, Vec<StoreItem>>,
    details: LruCache<String, StoreItem>,
    /// The current palette/action directory. Rebuilding it is linear in a
    /// catalogue snapshot, so it belongs on this serialized worker rather
    /// than in the UI's snapshot-drain loop.
    search_index: StoreSearchIndex,
    /// The catalogue's names and descriptions, indexed while they stay the
    /// same.
    item_index: ui::search::ListIndex,
}

fn run(
    requests: &mpsc::Receiver<QueuedRequest>,
    snapshots: &Snapshots,
    search_index: StoreSearchIndex,
) {
    let staging = crate::actions::tapp_staging_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("tilo-store-staged"));
    let _ = std::fs::create_dir_all(&staging);
    let mut state = State {
        host: app_host::client::BlockingClient::from_service()
            .map_err(|error| {
                tracing::error!(%error, "required app-host capability unavailable");
                error
            })
            .ok(),
        opened_ref: None,
        base_items: Vec::new(),
        installed: Vec::new(),
        remotes: Vec::new(),
        searches: LruCache::new(CACHE_CAP),
        details: LruCache::new(CACHE_CAP),
        search_index,
        item_index: ui::search::ListIndex::default(),
    };
    refresh(&mut state);
    if !send(
        snapshots,
        &state,
        state.base_items.clone(),
        "Store refreshed.",
        None,
        None,
    ) {
        return;
    }
    loop {
        match requests.recv_timeout(REFRESH_INTERVAL) {
            Ok(QueuedRequest { request, done }) => {
                let (alive, outcome) = handle(request, &mut state, snapshots, &staging);
                if let (Some(done), Some(outcome)) = (done, outcome) {
                    let _ = done.send(outcome);
                }
                if !alive {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                refresh(&mut state);
                if !send(
                    snapshots,
                    &state,
                    state.base_items.clone(),
                    "Store refreshed.",
                    None,
                    None,
                ) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
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
        Request::Refresh => {
            refresh(state);
            let alive = send(
                snapshots,
                state,
                state.base_items.clone(),
                "Store refreshed.",
                None,
                None,
            );
            (alive, Some(Ok(Finished::status("Store refreshed."))))
        }
        Request::Search(query) => (search(&query, state, snapshots), None),
        Request::Detail(id) => (detail(&id, state, snapshots), None),
        Request::OpenReference { path, contents } => {
            let opened = open_reference(path, contents, state);
            let (label, detail, error) = match &opened {
                Ok(item) => (
                    format!("Opened {} from its reference.", item.name),
                    Some(item.clone()),
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
        Request::RefreshRemotes => {
            state.remotes = crate::flathub_client::list_remotes();
            let label = format!("Loaded {} Flatpak remotes.", state.remotes.len());
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
        Request::RemoteAdd { name, url } => {
            let result = state.host.as_ref().map_or_else(
                || Err("App service unavailable.".to_string()),
                |host| host.remote(true, &name, Some(&url)),
            );
            state.remotes = crate::flathub_client::list_remotes();
            remote_result(snapshots, state, result)
        }
        Request::RemoteRemove(name) => {
            let result = state.host.as_ref().map_or_else(
                || Err("App service unavailable.".to_string()),
                |host| host.remote(false, &name, None),
            );
            state.remotes = crate::flathub_client::list_remotes();
            remote_result(snapshots, state, result)
        }
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
                Ok(Finished::status(outcome.status_line))
            } else {
                Err(outcome.status_line)
            };
            (alive, Some(finished))
        }
        Request::SubmitReport {
            registry_name,
            envelope,
        } => {
            let outcome = match submit_report(&registry_name, &envelope) {
                Ok(case) => Ok(Finished {
                    status: format!(
                        "Report submitted as {}. Status will appear after the registry owner publishes a signed transparency snapshot.",
                        case.case_id
                    ),
                    case: Some(case),
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
            let outcome = moderation_registry(&registry_name)
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
            let outcome = moderation_registry(&registry_name)
                .and_then(|registry| {
                    let now = ui::time::wall()
                        .duration_since(ui::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
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
                Some(reference) if crate::doc_actions::valid_app_id(&reference.app_id) => {
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
    let items = if let Some(cached) = state.searches.get(&query).cloned() {
        cached
    } else {
        let candidates: Vec<&StoreItem> = state
            .base_items
            .iter()
            .filter(|item| matches!(item.source, StoreSource::Tilo { .. }))
            .collect();
        let ranks = crate::model::relevance(&state.item_index, &query, &candidates);
        let tilo = candidates
            .into_iter()
            .zip(ranks)
            .filter_map(|(item, rank)| rank.map(|_| item.clone()))
            .collect();
        let flatpak = crate::flathub_client::search(&query)
            .unwrap_or_default()
            .iter()
            .map(|record| flathub_record_to_item(record, &state.installed))
            .collect();
        let items = merge(tilo, flatpak);
        state.searches.put(query.clone(), items.clone());
        items
    };
    let label = format!("{} results for “{}”.", items.len(), query);
    send(snapshots, state, items, &label, Some(query), None)
}

fn detail(id: &str, state: &mut State, snapshots: &Snapshots) -> bool {
    let loaded = if let Some(cached) = state.details.get(id).cloned() {
        Some(cached)
    } else if let Some(item) = state
        .base_items
        .iter()
        .find(|item| item.id == id && matches!(item.source, StoreSource::Tilo { .. }))
        .cloned()
    {
        state.details.put(id.to_string(), item.clone());
        Some(item)
    } else {
        let loaded = crate::flathub_client::fetch_appstream(id)
            .ok()
            .map(|record| flathub_record_to_item(&record, &state.installed))
            .or_else(|| state.base_items.iter().find(|item| item.id == id).cloned());
        if let Some(item) = loaded.as_ref() {
            state.details.put(id.to_string(), item.clone());
        }
        loaded
    };
    let label = loaded.as_ref().map_or_else(
        || format!("Could not load details for {id}."),
        |item| format!("Loaded details for {}.", item.name),
    );
    send(
        snapshots,
        state,
        state.base_items.clone(),
        &label,
        None,
        loaded,
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
    send_tracked(snapshots, state, items, status_line, query, detail, None)
}

fn send_tracked(
    snapshots: &Snapshots,
    state: &State,
    items: Vec<StoreItem>,
    status_line: &str,
    query: Option<String>,
    detail: Option<StoreItem>,
    operation_error: Option<String>,
) -> bool {
    snapshots.send(Snapshot {
        items,
        status_line: status_line.to_string(),
        query,
        detail,
        remotes: state.remotes.clone(),
        operation_error,
    })
}

fn submit_report(
    registry_name: &str,
    envelope: &ReportEnvelope,
) -> Result<registry_types::ModerationCase, crate::moderation_client::ModerationClientError> {
    let registry = moderation_registry(registry_name)?;
    let now = ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
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
    registry_name: &str,
) -> Result<RegistryConfig, crate::moderation_client::ModerationClientError> {
    load_registries()
        .registries
        .into_iter()
        .find(|registry| registry.enabled && registry.name == registry_name)
        .ok_or_else(|| {
            crate::moderation_client::ModerationClientError::Offline(
                "the app's registry is disabled or no longer configured".into(),
            )
        })
}

fn refresh(state: &mut State) {
    state.installed = installed_flatpaks();
    state.remotes = crate::flathub_client::list_remotes();
    state.base_items = collect_items(
        state.opened_ref.as_ref(),
        &state.installed,
        state.host.as_ref(),
    );
    state.searches.clear();
    state.details.clear();
    sync_authoritative_catalogue(state);
}

/// Publish the one authoritative catalogue directory. Presentation snapshots
/// may contain a local filtered search result or a detail projection, neither
/// of which is allowed to remove unrelated apps from Ctrl+K, agents, history,
/// or resource resolution. This executes on the serialized worker directly
/// after a true `base_items` refresh, never on the UI thread.
fn sync_authoritative_catalogue(state: &State) {
    state.search_index.sync(&state.base_items);
}

fn collect_items(
    opened_ref: Option<&FlatpakRef>,
    installed: &[(String, String)],
    host: Option<&app_host::client::BlockingClient>,
) -> Vec<StoreItem> {
    let registries = load_registries().registries;
    let installed_tilo = host.and_then(|host| host.list().ok()).unwrap_or_default();
    let mut tilo_items = Vec::new();
    for registry in registries.iter().filter(|registry| registry.enabled) {
        match crate::registry_client::fetch_entries(registry) {
            Ok(entries) => tilo_items.extend(
                entries
                    .iter()
                    .map(|entry| tilo_to_store_item(entry, &registry.name, &installed_tilo)),
            ),
            Err(error) => tracing::warn!("registry {} refresh failed: {error}", registry.name),
        }
    }
    // The Store's action surface is also the recovery path for software that
    // is already here. Never make installed Tilo apps vanish merely because a
    // configured registry is offline or no longer carries an older package.
    // Catalogue rows win whenever present because they carry the signed
    // download/update metadata; these local rows intentionally cannot update.
    for installed in &installed_tilo {
        if !tilo_items.iter().any(|item| item.id == installed.id) {
            tilo_items.push(installed_tilo_to_store_item(installed));
        }
    }
    let mut flatpak_items = crate::flathub_client::fetch_popular(200)
        .unwrap_or_default()
        .iter()
        .map(|entry| flathub_placeholder_to_item(entry, installed))
        .collect::<Vec<_>>();
    // Popular is a discovery feed, not an installed-app inventory. Preserve
    // every locally reported Flatpak so launch/remove remain possible during
    // an API outage and for apps that were never popular.
    for (app_id, version) in installed {
        if !flatpak_items.iter().any(|item| item.id == *app_id) {
            flatpak_items.push(installed_flatpak_to_item(app_id, version));
        }
    }
    if let Some(reference) = opened_ref
        && !flatpak_items.iter().any(|item| item.id == reference.app_id)
    {
        let version = installed
            .iter()
            .find_map(|(id, version)| (id == &reference.app_id).then(|| version.clone()));
        flatpak_items.insert(0, reference.clone().into_store_item(version));
    }
    tracing::debug!("{}", registries_summary(&registries));
    merge(tilo_items, flatpak_items)
}

fn registries_summary(registries: &[RegistryConfig]) -> String {
    let enabled = registries
        .iter()
        .filter(|registry| registry.enabled)
        .count();
    format!(
        "configured registries: {} total / {} enabled",
        registries.len(),
        enabled
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstalledState, StoreItemVersion, StoreTrust};
    use ui::search::SearchProvider;

    fn item(id: &str, name: &str) -> StoreItem {
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

    fn state_with_items(items: Vec<StoreItem>, search_index: StoreSearchIndex) -> State {
        State {
            host: None,
            opened_ref: None,
            base_items: items,
            installed: Vec::new(),
            remotes: Vec::new(),
            searches: LruCache::new(CACHE_CAP),
            details: LruCache::new(CACHE_CAP),
            search_index,
            item_index: ui::search::ListIndex::default(),
        }
    }

    /// LRU eviction, non-blocking requests and the authoritative action directory.
    #[test]
    fn worker_cache_is_non_blocking_and_keeps_the_authoritative_directory() {
        // Case `request_and_drain_are_non_blocking`.
        {
            let worker = StoreWorker::spawn(StoreSearchIndex::default(), WakeHandle::new());
            let started = ui::time::now();
            assert!(worker.request(Request::Search("editor".into())));
            assert!(started.elapsed() < Duration::from_millis(50));
            let started = ui::time::now();
            let _ = worker.try_snapshot();
            assert!(started.elapsed() < Duration::from_millis(20));
        }

        // Case `filtered_snapshot_never_truncates_authoritative_action_directory`.
        {
            let search_index = StoreSearchIndex::default();
            let state = state_with_items(
                vec![
                    item("dev.example.alpha", "Alpha"),
                    item("dev.example.beta", "Beta"),
                ],
                search_index.clone(),
            );
            sync_authoritative_catalogue(&state);
            let (sender, received) = mpsc::channel();
            let snapshots = Snapshots {
                sender,
                wake: WakeHandle::new(),
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

            let provider = search_index.provider();
            assert_eq!(provider.search_limited("Beta", 10).len(), 1);
        }

        // Case `authoritative_catalogue_replacement_updates_and_removes_directory_rows`.
        {
            let search_index = StoreSearchIndex::default();
            let mut state = state_with_items(
                vec![
                    item("dev.example.alpha", "Alpha"),
                    item("dev.example.beta", "Beta"),
                ],
                search_index.clone(),
            );
            sync_authoritative_catalogue(&state);
            let provider = search_index.provider();
            let cursor = provider
                .search_page("", None, 1, 1)
                .next
                .expect("two directory rows produce a continuation");

            state.base_items = vec![item("dev.example.gamma", "Gamma")];
            sync_authoritative_catalogue(&state);

            assert!(provider.search_limited("Beta", 10).is_empty());
            assert_eq!(provider.search_limited("Gamma", 10).len(), 1);
            assert!(!provider.action_directory_cursor_is_current(&cursor));
        }
    }
}
