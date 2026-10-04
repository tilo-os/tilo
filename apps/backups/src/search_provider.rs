//! Complete, retained search projection for local backup history.
//!
//! The visible resource tree deliberately publishes a small contextual page of
//! snapshots. Search is different: a recovery point must remain findable even
//! when it is older than that page, so this index retains every loaded local
//! snapshot and lets the shared catalogue enforce bounded query work.

use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};
use ui::tilo_actions::ActionAddress;

use crate::model::{Catalog, Snapshot};

pub const SNAPSHOT_KIND: &str = "Backup recovery points";

#[derive(Clone, Debug)]
pub struct BackupsSearchIndex {
    catalog: SearchCatalog,
}

impl BackupsSearchIndex {
    pub fn new(backups: &Catalog) -> Self {
        let index = Self {
            catalog: SearchCatalog::new("Backups"),
        };
        index.sync(backups);
        index
    }

    /// Refresh from the already-loaded local catalogue. This is deliberately
    /// called on a repository refresh, never from a palette keypress.
    pub fn sync(&self, backups: &Catalog) {
        let rows = backups.snapshots.iter().map(|snapshot| {
            let resource_id = snapshot_resource_id(snapshot);
            let result = SearchResult::for_action(
                SNAPSHOT_KIND,
                format!("{} backup", snapshot.time),
                ActionAddress::new(&resource_id, "open"),
            )
            .with_subtitle(format!(
                "{} · {} · {}",
                snapshot.kind.label(),
                snapshot.retention.label(),
                ui::format::bytes(snapshot.size_bytes)
            ))
            .with_icon(Icon::History);
            let searchable = format!(
                "{} {} {} {} {} {} {}",
                snapshot.id,
                snapshot.time,
                snapshot.source_id,
                snapshot.dest_id,
                snapshot.kind.label(),
                snapshot.retention.label(),
                snapshot.status_label()
            );
            (resource_id, searchable, result)
        });
        // Each row is generated from a stable snapshot id and canonical open
        // address. An invalid local catalogue is not an excuse to publish a
        // partial index, so leave the last coherent projection in place.
        let _ = self.catalog.replace_all(rows);
    }

    pub fn provider(&self) -> BackupsSearchProvider {
        BackupsSearchProvider {
            catalog: self.catalog.clone(),
        }
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

#[derive(Clone, Debug)]
pub struct BackupsSearchProvider {
    catalog: SearchCatalog,
}

impl CatalogSearchProvider for BackupsSearchProvider {
    fn provider_name(&self) -> &'static str {
        "Backups"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn limited_work_budget(&self, limit: usize) -> usize {
        // A one-shot query inspects no more candidates than results it asked for.
        limit
    }
}

pub fn snapshot_resource_id(snapshot: &Snapshot) -> String {
    format!("backups.snapshot:{}", snapshot.id)
}

impl Snapshot {
    fn status_label(&self) -> &'static str {
        match self.status {
            crate::model::SnapStatus::Complete => "complete",
            crate::model::SnapStatus::Verifying => "verifying",
            crate::model::SnapStatus::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;
    use ui::search::testing::{Corpus, LATE_HIT_NEEDLE, assert_late_hit_reachable};

    fn large_catalog(corpus: Corpus) -> Catalog {
        let mut catalog = Catalog::sample();
        let template = catalog.snapshots[0].clone();
        catalog.snapshots = corpus
            .map(|row| {
                let mut snapshot = template.clone();
                snapshot.id = format!("snapshot-{:06}", row.index);
                snapshot.time = row.text;
                snapshot
            })
            .collect();
        catalog
    }

    /// A late recovery point is found with bounded query work and resolves to
    /// its resource, and a catalog refresh invalidates prior cursors; each case
    /// builds its own index.
    #[test]
    fn snapshot_search_is_bounded_and_invalidated_by_catalog_refresh() {
        late_recovery_point_is_found_with_bounded_query_work();
        catalog_refresh_invalidates_existing_search_cursors();
    }

    /// Case of `snapshot_search_is_bounded_and_invalidated_by_catalog_refresh`.
    fn late_recovery_point_is_found_with_bounded_query_work() {
        let mut loaded = None;
        let (provider, hit) = assert_late_hit_reachable(
            |corpus| {
                let catalog = large_catalog(corpus);
                let index = BackupsSearchIndex::new(&catalog);
                assert_eq!(index.stats().records, 3_000);
                loaded = Some(catalog);
                index.provider()
            },
            3_000,
        );
        let catalog = loaded.expect("the corpus was loaded");
        let page = provider.search_page(LATE_HIT_NEEDLE, None, 8, 4);
        assert_eq!(page.inspected, 1, "unique posting avoids a corpus scan");
        let address = hit.action_address().expect("canonical address");
        assert_eq!(address.resource, "backups.snapshot:snapshot-002999");
        let resource = crate::actions::live_row(&catalog, None, &address.resource)
            .expect("late search address resolves its live row")
            .into_resource();
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == address.action),
            "the exact search address action is exported by its resolved resource"
        );
        assert!(resource.actions.iter().any(|action| action.id == "verify"));
        assert!(resource.actions.iter().any(|action| action.id == "restore"));
        let page = provider.search_page("snapshot", None, 17, 5);
        assert!(page.inspected <= 17);
        assert!(page.results.len() <= 5);
        assert!(page.next.is_some(), "the complete history remains pageable");
    }

    /// Case of `snapshot_search_is_bounded_and_invalidated_by_catalog_refresh`.
    fn catalog_refresh_invalidates_existing_search_cursors() {
        let catalog = Catalog::sample();
        let index = BackupsSearchIndex::new(&catalog);
        let provider = index.provider();
        let first = provider.search_page("", None, 1, 1);
        let cursor = first.next.expect("a multi-snapshot catalogue pages");
        let generation = index.stats().generation;

        let mut refreshed = catalog;
        refreshed.snapshots[0].time = "changed recovery time".into();
        index.sync(&refreshed);

        assert!(index.stats().generation > generation);
        assert!(
            !provider.action_directory_cursor_is_current(&cursor),
            "a cursor from the prior loaded catalogue is stale"
        );
    }
}
