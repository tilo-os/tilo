//! File-explorer places as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the file explorer declares
//! its searchable type by implementing [`SearchProvider`] over the user's
//! curated **Favourites / Places** list, so a pinned location is findable from
//! the unified Ctrl+K palette by its label or path. Activating a row navigates
//! there.
//!
//! Only the stable, in-memory places set is indexed — never the live directory
//! contents (which are not a bounded, stable library) nor the transient
//! back/forward history. The query path touches no filesystem, so it stays
//! synchronous.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ui::search::{SearchProvider, SearchResult};

use crate::favorites::Favorite;
use crate::model::WorkspaceRecord;

/// The `action_id` prefix. A chosen row's `action_id` is `fav:<path>`;
/// [`parse_action_id`] turns it back into the destination path.
const ACTION_PREFIX: &str = "fav:";
const RECORD_ACTION_PREFIX: &str = "record:";

/// A directory projection is untrusted input. Keep the palette snapshot small
/// even when a mount exposes an unusually large directory.
pub const MAX_COLLABORATIVE_SEARCH_RECORDS: usize = 256;

/// A [`SearchProvider`] over a snapshot of the Favourites / Places list.
pub struct PlacesSearchProvider {
    favorites: Vec<Favorite>,
    kind: String,
}

impl PlacesSearchProvider {
    /// Snapshot the current favourites into a provider.
    pub fn new(favorites: Vec<Favorite>) -> Self {
        Self {
            favorites,
            kind: crate::i18n::t("files-a11y-places"),
        }
    }

    /// The `action_id` for a place — its destination path is its identity.
    pub fn action_id(path: &std::path::Path) -> String {
        format!("{ACTION_PREFIX}{}", path.display())
    }

    /// Resolve an `action_id` produced by this provider back into a destination
    /// path. Returns `None` for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<PathBuf> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }
}

impl SearchProvider for PlacesSearchProvider {
    fn name(&self) -> &str {
        &self.kind
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.favorites
            .iter()
            .filter(|f| {
                f.label.to_lowercase().contains(&q)
                    || f.path.to_string_lossy().to_lowercase().contains(&q)
            })
            .map(|f| {
                SearchResult::new(&self.kind, f.label.clone(), Self::action_id(&f.path))
                    .with_subtitle(f.path.display().to_string())
                    .with_icon(f.icon())
            })
            .collect()
    }
}

/// Bounded search over the mount's ACL-filtered room record catalog. Record
/// UUIDs, rather than paths, are action identity: paths are mutable provenance
/// used only for matching and presentation.
pub struct CollaborativeRecordSearchProvider {
    records: Vec<CollaborativeSearchRecord>,
    kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollaborativeSearchRecord {
    record_id: uuid::Uuid,
    name: String,
    provenance: PathBuf,
    is_dir: bool,
}

impl CollaborativeRecordSearchProvider {
    pub fn new(entries: &[WorkspaceRecord]) -> Self {
        let mut seen = BTreeSet::new();
        let records = entries
            .iter()
            .filter_map(|entry| {
                let record_id = entry.record_id;
                seen.insert(record_id).then(|| CollaborativeSearchRecord {
                    record_id,
                    name: entry.name.clone(),
                    provenance: entry.path.clone(),
                    is_dir: entry.is_dir,
                })
            })
            .take(MAX_COLLABORATIVE_SEARCH_RECORDS)
            .collect();
        Self {
            records,
            kind: crate::i18n::t("files-search-collaborative-records"),
        }
    }

    pub fn action_id(record_id: uuid::Uuid) -> String {
        format!("{RECORD_ACTION_PREFIX}{record_id}")
    }

    pub fn parse_action_id(action_id: &str) -> Option<uuid::Uuid> {
        action_id
            .strip_prefix(RECORD_ACTION_PREFIX)
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
    }
}

impl SearchProvider for CollaborativeRecordSearchProvider {
    fn name(&self) -> &str {
        &self.kind
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let query = query.to_lowercase();
        self.records
            .iter()
            .filter(|record| {
                record.name.to_lowercase().contains(&query)
                    || record
                        .provenance
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|record| {
                SearchResult::new(
                    &self.kind,
                    record.name.clone(),
                    Self::action_id(record.record_id),
                )
                .with_subtitle(crate::i18n::t_args(
                    "files-search-record-provenance",
                    &[
                        crate::i18n::arg("path", record.provenance.display().to_string()),
                        crate::i18n::arg("record", record.record_id.to_string()),
                    ],
                ))
                .with_icon(ui::icons::icon_for_path(&record.provenance, record.is_dir))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favorites::{FavKind, Favorite};
    use ui::search::SearchRegistry;

    fn places() -> Vec<Favorite> {
        vec![
            Favorite {
                label: "Home".into(),
                path: "/home/tester".into(),
                is_home: true,
                kind: FavKind::Home,
            },
            Favorite {
                label: "Documents".into(),
                path: "/home/tester/Documents".into(),
                is_home: false,
                kind: FavKind::Documents,
            },
            Favorite {
                label: "Projects".into(),
                path: "/home/tester/code/projects".into(),
                is_home: false,
                kind: FavKind::Folder,
            },
        ]
    }

    fn collaborative_entry(name: &str) -> WorkspaceRecord {
        WorkspaceRecord {
            record_id: uuid::Uuid::nil(),
            name: name.into(),
            path: PathBuf::from(name),
            is_dir: false,
        }
    }

    #[test]
    fn provider_finds_a_place_by_label_or_path() {
        let provider = PlacesSearchProvider::new(places());
        assert!(
            provider
                .search("projects")
                .iter()
                .any(|r| r.title == "Projects")
        );
        // A path-only match (the "code" segment isn't in any label) still hits.
        assert!(
            provider
                .search("code")
                .iter()
                .any(|r| r.title == "Projects")
        );
        assert_eq!(provider.search("").len(), 3);
    }

    #[test]
    fn action_id_round_trips_to_the_destination_path() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(PlacesSearchProvider::new(places())));
        let hit = registry
            .query("documents")
            .into_iter()
            .find(|r| r.title == "Documents")
            .expect("Documents is findable");
        assert_eq!(
            PlacesSearchProvider::parse_action_id(&hit.action_id),
            Some(PathBuf::from("/home/tester/Documents"))
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(PlacesSearchProvider::parse_action_id("contact:42").is_none());
        assert!(PlacesSearchProvider::parse_action_id("fav:").is_none());
    }

    #[test]
    fn collaborative_results_use_stable_record_identity_and_path_as_provenance() {
        let record_id = uuid::Uuid::from_u128(0x51);
        let mut entry = collaborative_entry("old-name.txt");
        entry.path = PathBuf::from("/Workspace/old-name.txt");
        entry.record_id = record_id;
        let provider = CollaborativeRecordSearchProvider::new(&[entry]);
        let hit = provider.search("workspace").pop().expect("path matches");
        assert_eq!(
            CollaborativeRecordSearchProvider::parse_action_id(&hit.action_id),
            Some(record_id)
        );
        assert!(hit.subtitle.unwrap().contains("/Workspace/old-name.txt"));
    }

    #[test]
    fn collaborative_projection_is_deduplicated_and_bounded() {
        let entries: Vec<_> = (0..MAX_COLLABORATIVE_SEARCH_RECORDS + 20)
            .flat_map(|index| {
                let id = uuid::Uuid::from_u128(index as u128 + 1);
                let mut first = collaborative_entry(&format!("file-{index}"));
                first.record_id = id;
                let duplicate = first.clone();
                [first, duplicate]
            })
            .collect();
        let provider = CollaborativeRecordSearchProvider::new(&entries);
        assert_eq!(provider.search("").len(), MAX_COLLABORATIVE_SEARCH_RECORDS);
    }

    #[test]
    fn widget_hooks_find_then_navigate_to_the_place() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        // Build the app against real temp dirs so navigation actually loads.
        let base = std::env::temp_dir().join(format!("qos-fe-search-{}", std::process::id()));
        let home = base.join("home");
        let sub = home.join("sub");
        let cfg = base.join("cfg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();

        let mut app =
            crate::files_app::FileExplorerWidget::with_persistence(Some(cfg), Some(home.clone()));
        // Move away from Home so activation demonstrably navigates back.
        app.navigate(sub.clone());
        assert_eq!(app.current_path(), sub);

        let providers = app.search_providers();
        assert!(
            !providers.is_empty(),
            "the file explorer publishes a search provider"
        );
        let hit = providers[0]
            .search("")
            .into_iter()
            .find(|r| PlacesSearchProvider::parse_action_id(&r.action_id) == Some(home.clone()))
            .expect("the Home favourite is findable through the real hook");

        let mut doc = AutoCommit::new();
        app.activate_search_result(&hit.action_id, &mut doc, &ROOT);
        assert_eq!(
            app.current_path(),
            home,
            "activation navigated to the chosen place"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
