//! Snippets as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the snippet manager
//! declares its searchable type by implementing [`SearchProvider`] over a
//! snapshot of the library, so a snippet is findable from the unified Ctrl+K
//! palette by title, language, folder, tag, description or body. Activating a
//! row hands back an opaque `action_id` the app resolves to a [`SnippetId`] and
//! opens.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! against the in-memory [`Library`] the app already holds, so a Ctrl+K query
//! never blocks.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{Library, Snippet, SnippetId};

/// The kind label (and result-group header) Snippets produces.
pub const SNIPPET_KIND: &str = "Snippets";

/// The `action_id` prefix, shared with the app's action/resource layer so
/// activation stays consistent. A chosen row's `action_id` is
/// `snippets.snippet:<uuid>`; [`parse_action_id`] turns it back into a
/// [`SnippetId`].
const ACTION_PREFIX: &str = "snippets.snippet:";

/// Shown for a snippet whose title is still blank.
const UNTITLED: &str = "Untitled snippet";

/// A [`SearchProvider`] over a snapshot of the library. Rebuild it (cheap)
/// whenever the search surface opens.
pub struct SnippetSearchProvider {
    snippets: Vec<Snippet>,
}

impl SnippetSearchProvider {
    /// Snapshot the current library into a provider.
    pub fn new(library: &Library) -> Self {
        Self {
            snippets: library.snippets.clone(),
        }
    }

    /// The `action_id` for a snippet — what a chosen row hands back.
    pub fn action_id(id: SnippetId) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back to a
    /// [`SnippetId`]. Returns `None` for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<SnippetId> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| rest.parse::<SnippetId>().ok())
    }

    fn display_title(s: &Snippet) -> &str {
        if s.title.trim().is_empty() {
            UNTITLED
        } else {
            s.title.as_str()
        }
    }

    /// Whether the snippet matches `q` (already lower-cased) across all its
    /// searchable text.
    fn matches(s: &Snippet, q: &str) -> bool {
        if q.is_empty() {
            return true;
        }
        Self::display_title(s).to_lowercase().contains(q)
            || s.language.label().to_lowercase().contains(q)
            || s.folder.to_lowercase().contains(q)
            || s.description.to_lowercase().contains(q)
            || s.body.to_lowercase().contains(q)
            || s.tags.iter().any(|t| t.to_lowercase().contains(q))
    }
}

impl SearchProvider for SnippetSearchProvider {
    fn name(&self) -> &str {
        SNIPPET_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.snippets
            .iter()
            .filter(|s| Self::matches(s, &q))
            .map(|s| {
                let subtitle = if s.folder.trim().is_empty() {
                    s.language.label().to_string()
                } else {
                    format!("{} · {}", s.language.label(), s.folder)
                };
                let mut r =
                    SearchResult::new(SNIPPET_KIND, Self::display_title(s), Self::action_id(s.id))
                        .with_subtitle(subtitle)
                        .with_icon(Icon::Code);
                if s.favorite {
                    r = r.with_score(1);
                }
                r
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn seeded() -> Library {
        Library {
            snippets: crate::seed::starter_library(),
        }
    }

    #[test]
    fn provider_finds_a_snippet_by_title_or_body() {
        let lib = seeded();
        let provider = SnippetSearchProvider::new(&lib);
        let some = lib
            .snippets
            .first()
            .expect("starter library is populated")
            .clone();
        let hits = provider.search(&some.title);
        assert!(
            hits.iter()
                .any(|r| r.title == SnippetSearchProvider::display_title(&some)),
            "the provider surfaces the matching snippet"
        );
        assert!(hits.iter().all(|r| r.kind == SNIPPET_KIND));
    }

    #[test]
    fn empty_query_browses_the_whole_library() {
        let lib = seeded();
        let provider = SnippetSearchProvider::new(&lib);
        assert_eq!(provider.search("").len(), lib.snippets.len());
    }

    #[test]
    fn action_id_round_trips_to_a_stored_snippet() {
        let lib = seeded();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SnippetSearchProvider::new(&lib)));

        let some = lib.snippets.first().unwrap().clone();
        let hit = registry
            .query(&some.title)
            .into_iter()
            .find(|r| r.action_id == SnippetSearchProvider::action_id(some.id))
            .expect("the snippet is findable through the registry");
        let id = SnippetSearchProvider::parse_action_id(&hit.action_id).unwrap();
        assert!(
            lib.get(id).is_some(),
            "the id resolves to a real stored snippet"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(SnippetSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(SnippetSearchProvider::parse_action_id("snippets.snippet:not-a-uuid").is_none());
    }

    #[test]
    fn widget_hooks_find_then_open_the_snippet() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Browse the app's OWN provider (its real ids) and take a hit.
        let (action_id, id) = {
            let providers = app.search_providers();
            assert!(
                !providers.is_empty(),
                "snippets publishes a search provider"
            );
            let hits = providers[0].search("");
            let hit = hits
                .first()
                .expect("the seeded library is browsable through the real hook")
                .clone();
            let id = SnippetSearchProvider::parse_action_id(&hit.action_id).unwrap();
            (hit.action_id, id)
        };

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.open_id_for_test(),
            Some(id),
            "activation opened the chosen snippet"
        );
    }
}
