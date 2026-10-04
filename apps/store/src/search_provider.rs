//! Bounded, cursor-paged global search over the current Store catalogue.
//!
//! The palette never receives an install operation as its activation: picking
//! an app opens its details, where provenance and the explicit primary action
//! remain visible. The retained catalogue is cheap to clone and its resolver
//! checks the current rows, so a result that disappeared after it was rendered
//! is refused rather than navigating to a stale package identity.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use ui::search::{
    CatalogSearchProvider, Facet, SearchCatalog, SearchCatalogError, SearchQuery, SearchResult,
};

use tilo_actions::{ActionAddress, ActionSpec, OperationKind, Resource};
use ui::icons::Icon;

use crate::widgets::{category_label, provenance};
use crate::{StoreItem, StoreSource};

const DEFAULT_RESULT_LIMIT: usize = 100;

#[derive(Clone)]
struct Row {
    resource_id: String,
    title: String,
    subtitle: String,
    /// The values this app has for each facet, so a hit is admitted by the
    /// filters the person picked or typed.
    source: &'static str,
    categories: Vec<String>,
    statuses: Vec<&'static str>,
}

#[derive(Default)]
struct IndexState {
    revision: Option<[u8; 32]>,
    /// The current immutable catalogue. A replacement is completely built
    /// before this lock is held, then receives a successor generation and is
    /// swapped with its resolver rows in one short critical section.
    catalog: Option<SearchCatalog>,
    rows: BTreeMap<String, Row>,
    /// The filter dimensions the current catalogue offers.
    facets: Vec<Facet>,
    /// A failed rebuild deliberately withdraws the provider. Returning the
    /// prior directory after a malformed or exhausted publication would make
    /// agents act on a stale catalogue.
    available: bool,
}

/// Store-owned retained search index. Replacing an unchanged worker snapshot
/// is O(1), while the shared catalogue bounds each query by a cursor and a
/// caller-supplied inspection budget.
#[derive(Clone, Default)]
pub struct StoreSearchIndex {
    state: Arc<RwLock<IndexState>>,
    /// Rebuilds may take linear time, and their publication order must match
    /// authoritative worker order. This intentionally does not guard search
    /// snapshots or resource resolution.
    writer: Arc<Mutex<()>>,
}

impl StoreSearchIndex {
    /// Replace the durable catalogue projection after a worker snapshot.
    pub fn sync(&self, items: &[StoreItem]) {
        self.rebuild_serialized(items, || {});
    }

    /// Build a complete replacement without the public provider lock, then
    /// publish its catalogue and resolver rows together. The hook exists so
    /// tests can pause precisely at that lock-free boundary.
    fn rebuild_serialized(&self, items: &[StoreItem], before_publish: impl FnOnce()) {
        let writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.rebuild_with_writer(items, before_publish);
        drop(writer);
    }

    fn rebuild_with_writer(&self, items: &[StoreItem], before_publish: impl FnOnce()) {
        let revision = catalogue_revision(items);
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revision == Some(revision) && state.available {
            return;
        }
        drop(state);

        let staged = match StagedCatalogue::build(items, revision) {
            Ok(staged) => staged,
            Err(error) => {
                self.fail_closed(&error);
                return;
            }
        };

        before_publish();

        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revision == Some(revision) && state.available {
            return;
        }
        let predecessor_generation = state
            .catalog
            .as_ref()
            .map_or(0, |catalog| catalog.stats().generation);
        match staged.catalog.advance_after(predecessor_generation) {
            Ok(()) => {
                state.revision = Some(staged.revision);
                state.catalog = Some(staged.catalog);
                state.rows = staged.rows;
                state.facets = staged.facets;
                state.available = true;
            }
            Err(error) => Self::fail_closed_locked(&mut state, &error),
        }
    }

    fn fail_closed(&self, error: &SearchCatalogError) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::fail_closed_locked(&mut state, error);
    }

    fn fail_closed_locked(state: &mut IndexState, error: &SearchCatalogError) {
        tracing::error!(%error, "withdrawing Store action directory after failed catalogue rebuild");
        state.revision = None;
        state.catalog = None;
        state.rows.clear();
        state.facets.clear();
        state.available = false;
    }

    #[must_use]
    pub fn provider(&self) -> StoreSearchProvider {
        StoreSearchProvider {
            index: self.clone(),
        }
    }
}

/// A complete, validated replacement built away from the shared provider
/// state. Its independent catalogue is made current only by
/// [`StoreSearchIndex::sync`] after it receives the predecessor generation.
struct StagedCatalogue {
    revision: [u8; 32],
    catalog: SearchCatalog,
    rows: BTreeMap<String, Row>,
    facets: Vec<Facet>,
}

impl StagedCatalogue {
    fn build(items: &[StoreItem], revision: [u8; 32]) -> Result<Self, SearchCatalogError> {
        let catalog = SearchCatalog::new("App Store");
        let mut rows = BTreeMap::new();
        let mut catalogue_rows = Vec::with_capacity(items.len());
        let mut categories = std::collections::BTreeSet::new();
        for item in items {
            let mut statuses = Vec::new();
            if item.installed.is_installed() {
                statuses.push("installed");
            }
            if item.installed.update_available() {
                statuses.push("update");
            }
            categories.extend(item.categories.iter().cloned());
            let row = Row {
                resource_id: resource_id(item),
                title: item.name.clone(),
                subtitle: provenance(item),
                source: source_value(item),
                categories: item.categories.clone(),
                statuses,
            };
            let result = SearchResult::for_action(
                "Apps",
                &row.title,
                ActionAddress::new(&row.resource_id, "show-details"),
            )
            .with_subtitle(&row.subtitle)
            .with_icon(Icon::LayoutGrid);
            if rows.insert(row.resource_id.clone(), row.clone()).is_some() {
                return Err(SearchCatalogError::DuplicateResourceId);
            }
            catalogue_rows.push((row.resource_id.clone(), searchable_text(item), result));
        }
        catalog.replace_all(catalogue_rows)?;
        let mut category = Facet::new("category", "Category");
        for value in categories {
            let label = category_label(&value);
            category = category.value(value, label);
        }
        let facets = vec![
            Facet::new("source", "Source")
                .value("tilo", "Tilo")
                .value("flatpak", "Flatpak"),
            category,
            Facet::new("status", "Status")
                .value("installed", "On this device")
                .value("update", "Update available"),
        ];
        Ok(Self {
            revision,
            catalog,
            rows,
            facets,
        })
    }
}

/// Search handle given to the shared Ctrl+K palette.
pub struct StoreSearchProvider {
    index: StoreSearchIndex,
}

impl StoreSearchProvider {
    fn row_resource(row: &Row) -> Resource {
        let mut resource = Resource::new(&row.resource_id, "store.app", &row.title);
        let app_id = row.resource_id.trim_start_matches("app:");
        if let Ok(details) = ActionSpec::operation(
            "show-details",
            "Show app details",
            OperationKind::View,
            "store.view",
            &app_id,
        ) {
            resource.actions.push(details);
        }
        resource
    }
}

impl CatalogSearchProvider for StoreSearchProvider {
    const RESULT_LIMIT: usize = DEFAULT_RESULT_LIMIT;

    fn provider_name(&self) -> &'static str {
        "App Store"
    }

    /// An unavailable store publishes no catalogue, so it reports no
    /// generation and answers every page empty.
    fn catalog(&self) -> Option<SearchCatalog> {
        let state = self
            .index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.available {
            state.catalog.clone()
        } else {
            None
        }
    }

    /// Source, category and status narrow a search the way the catalogue's
    /// own filters narrowed its list.
    fn facets(&self) -> Vec<Facet> {
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .facets
            .clone()
    }

    fn admits(&self, hit: &SearchResult, query: &SearchQuery) -> bool {
        let row = self
            .index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .get(&hit.anchor())
            .cloned();
        let Some(row) = row else {
            return false;
        };
        let categories: Vec<&str> = row.categories.iter().map(String::as_str).collect();
        query.admits("source", &[row.source])
            && query.admits("category", &categories)
            && query.admits("status", &row.statuses)
    }

    fn limited_work_budget(&self, limit: usize) -> usize {
        // A one-shot query inspects no more candidates than results it asked for.
        limit
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        if address.action != "show-details" {
            return None;
        }
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .get(&address.resource)
            .map(Self::row_resource)
    }
}

/// The value of the source facet: which feed the app comes from.
fn source_value(item: &StoreItem) -> &'static str {
    match item.source {
        StoreSource::Tilo { .. } => "tilo",
        StoreSource::Flathub => "flatpak",
    }
}

fn resource_id(item: &StoreItem) -> String {
    format!("app:{}", item.id)
}

fn searchable_text(item: &StoreItem) -> String {
    let source = match &item.source {
        StoreSource::Tilo { registry_name } => registry_name.as_str(),
        StoreSource::Flathub => "Flathub",
    };
    format!(
        "{} {} {} {} {} {} {}",
        item.name,
        item.id,
        item.description,
        item.categories.join(" "),
        source,
        item.trust.label(),
        item.latest_version(),
    )
}

fn catalogue_revision(items: &[StoreItem]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for item in items {
        hasher.update(item.id.as_bytes());
        hasher.update(&[0]);
        hasher.update(item.name.as_bytes());
        hasher.update(&[0]);
        hasher.update(item.description.as_bytes());
        hasher.update(&[0]);
        for category in &item.categories {
            hasher.update(category.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(item.latest_version().as_bytes());
        hasher.update(&[0]);
        hasher.update(format!("{:?}{:?}{:?}", item.source, item.trust, item.installed).as_bytes());
        hasher.update(&[0xff]);
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstalledState, StoreItemVersion, StoreTrust};
    use std::sync::mpsc;
    use ui::search::SearchProvider;

    fn item(index: usize) -> StoreItem {
        StoreItem {
            id: format!("dev.example.app{index}"),
            name: format!("Example App {index}"),
            description: "A durable Store test row".into(),
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

    /// Cursor paging, canonical details and invalidated stale addresses/cursors.
    #[test]
    fn catalogue_paging_and_stale_addresses_are_refused() {
        // Case `full_catalogue_is_cursor_paged_and_details_are_canonical`.
        {
            let items = (0..2_048).map(item).collect::<Vec<_>>();
            let index = StoreSearchIndex::default();
            index.sync(&items);
            let provider = index.provider();
            let first = provider.search_page(&ui::search::SearchQuery::new("example"), None, 9, 4);
            assert!(first.inspected <= 9);
            assert_eq!(first.results.len(), 4);
            assert!(first.next.is_some());
            let address = first.results[0].action_address().unwrap();
            let resource = provider.resolve_action_resource(&address).unwrap();
            assert_eq!(resource.actions[0].id, "show-details");
            assert_eq!(resource.actions[0].kind, OperationKind::View);
        }

        // Case `removed_catalogue_entry_refuses_stale_palette_address`.
        {
            let index = StoreSearchIndex::default();
            index.sync(&[item(7)]);
            let provider = index.provider();
            let hit = provider.search_limited("app 7", 1).pop().unwrap();
            let address = hit.action_address().unwrap();
            index.sync(&[]);
            assert!(provider.resolve_action_resource(&address).is_none());
        }

        // Case `same_cardinality_replacement_invalidates_old_cursor`.
        {
            let index = StoreSearchIndex::default();
            index.sync(&[item(1), item(2)]);
            let provider = index.provider();
            let cursor = provider
                .search_page(&ui::search::SearchQuery::new(""), None, 1, 1)
                .next
                .expect("a two-row directory has a continuation");

            index.sync(&[item(3), item(4)]);

            assert_eq!(provider.search_limited("app 3", 1).len(), 1);
            assert!(!provider.action_directory_cursor_is_current(&cursor));
        }
    }

    /// Malformed rebuilds withdraw and older stages never publish after newer ones.
    #[test]
    fn bulk_rebuilds_withdraw_malformed_input_and_publish_in_order() {
        // Case `malformed_bulk_rebuild_withdraws_instead_of_partially_publishing`.
        {
            let index = StoreSearchIndex::default();
            index.sync(&[item(9)]);
            let provider = index.provider();
            index.sync(&[item(10), item(10)]);

            assert_eq!(provider.action_directory_generation(), None);
            assert!(provider.search_limited("app", 10).is_empty());
        }

        // Case `serialized_writers_cannot_publish_an_older_stage_after_a_newer_one`.
        {
            let index = StoreSearchIndex::default();
            index.sync(&[item(1)]);
            let older_index = index.clone();
            let (older_staged, older_staged_receiver) = mpsc::channel();
            let (release_older, release_older_receiver) = mpsc::channel();
            let older = std::thread::spawn(move || {
                older_index.rebuild_serialized(&[item(2)], || {
                    older_staged.send(()).expect("older stage signal");
                    release_older_receiver.recv().expect("older release");
                });
            });
            older_staged_receiver
                .recv()
                .expect("older stage pauses before publication");
            assert!(
                index.writer.try_lock().is_err(),
                "the serial writer lock covers the complete rebuild lifecycle"
            );

            let newer_index = index.clone();
            let (newer_started, newer_started_receiver) = mpsc::channel();
            let newer = std::thread::spawn(move || {
                newer_started.send(()).expect("newer writer started");
                newer_index.sync(&[item(3)]);
            });
            newer_started_receiver
                .recv()
                .expect("newer writer attempted publication");

            release_older.send(()).expect("release older publication");
            older.join().expect("older writer");
            newer.join().expect("newer writer");

            let provider = index.provider();
            assert!(provider.search_limited("app 2", 10).is_empty());
            assert_eq!(provider.search_limited("app 3", 10).len(), 1);
        }
    }

    #[test]
    fn large_replacement_keeps_current_search_and_resolution_responsive_before_publish() {
        let index = StoreSearchIndex::default();
        index.sync(&[item(1)]);
        let provider = index.provider();
        let address = provider
            .search_limited("app 1", 1)
            .pop()
            .and_then(|result| result.action_address())
            .expect("initial Store row resolves canonically");
        let items = std::iter::once(item(1))
            .chain((2..20_000).map(item))
            .collect::<Vec<_>>();
        let worker_index = index;
        let (staged, staged_receiver) = mpsc::channel();
        let (continue_publish, continue_receiver) = mpsc::channel();
        let replacement = std::thread::spawn(move || {
            worker_index.rebuild_serialized(&items, || {
                staged.send(()).expect("stage signal");
                continue_receiver.recv().expect("publish release");
            });
        });

        staged_receiver
            .recv()
            .expect("large replacement is staged before publication");
        assert_eq!(provider.search_limited("app 1", 1).len(), 1);
        assert!(provider.resolve_action_resource(&address).is_some());
        continue_publish.send(()).expect("release publication");
        replacement.join().expect("replacement worker");
    }

    /// Source, category and status are facets: typed tokens and chips narrow a search by them.
    #[test]
    fn facets_narrow_a_search_by_source_category_and_status() {
        let tilo = |index: usize, category: &str, installed: InstalledState| {
            let mut entry = item(index);
            entry.categories = vec![category.into()];
            entry.installed = installed;
            entry
        };
        let mut flatpak = tilo(3, "Games", InstalledState::Not);
        flatpak.source = StoreSource::Flathub;
        flatpak.trust = StoreTrust::Flathub;
        let items = vec![
            tilo(1, "Productivity", InstalledState::Not),
            tilo(
                2,
                "Games",
                InstalledState::Installed {
                    version: "1.0.0".into(),
                    available: "1.1.0".into(),
                    pinned: false,
                    user_managed: true,
                },
            ),
            flatpak,
        ];
        let index = StoreSearchIndex::default();
        index.sync(&items);
        let provider = index.provider();

        let facets = SearchProvider::facets(&provider);
        let keys: Vec<&str> = facets.iter().map(|facet| facet.key.as_str()).collect();
        assert_eq!(keys, ["source", "category", "status"]);
        let category = facets.iter().find(|facet| facet.key == "category").unwrap();
        let categories: Vec<&str> = category
            .values
            .iter()
            .map(|value| value.value.as_str())
            .collect();
        assert_eq!(categories, ["Games", "Productivity"]);

        let titles = |input: &str| -> Vec<String> {
            let query = SearchQuery::parse(input, &facets);
            let mut titles: Vec<String> = provider
                .search_page(&query, None, 100, 100)
                .results
                .into_iter()
                .map(|hit| hit.title)
                .collect();
            titles.sort();
            titles
        };
        assert_eq!(titles("example").len(), 3);
        assert_eq!(titles("example source:flatpak"), ["Example App 3"]);
        assert_eq!(
            titles("example category:Games"),
            ["Example App 2", "Example App 3"]
        );
        assert_eq!(titles("example status:installed"), ["Example App 2"]);
        assert_eq!(titles("example status:update"), ["Example App 2"]);
        assert_eq!(
            titles("example source:tilo category:Games"),
            ["Example App 2"]
        );
    }
}
