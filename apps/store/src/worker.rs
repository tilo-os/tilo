//! Serialized package/catalog actor. All blocking package and network work
//! crosses this one queue; the UI only performs non-blocking snapshot drains.

use std::num::NonZeroUsize;

use lru::LruCache;
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use getrandom::{SysRng, rand_core::TryRng as _};

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

#[derive(Clone, Debug)]
pub enum Request {
    Refresh,
    Search(String),
    Detail(String),
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

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub items: Vec<StoreItem>,
    pub status_line: String,
    /// Search responses name their query so stale results can be discarded.
    pub query: Option<String>,
    pub detail: Option<StoreItem>,
    pub remotes: Vec<Remote>,
    pub invocation: Option<tilo_actions::InvocationId>,
    pub operation_error: Option<String>,
    pub moderation_case: Option<registry_types::ModerationCase>,
}

struct QueuedRequest {
    request: Request,
    invocation: Option<tilo_actions::InvocationId>,
}

pub struct StoreWorker {
    requests: mpsc::Sender<QueuedRequest>,
    snapshots: mpsc::Receiver<Snapshot>,
    pending: Arc<AtomicUsize>,
}

impl StoreWorker {
    /// # Panics
    /// Panics if the OS refuses to spawn a thread (extreme resource
    /// exhaustion). The binary's root widget constructor that calls this is
    /// infallible, so there is no fallible path back through it to report a
    /// failure to instead.
    #[allow(
        clippy::expect_used,
        reason = "thread spawn failure means the OS is out of resources; there is no fallible path back through the root widget constructor to report it, and no live app to hand back without a worker"
    )]
    pub fn spawn(opened_ref: Option<FlatpakRef>, search_index: StoreSearchIndex) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(1));
        std::thread::Builder::new()
            .name("store-worker".into())
            .spawn(move || run(&request_rx, &snapshot_tx, opened_ref, search_index))
            .expect("spawn store worker");
        Self {
            requests: request_tx,
            snapshots: snapshot_rx,
            pending,
        }
    }

    pub fn request(&self, request: Request) -> bool {
        self.request_with_invocation(request, None)
    }

    pub fn request_with_invocation(
        &self,
        request: Request,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> bool {
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self
            .requests
            .send(QueuedRequest {
                request,
                invocation,
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
    snapshots: &mpsc::Sender<Snapshot>,
    opened_ref: Option<FlatpakRef>,
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
        opened_ref,
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
            Ok(request) => {
                if !handle(
                    request.request,
                    request.invocation,
                    &mut state,
                    snapshots,
                    &staging,
                ) {
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

#[allow(
    clippy::too_many_lines,
    reason = "the serialized worker owns one exhaustive request dispatch; splitting it would obscure that every blocking Store effect crosses this queue"
)]
fn handle(
    request: Request,
    invocation: Option<tilo_actions::InvocationId>,
    state: &mut State,
    snapshots: &mpsc::Sender<Snapshot>,
    staging: &std::path::Path,
) -> bool {
    match request {
        Request::Refresh => {
            refresh(state);
            send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                "Store refreshed.",
                None,
                None,
                invocation,
                None,
            )
        }
        Request::Search(query) => search(&query, state, snapshots),
        Request::Detail(id) => detail(&id, state, snapshots),
        Request::RefreshRemotes => {
            state.remotes = crate::flathub_client::list_remotes();
            let label = format!("Loaded {} Flatpak remotes.", state.remotes.len());
            send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
                invocation,
                None,
            )
        }
        Request::RemoteAdd { name, url } => {
            let label = state.host.as_ref().map_or_else(
                || "App service unavailable.".to_string(),
                |host| {
                    host.remote(true, &name, Some(&url))
                        .unwrap_or_else(|error| error)
                },
            );
            state.remotes = crate::flathub_client::list_remotes();
            send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
                invocation,
                None,
            )
        }
        Request::RemoteRemove(name) => {
            let label = state.host.as_ref().map_or_else(
                || "App service unavailable.".to_string(),
                |host| {
                    host.remote(false, &name, None)
                        .unwrap_or_else(|error| error)
                },
            );
            state.remotes = crate::flathub_client::list_remotes();
            send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
                invocation,
                None,
            )
        }
        Request::Act(item, action) => {
            let outcome =
                crate::actions::execute_with_host(state.host.as_ref(), &item, action, staging);
            refresh(state);
            let operation_error = (!outcome.ok).then(|| outcome.status_line.clone());
            send_tracked(
                snapshots,
                state,
                state.base_items.clone(),
                &outcome.status_line,
                None,
                None,
                invocation,
                operation_error,
            )
        }
        Request::SubmitReport {
            registry_name,
            envelope,
        } => {
            let result = submit_report(&registry_name, &envelope);
            let (label, moderation_case, operation_error) = match result {
                Ok(case) => (
                    format!(
                        "Report submitted as {}. Status will appear after the registry owner publishes a signed transparency snapshot.",
                        case.case_id
                    ),
                    None,
                    None,
                ),
                Err(error) => {
                    let message = format!(
                        "Report was not submitted. {error} The collaborative draft is unchanged; check your identity or connection and retry."
                    );
                    (message.clone(), None, Some(message))
                }
            };
            snapshots
                .send(Snapshot {
                    items: state.base_items.clone(),
                    status_line: label,
                    query: None,
                    detail: None,
                    remotes: state.remotes.clone(),
                    invocation,
                    operation_error,
                    moderation_case,
                })
                .is_ok()
        }
        Request::RefreshModeration {
            registry_name,
            case_id,
        } => send_moderation_result(
            state,
            snapshots,
            invocation,
            moderation_registry(&registry_name).and_then(|registry| {
                crate::moderation_client::fetch_verified_case(&registry, &case_id)
            }),
            "Moderation status refreshed",
        ),
        Request::AppealModeration {
            registry_name,
            case_id,
            reason,
        } => {
            let result = moderation_registry(&registry_name).and_then(|registry| {
                let now = ui::time::wall()
                    .duration_since(ui::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let mut nonce = [0_u8; 16];
                SysRng.try_fill_bytes(&mut nonce).map_err(|error| {
                    crate::moderation_client::ModerationClientError::Identity(error.to_string())
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
            });
            send_moderation_submission_result(
                state,
                snapshots,
                invocation,
                result,
                "Appeal submitted",
            )
        }
    }
}

fn search(query: &str, state: &mut State, snapshots: &mpsc::Sender<Snapshot>) -> bool {
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

fn detail(id: &str, state: &mut State, snapshots: &mpsc::Sender<Snapshot>) -> bool {
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
    snapshots: &mpsc::Sender<Snapshot>,
    state: &State,
    items: Vec<StoreItem>,
    status_line: &str,
    query: Option<String>,
    detail: Option<StoreItem>,
) -> bool {
    send_tracked(
        snapshots,
        state,
        items,
        status_line,
        query,
        detail,
        None,
        None,
    )
}

fn send_tracked(
    snapshots: &mpsc::Sender<Snapshot>,
    state: &State,
    items: Vec<StoreItem>,
    status_line: &str,
    query: Option<String>,
    detail: Option<StoreItem>,
    invocation: Option<tilo_actions::InvocationId>,
    operation_error: Option<String>,
) -> bool {
    snapshots
        .send(Snapshot {
            items,
            status_line: status_line.to_string(),
            query,
            detail,
            remotes: state.remotes.clone(),
            invocation,
            operation_error,
            moderation_case: None,
        })
        .is_ok()
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

fn send_moderation_result(
    state: &State,
    snapshots: &mpsc::Sender<Snapshot>,
    invocation: Option<tilo_actions::InvocationId>,
    result: Result<registry_types::ModerationCase, crate::moderation_client::ModerationClientError>,
    success: &str,
) -> bool {
    let (status_line, moderation_case, operation_error) = match result {
        Ok(case) => (
            format!(
                "{success}: {} is {}.",
                case.case_id,
                crate::app::moderation_status_label(case.status)
            ),
            Some(case),
            None,
        ),
        Err(error) => {
            let message = format!(
                "Registry request failed: {error} The collaborative draft and last confirmed status are unchanged; retry when ready."
            );
            (message.clone(), None, Some(message))
        }
    };
    snapshots
        .send(Snapshot {
            items: state.base_items.clone(),
            status_line,
            query: None,
            detail: None,
            remotes: state.remotes.clone(),
            invocation,
            operation_error,
            moderation_case,
        })
        .is_ok()
}

fn send_moderation_submission_result(
    state: &State,
    snapshots: &mpsc::Sender<Snapshot>,
    invocation: Option<tilo_actions::InvocationId>,
    result: Result<registry_types::ModerationCase, crate::moderation_client::ModerationClientError>,
    success: &str,
) -> bool {
    let (status_line, operation_error) = match result {
        Ok(case) => (
            format!(
                "{success} for {}. The last verified status stays visible until an owner-signed transparency snapshot includes it.",
                case.case_id
            ),
            None,
        ),
        Err(error) => {
            let message = format!(
                "Registry request failed: {error} The collaborative draft and last verified status are unchanged; retry when ready."
            );
            (message.clone(), Some(message))
        }
    };
    snapshots
        .send(Snapshot {
            items: state.base_items.clone(),
            status_line,
            query: None,
            detail: None,
            remotes: state.remotes.clone(),
            invocation,
            operation_error,
            moderation_case: None,
        })
        .is_ok()
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
            let worker = StoreWorker::spawn(None, StoreSearchIndex::default());
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
            let (snapshots, received) = mpsc::channel();

            assert!(send_tracked(
                &snapshots,
                &state,
                vec![item("dev.example.alpha", "Alpha")],
                "1 result for Alpha.",
                Some("Alpha".into()),
                None,
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
                .search_page(&ui::search::SearchQuery::new(""), None, 1, 1)
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
