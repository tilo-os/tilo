//! Snippets as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the snippet manager
//! declares its searchable type by implementing [`SearchProvider`] over a
//! snapshot of the library, so a snippet is findable from the unified Ctrl+K
//! palette by title, language, folder, tag, description or body. Snippet hits
//! address their exported `open` view action; distinct tag hits address their
//! exported `reveal` view action. The shared broker therefore owns activation
//! for search, accessibility and agents alike.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! against the in-memory [`Library`] the app already holds, so a Ctrl+K query
//! never blocks.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{Library, Snippet};

/// The kind label (and result-group header) Snippets produces.
pub const SNIPPET_KIND: &str = "Snippets";
pub const TAG_KIND: &str = "Snippet tags";

/// Shown for a snippet whose title is still blank.
const UNTITLED: &str = "Untitled snippet";

/// A [`SearchProvider`] over a snapshot of the library. Rebuild it (cheap)
/// whenever the search surface opens.
pub struct SnippetSearchProvider {
    snippets: Vec<Snippet>,
    tags: Vec<(String, usize)>,
}

impl SnippetSearchProvider {
    /// Snapshot the current library into a provider.
    pub fn new(library: &Library) -> Self {
        let snippets = library
            .snippets
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        let tags = Library {
            snippets: snippets.clone(),
        }
        .tags();
        Self { snippets, tags }
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
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        let mut results = self
            .snippets
            .iter()
            .filter(|s| Self::matches(s, &q))
            .map(|s| {
                let subtitle = if s.folder.trim().is_empty() {
                    s.language.label().to_string()
                } else {
                    format!("{} · {}", s.language.label(), s.folder)
                };
                let mut r = SearchResult::for_action(
                    SNIPPET_KIND,
                    Self::display_title(s),
                    crate::actions::open_snippet_address(s.id),
                )
                .with_subtitle(subtitle)
                .with_icon(Icon::Code);
                if s.favorite {
                    r = r.with_score(1);
                }
                r
            })
            .take(limit)
            .collect::<Vec<_>>();
        if results.len() == limit {
            return results;
        }
        let remaining = limit - results.len();
        results.extend(
            self.tags
                .iter()
                .filter(|(tag, _)| q.is_empty() || tag.to_lowercase().contains(&q))
                .map(|(tag, count)| {
                    SearchResult::for_action(
                        TAG_KIND,
                        format!("#{tag}"),
                        crate::actions::reveal_tag_address(tag),
                    )
                    .with_subtitle(format!(
                        "{count} snippet{}",
                        if *count == 1 { "" } else { "s" }
                    ))
                    .with_icon(Icon::List)
                })
                .take(remaining),
        );
        results
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
        assert_eq!(
            provider.search("").len(),
            lib.snippets.len() + lib.tags().len()
        );
    }

    #[test]
    fn snippet_result_addresses_the_exported_open_action() {
        let lib = seeded();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SnippetSearchProvider::new(&lib)));

        let some = lib.snippets.first().unwrap().clone();
        let hit = registry
            .query(&some.title)
            .into_iter()
            .find(|r| r.title == SnippetSearchProvider::display_title(&some))
            .expect("the snippet is findable through the registry");
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::open_snippet_address(some.id))
        );
    }

    #[test]
    fn tag_result_addresses_the_exported_reveal_action() {
        let lib = seeded();
        let (tag, count) = lib.tags().into_iter().next().expect("a seeded tag");
        let hit = SnippetSearchProvider::new(&lib)
            .search(&tag)
            .into_iter()
            .find(|hit| hit.kind == TAG_KIND && hit.title == format!("#{tag}"))
            .expect("tag is independently searchable");
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::reveal_tag_address(&tag))
        );
        let expected_subtitle = format!("{count} snippet{}", if count == 1 { "" } else { "s" });
        assert_eq!(hit.subtitle.as_deref(), Some(expected_subtitle.as_str()));
    }

    #[test]
    fn canonical_search_hit_resolves_and_opens_through_the_broker() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let (address, id) = {
            let providers = app.search_providers();
            assert!(
                !providers.is_empty(),
                "snippets publishes a search provider"
            );
            let hits = providers[0].search("");
            let hit = hits
                .iter()
                .find(|hit| hit.kind == SNIPPET_KIND)
                .expect("the seeded library is browsable through the real hook")
                .clone();
            let address = hit.action_address().expect("canonical action address");
            let id = address
                .resource
                .strip_prefix("snippets.snippet:")
                .and_then(|raw| raw.parse().ok())
                .expect("snippet resource id");
            (address, id)
        };

        let resources = Widget::resources(&app, &doc);
        let spec = qos_actions::door::resolve(&resources, &address)
            .expect("search address resolves against the live resource tree");
        let encoded = match &spec.input {
            qos_actions::ActionInput::Immediate(encoded) => encoded,
            qos_actions::ActionInput::Prompted { .. } => panic!("open takes no argument"),
        };
        assert_ne!(
            app.open_id_for_test(),
            Some(id),
            "precondition: the selected search hit is not already open"
        );
        assert_eq!(spec.kind, qos_actions::OperationKind::View);
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .expect("brokered open succeeds")
        );
        assert_eq!(
            app.open_id_for_test(),
            Some(id),
            "the exported view operation opened the chosen snippet"
        );
    }

    #[test]
    fn canonical_tag_hit_resolves_and_reveals_the_filtered_library() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let (tag, expected_count) = seeded().tags().into_iter().next().expect("a seeded tag");
        let hit = app.search_providers()[0]
            .search(&tag)
            .into_iter()
            .find(|hit| hit.kind == TAG_KIND && hit.title == format!("#{tag}"))
            .expect("the tag provider hit");
        let address = hit.action_address().expect("canonical tag address");
        let resources = Widget::resources(&app, &doc);
        let spec = qos_actions::door::resolve(&resources, &address)
            .expect("tag address resolves against the exported tree");
        let encoded = match &spec.input {
            qos_actions::ActionInput::Immediate(encoded) => encoded,
            qos_actions::ActionInput::Prompted { .. } => panic!("reveal takes no argument"),
        };

        assert_eq!(spec.kind, qos_actions::OperationKind::View);
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .expect("brokered tag reveal succeeds")
        );
        assert_eq!(
            app.visible_count_for_test(),
            expected_count,
            "the reveal operation selects exactly the tagged snippets"
        );
    }
}
