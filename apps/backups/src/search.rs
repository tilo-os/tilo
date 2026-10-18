//! Local backup history as search entries.
//!
//! The visible resource tree deliberately publishes a small contextual page of
//! snapshots. Search is different: a recovery point must remain findable even
//! when it is older than that page, so every loaded local snapshot is an
//! entry, and the framework's search source bounds the query work.

use ui::tilo_resource::SearchEntry;

use crate::model::{Catalog, Snapshot};

pub const SNAPSHOT_KIND: &str = "Backup recovery points";

/// Every recovery point in the loaded catalogue, each anchored at its live
/// row, which the window's opener shows:
/// found by its id, time, source, destination, kind, retention or status.
pub fn snapshot_entries(backups: &Catalog) -> Vec<SearchEntry> {
    backups
        .snapshots
        .iter()
        .map(|snapshot| {
            SearchEntry::new(
                snapshot
                    .label
                    .clone()
                    .unwrap_or_else(|| format!("{} backup", snapshot.taken)),
                format!(
                    "{} {} {} {}",
                    snapshot.id,
                    snapshot.source_id,
                    snapshot.dest_id,
                    snapshot.status_label()
                ),
            )
            .anchored(snapshot_resource_id(snapshot))
            .grouped(SNAPSHOT_KIND)
            .subtitled(format!(
                "{} backup, retained {} ({}).",
                snapshot.kind.label(),
                snapshot.retention.label(),
                ui::format::size(snapshot.size_bytes)
            ))
            .with_icon("history")
        })
        .collect()
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

    /// The app's search source over `catalog`, as the framework builds it.
    fn source(catalog: &Catalog) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::resource::Backups>(snapshot_entries(catalog))
    }
    use ui::search::testing::{Corpus, LATE_HIT_NEEDLE, assert_late_hit_reachable};

    fn large_catalog(corpus: Corpus) -> Catalog {
        let mut catalog = Catalog::sample(
            chrono::DateTime::from_timestamp(1_782_835_500, 0)
                .expect("fixture timestamp")
                .naive_utc(),
        );
        let template = catalog.snapshots[0].clone();
        catalog.snapshots = corpus
            .map(|row| {
                let mut snapshot = template.clone();
                snapshot.id = format!("snapshot-{:06}", row.index);
                snapshot.label = Some(row.text);
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
                let provider = source(&catalog);
                loaded = Some(catalog);
                provider
            },
            3_000,
        );
        let catalog = loaded.expect("the corpus was loaded");
        let page = provider.search_page(&ui::search::SearchQuery::new(LATE_HIT_NEEDLE), None, 8, 4);
        assert_eq!(page.results.len(), 1, "the one recovery point that matches");
        let anchor = hit.anchor();
        assert_eq!(anchor, "backups.snapshot:snapshot-002999");
        let resource = crate::actions::live_row(&catalog, None, &anchor)
            .expect("late search anchor resolves its live row")
            .into_resource();
        assert!(
            resource.actions.iter().any(|action| action.id == "open"),
            "the hit's row opens it"
        );
        assert_eq!(
            crate::resource::opened(&anchor).expect("the opener shows it"),
            crate::actions::BackupsEffect::ShowSnapshot {
                snapshot_id: "snapshot-002999".into(),
            }
        );
        assert!(resource.actions.iter().any(|action| action.id == "verify"));
        assert!(resource.actions.iter().any(|action| action.id == "restore"));
        let page = provider.search_page(&ui::search::SearchQuery::new("snapshot"), None, 17, 5);
        assert!(page.inspected <= 17);
        assert!(page.results.len() <= 5);
        assert!(page.next.is_some(), "the complete history remains pageable");
    }

    /// Case of `snapshot_search_is_bounded_and_invalidated_by_catalog_refresh`.
    fn catalog_refresh_invalidates_existing_search_cursors() {
        let catalog = Catalog::sample(
            chrono::DateTime::from_timestamp(1_782_835_500, 0)
                .expect("fixture timestamp")
                .naive_utc(),
        );
        let mut refreshed = catalog.clone();
        refreshed.snapshots[0].label = Some("changed recovery time".into());
        assert_ne!(
            snapshot_entries(&catalog),
            snapshot_entries(&refreshed),
            "a refreshed catalogue is new entries, which the framework indexes afresh",
        );
        assert!(
            source(&refreshed)
                .search("changed recovery")
                .iter()
                .any(|hit| hit.title == "changed recovery time")
        );
    }
}
