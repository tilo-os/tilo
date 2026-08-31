//! Serialized package/catalog actor. All blocking package and network work
//! crosses this one queue; the UI only performs non-blocking snapshot drains.

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use crate::config::{RegistryConfig, load_registries};
use crate::flathub_client::{Remote, installed_flatpaks};
use crate::model::{ActionKind, StoreSource};
use crate::sources::{
    flathub_placeholder_to_item, flathub_record_to_item, merge, qos_to_store_item,
};
use crate::{FlatpakRef, StoreItem};

const REFRESH_INTERVAL: Duration = Duration::from_mins(15);
const CACHE_CAP: usize = 128;

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
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub items: Vec<StoreItem>,
    pub status_line: String,
    /// Search responses name their query so stale results can be discarded.
    pub query: Option<String>,
    pub detail: Option<StoreItem>,
    pub remotes: Vec<Remote>,
}

pub struct StoreWorker {
    requests: mpsc::Sender<Request>,
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
    pub fn spawn(opened_ref: Option<FlatpakRef>) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(1));
        std::thread::Builder::new()
            .name("store-worker".into())
            .spawn(move || run(&request_rx, &snapshot_tx, opened_ref))
            .expect("spawn store worker");
        Self {
            requests: request_tx,
            snapshots: snapshot_rx,
            pending,
        }
    }

    pub fn request(&self, request: Request) -> bool {
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self.requests.send(request).is_ok() {
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
    opened_ref: Option<FlatpakRef>,
    base_items: Vec<StoreItem>,
    installed: Vec<(String, String)>,
    remotes: Vec<Remote>,
    searches: LruMap<String, Vec<StoreItem>>,
    details: LruMap<String, StoreItem>,
}

fn run(
    requests: &mpsc::Receiver<Request>,
    snapshots: &mpsc::Sender<Snapshot>,
    opened_ref: Option<FlatpakRef>,
) {
    let staging = crate::actions::qpkg_staging_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("qos-store-staged"));
    let _ = std::fs::create_dir_all(&staging);
    let mut state = State {
        opened_ref,
        base_items: Vec::new(),
        installed: Vec::new(),
        remotes: Vec::new(),
        searches: LruMap::new(CACHE_CAP),
        details: LruMap::new(CACHE_CAP),
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
                if !handle(request, &mut state, snapshots, &staging) {
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

fn handle(
    request: Request,
    state: &mut State,
    snapshots: &mpsc::Sender<Snapshot>,
    staging: &std::path::Path,
) -> bool {
    match request {
        Request::Refresh => {
            refresh(state);
            send(
                snapshots,
                state,
                state.base_items.clone(),
                "Store refreshed.",
                None,
                None,
            )
        }
        Request::Search(query) => search(&query, state, snapshots),
        Request::Detail(id) => detail(&id, state, snapshots),
        Request::RefreshRemotes => {
            state.remotes = crate::flathub_client::list_remotes();
            let label = format!("Loaded {} Flatpak remotes.", state.remotes.len());
            send(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
            )
        }
        Request::RemoteAdd { name, url } => {
            let label = run_plan(&crate::actions::flatpak_remote_add_plan(&name, &url));
            state.remotes = crate::flathub_client::list_remotes();
            send(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
            )
        }
        Request::RemoteRemove(name) => {
            let label = run_plan(&crate::actions::flatpak_remote_remove_plan(&name));
            state.remotes = crate::flathub_client::list_remotes();
            send(
                snapshots,
                state,
                state.base_items.clone(),
                &label,
                None,
                None,
            )
        }
        Request::Act(item, action) => {
            let outcome = crate::actions::execute(&item, action, staging);
            refresh(state);
            send(
                snapshots,
                state,
                state.base_items.clone(),
                &outcome.status_line,
                None,
                None,
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
        let qos = state
            .base_items
            .iter()
            .filter(|item| {
                matches!(item.source, StoreSource::Qos { .. }) && item.matches_query(&query)
            })
            .cloned()
            .collect();
        let flatpak = crate::flathub_client::search(&query)
            .unwrap_or_default()
            .iter()
            .map(|record| flathub_record_to_item(record, &state.installed))
            .collect();
        let items = merge(qos, flatpak);
        state.searches.insert(query.clone(), items.clone());
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
        .find(|item| item.id == id && matches!(item.source, StoreSource::Qos { .. }))
        .cloned()
    {
        state.details.insert(id.to_string(), item.clone());
        Some(item)
    } else {
        let loaded = crate::flathub_client::fetch_appstream(id)
            .ok()
            .map(|record| flathub_record_to_item(&record, &state.installed))
            .or_else(|| state.base_items.iter().find(|item| item.id == id).cloned());
        if let Some(item) = loaded.as_ref() {
            state.details.insert(id.to_string(), item.clone());
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

fn run_plan(plan: &crate::actions::SpawnPlan) -> String {
    let (code, output) = crate::actions::spawn_plan(plan);
    if code == 0 {
        plan.status_line.trim_end_matches('…').to_string()
    } else {
        format!(
            "{} (exit {code})",
            output.lines().last().unwrap_or("Flatpak failed").trim()
        )
    }
}

fn send(
    snapshots: &mpsc::Sender<Snapshot>,
    state: &State,
    items: Vec<StoreItem>,
    status_line: &str,
    query: Option<String>,
    detail: Option<StoreItem>,
) -> bool {
    snapshots
        .send(Snapshot {
            items,
            status_line: status_line.to_string(),
            query,
            detail,
            remotes: state.remotes.clone(),
        })
        .is_ok()
}

fn refresh(state: &mut State) {
    state.installed = installed_flatpaks();
    state.remotes = crate::flathub_client::list_remotes();
    state.base_items = collect_items(state.opened_ref.as_ref(), &state.installed);
    state.searches.clear();
    state.details.clear();
}

fn collect_items(
    opened_ref: Option<&FlatpakRef>,
    installed: &[(String, String)],
) -> Vec<StoreItem> {
    let registries = load_registries().registries;
    let installed_qos = crate::installed::load_installed();
    let mut qos_items = Vec::new();
    for registry in registries.iter().filter(|registry| registry.enabled) {
        match crate::registry_client::fetch_entries(registry) {
            Ok(entries) => qos_items.extend(
                entries
                    .iter()
                    .map(|entry| qos_to_store_item(entry, &registry.name, &installed_qos)),
            ),
            Err(error) => tracing::warn!("registry {} refresh failed: {error}", registry.name),
        }
    }
    let mut flatpak_items = crate::flathub_client::fetch_popular(200)
        .unwrap_or_default()
        .iter()
        .map(|entry| flathub_placeholder_to_item(entry, installed))
        .collect::<Vec<_>>();
    if let Some(reference) = opened_ref
        && !flatpak_items.iter().any(|item| item.id == reference.app_id)
    {
        let version = installed
            .iter()
            .find_map(|(id, version)| (id == &reference.app_id).then(|| version.clone()));
        flatpak_items.insert(0, reference.clone().into_store_item(version));
    }
    tracing::debug!("{}", registries_summary(&registries));
    merge(qos_items, flatpak_items)
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

struct LruMap<K, V> {
    values: HashMap<K, V>,
    order: VecDeque<K>,
    capacity: usize,
}

impl<K: Clone + Eq + Hash, V> LruMap<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    /// Generic over `Borrow<Q>` (like [`HashMap::get`]) so callers can look
    /// up by `&str` without owning a `String` just for the query.
    fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        if let Some(index) = self.order.iter().position(|item| item.borrow() == key)
            && let Some(key) = self.order.remove(index)
        {
            self.order.push_back(key);
        }
        self.values.get(key)
    }

    fn insert(&mut self, key: K, value: V) {
        self.values.insert(key.clone(), value);
        if let Some(index) = self.order.iter().position(|item| item == &key) {
            self.order.remove(index);
        }
        self.order.push_back(key);
        while self.values.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
            }
        }
    }

    fn clear(&mut self) {
        self.values.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_revisit_is_retained_and_oldest_is_evicted() {
        let mut cache = LruMap::new(2);
        cache.insert("a", 1);
        cache.insert("b", 2);
        assert_eq!(cache.get(&"a"), Some(&1));
        cache.insert("c", 3);
        assert!(cache.get(&"a").is_some());
        assert!(cache.get(&"b").is_none());
    }

    #[test]
    fn request_and_drain_are_non_blocking() {
        let worker = StoreWorker::spawn(None);
        let started = std::time::Instant::now();
        assert!(worker.request(Request::Search("editor".into())));
        assert!(started.elapsed() < Duration::from_millis(50));
        let started = std::time::Instant::now();
        let _ = worker.try_snapshot();
        assert!(started.elapsed() < Duration::from_millis(20));
    }
}
