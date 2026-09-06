//! Bounded, cursor-paged global search over the current Store catalogue.
//!
//! The palette never receives an install operation as its activation: picking
//! an app opens its details, where provenance and the explicit primary action
//! remain visible. The retained catalogue is cheap to clone and its resolver
//! checks the current rows, so a result that disappeared after it was rendered
//! is refused rather than navigating to a stale package identity.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use qos_actions::{ActionAddress, ActionSpec, OperationKind, Resource};
use ui::icons::Icon;
use ui::search::{SearchCatalog, SearchPage, SearchProvider, SearchResult};

use crate::{StoreItem, StoreSource};

const DEFAULT_RESULT_LIMIT: usize = 100;

#[derive(Clone)]
struct Row {
    resource_id: String,
    title: String,
    subtitle: String,
}

#[derive(Default)]
struct IndexState {
    revision: Option<u64>,
    catalog: Option<SearchCatalog>,
    rows: BTreeMap<String, Row>,
}

/// Store-owned retained search index. Replacing an unchanged worker snapshot
/// is O(1), while the shared catalogue bounds each query by a cursor and a
/// caller-supplied inspection budget.
#[derive(Clone, Default)]
pub struct StoreSearchIndex {
    state: Arc<RwLock<IndexState>>,
}

impl StoreSearchIndex {
    /// Replace the durable catalogue projection after a worker snapshot.
    pub fn sync(&self, items: &[StoreItem]) {
        let revision = catalogue_revision(items);
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revision == Some(revision) && state.catalog.is_some() {
            return;
        }
        drop(state);

        let catalog = SearchCatalog::new("App Store");
        let mut rows = BTreeMap::new();
        for item in items {
            let row = Row {
                resource_id: resource_id(item),
                title: item.name.clone(),
                subtitle: format!(
                    "{} · {} · {}",
                    item.source.short_label(),
                    item.trust.label(),
                    item.latest_version()
                ),
            };
            let result = SearchResult::for_action(
                "Apps",
                &row.title,
                ActionAddress::new(&row.resource_id, "show-details"),
            )
            .with_subtitle(&row.subtitle)
            .with_icon(Icon::LayoutGrid);
            if catalog
                .upsert(&row.resource_id, searchable_text(item), result)
                .is_ok()
            {
                rows.insert(row.resource_id.clone(), row);
            }
        }

        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.revision = Some(revision);
        state.catalog = Some(catalog);
        state.rows = rows;
    }

    #[must_use]
    pub fn provider(&self) -> StoreSearchProvider {
        StoreSearchProvider {
            index: self.clone(),
        }
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

impl SearchProvider for StoreSearchProvider {
    #[allow(
        clippy::unnecessary_literal_bound,
        reason = "The shared SearchProvider trait requires a borrowed literal name."
    )]
    fn name(&self) -> &str {
        "App Store"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.search_page(query, None, limit, limit.min(DEFAULT_RESULT_LIMIT))
            .results
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&ui::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let catalog = self
            .index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .clone()
            .unwrap_or_else(|| SearchCatalog::new("App Store"));
        catalog.query_page(query, cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
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

fn resource_id(item: &StoreItem) -> String {
    format!("app:{}", item.id)
}

fn searchable_text(item: &StoreItem) -> String {
    let source = match &item.source {
        StoreSource::Qos { registry_name } => registry_name.as_str(),
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

fn catalogue_revision(items: &[StoreItem]) -> u64 {
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
    hasher
        .finalize()
        .as_bytes()
        .iter()
        .fold(0_u64, |revision, byte| {
            revision.rotate_left(5) ^ u64::from(*byte)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstalledState, StoreItemVersion, StoreTrust};

    fn item(index: usize) -> StoreItem {
        StoreItem {
            id: format!("dev.example.app{index}"),
            name: format!("Example App {index}"),
            description: "A durable Store test row".into(),
            categories: vec!["Utilities".into()],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Qos {
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

    #[test]
    fn full_catalogue_is_cursor_paged_and_details_are_canonical() {
        let items = (0..2_048).map(item).collect::<Vec<_>>();
        let index = StoreSearchIndex::default();
        index.sync(&items);
        let provider = index.provider();
        let first = provider.search_page("example", None, 9, 4);
        assert!(first.inspected <= 9);
        assert_eq!(first.results.len(), 4);
        assert!(first.next.is_some());
        let address = first.results[0].action_address().unwrap();
        let resource = provider.resolve_action_resource(&address).unwrap();
        assert_eq!(resource.actions[0].id, "show-details");
        assert_eq!(resource.actions[0].kind, OperationKind::View);
    }

    #[test]
    fn removed_catalogue_entry_refuses_stale_palette_address() {
        let index = StoreSearchIndex::default();
        index.sync(&[item(7)]);
        let provider = index.provider();
        let hit = provider.search_limited("app 7", 1).pop().unwrap();
        let address = hit.action_address().unwrap();
        index.sync(&[]);
        assert!(provider.resolve_action_resource(&address).is_none());
    }
}
