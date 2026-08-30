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

use std::path::PathBuf;

use ui::search::{SearchProvider, SearchResult};

use crate::favorites::Favorite;

/// The kind label (and result-group header) the file explorer produces.
pub const PLACE_KIND: &str = "Places";

/// The `action_id` prefix. A chosen row's `action_id` is `fav:<path>`;
/// [`parse_action_id`] turns it back into the destination path.
const ACTION_PREFIX: &str = "fav:";

/// A [`SearchProvider`] over a snapshot of the Favourites / Places list.
pub struct PlacesSearchProvider {
    favorites: Vec<Favorite>,
}

impl PlacesSearchProvider {
    /// Snapshot the current favourites into a provider.
    pub fn new(favorites: Vec<Favorite>) -> Self {
        Self { favorites }
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
        PLACE_KIND
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
                SearchResult::new(PLACE_KIND, f.label.clone(), Self::action_id(&f.path))
                    .with_subtitle(f.path.display().to_string())
                    .with_icon(f.icon())
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
