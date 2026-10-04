//! Snippets as a Tilo global-search source.
//!
//! The snippet manager declares its searchable type through a persistent
//! incremental catalogue, so a snippet is findable from the platform search
//! by title, language, folder, tag, description or body. Snippet hits
//! address their exported `open` view action; distinct tag hits address their
//! exported `reveal` view action. The shared broker therefore owns activation
//! for search, accessibility and agents alike.
//!
//! Two facets narrow a query: `language:rust` and `tag:k8s`. The platform
//! parses the typed tokens (and offers the values as chips); the provider
//! only checks a hit's language and tags against them.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! against the in-memory [`Library`] the app already holds, so a query never
//! blocks.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock};

use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, Facet, SearchCatalog, SearchQuery, SearchResult};

use crate::model::{Language, Library, Snippet};

/// The kind label (and result-group header) Snippets produces.
pub const SNIPPET_KIND: &str = "Snippets";
pub const TAG_KIND: &str = "Snippet tags";

/// The facet keys a query can be narrowed by.
pub const LANGUAGE_FACET: &str = "language";
pub const TAG_FACET: &str = "tag";

/// What a facet filter checks a hit against, by the hit's resource id.
#[derive(Default)]
struct FacetIndex {
    /// Snippet resource id → (language slug, tags).
    snippets: HashMap<String, (&'static str, Vec<String>)>,
    /// Tag resource id → tag.
    tags: HashMap<String, String>,
    /// Every tag in the library, sorted.
    tag_names: Vec<String>,
}

/// A cheap handle to the app's complete, incrementally maintained catalogue.
/// Cloning this provider when the palette opens does not clone the library or
/// rebuild an index.
#[derive(Clone)]
pub struct SnippetSearchProvider {
    catalog: SearchCatalog,
    facets: Arc<RwLock<FacetIndex>>,
}

impl SnippetSearchProvider {
    /// Snapshot the current library into a provider.
    pub fn new(library: &Library) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(SNIPPET_KIND),
            facets: Arc::default(),
        };
        for snippet in &library.snippets {
            provider.index_snippet(snippet);
        }
        provider.update_tags(&Library::new(), library);
        provider
    }

    fn display_title(s: &Snippet) -> String {
        if s.title.trim().is_empty() {
            crate::tr("snippets-untitled")
        } else {
            s.title.clone()
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "SearchResult::for_action is canonical and uses the same resource id passed to SearchCatalog"
    )]
    fn index_snippet(&self, snippet: &Snippet) {
        let mut result = SearchResult::for_action(
            SNIPPET_KIND,
            Self::display_title(snippet),
            crate::actions::open_snippet_address(snippet.id),
        )
        .with_subtitle(snippet.language.name())
        .with_icon(Icon::Code);
        if snippet.favorite {
            result = result.with_score(1);
        }
        let address = result.action_address().expect("constructor is canonical");
        let searchable = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            Self::display_title(snippet),
            snippet.language.label(),
            snippet.folder,
            snippet.description,
            snippet.body,
            snippet.tags.join("\0")
        );
        self.facets
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .snippets
            .insert(
                address.resource.clone(),
                (snippet.language.slug(), snippet.tags.clone()),
            );
        self.catalog
            .upsert(address.resource, searchable, result)
            .expect("snippet action addresses its own resource");
    }

    fn tag_counts(library: &Library) -> BTreeMap<String, usize> {
        library.tags().into_iter().collect()
    }

    #[allow(
        clippy::expect_used,
        reason = "SearchResult::for_action is canonical and uses the same tag resource id passed to SearchCatalog"
    )]
    fn update_tags(&self, before: &Library, after: &Library) {
        let before = Self::tag_counts(before);
        let after = Self::tag_counts(after);
        let mut tags = Vec::new();
        for tag in before.keys().filter(|tag| !after.contains_key(*tag)) {
            let address = crate::actions::reveal_tag_address(tag);
            tags.push((address.resource.clone(), None));
            self.catalog.remove(&address.resource);
        }
        let names = after.keys().cloned().collect();
        for (tag, count) in after {
            if before.get(&tag) == Some(&count) {
                continue;
            }
            let result = SearchResult::for_action(
                TAG_KIND,
                tag.clone(),
                crate::actions::reveal_tag_address(&tag),
            )
            .with_subtitle(snippet_count(count))
            .with_icon(Icon::List);
            let address = result.action_address().expect("constructor is canonical");
            tags.push((address.resource.clone(), Some(tag.clone())));
            self.catalog
                .upsert(address.resource, &tag, result)
                .expect("tag action addresses its own resource");
        }
        let mut facets = self.facets.write().unwrap_or_else(PoisonError::into_inner);
        facets.tag_names = names;
        for (resource, tag) in tags {
            match tag {
                Some(tag) => facets.tags.insert(resource, tag),
                None => facets.tags.remove(&resource),
            };
        }
    }

    /// Incrementally reconcile changed, inserted and deleted stable ids.
    pub(crate) fn update(&self, before: &Library, after: &Library) {
        let old: BTreeMap<_, _> = before.snippets.iter().map(|s| (s.id, s)).collect();
        let new: BTreeMap<_, _> = after.snippets.iter().map(|s| (s.id, s)).collect();
        for id in old.keys().filter(|id| !new.contains_key(*id)) {
            let resource = crate::actions::snippet_resource_id(*id);
            self.facets
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .snippets
                .remove(&resource);
            self.catalog.remove(&resource);
        }
        for (id, snippet) in new {
            if old.get(&id).copied() != Some(snippet) {
                self.index_snippet(snippet);
            }
        }
        self.update_tags(before, after);
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

/// "1 snippet", "3 snippets".
fn snippet_count(count: usize) -> String {
    crate::tr_count("snippets-count", count)
}

impl CatalogSearchProvider for SnippetSearchProvider {
    fn provider_name(&self) -> &str {
        SNIPPET_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn facets(&self) -> Vec<Facet> {
        let languages = Language::ALL.into_iter().fold(
            Facet::new(LANGUAGE_FACET, crate::tr("snippets-language")),
            |facet, language| facet.value(language.slug(), language.name()),
        );
        let names = self
            .facets
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .tag_names
            .clone();
        let tags = names.iter().fold(
            Facet::new(TAG_FACET, crate::tr("snippets-facet-tag")),
            |facet, tag| facet.value(tag.as_str(), tag.as_str()),
        );
        vec![languages, tags]
    }

    fn admits(&self, hit: &SearchResult, query: &SearchQuery) -> bool {
        if query.filters().is_empty() {
            return true;
        }
        let anchor = hit.anchor();
        let (snippet, tag) = {
            let index = self.facets.read().unwrap_or_else(PoisonError::into_inner);
            (
                index.snippets.get(&anchor).cloned(),
                index.tags.get(&anchor).cloned(),
            )
        };
        if let Some((language, tags)) = snippet {
            let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
            return query.admits(LANGUAGE_FACET, &[language]) && query.admits(TAG_FACET, &tags);
        }
        if let Some(tag) = tag {
            // A tag hit has no language: any language filter leaves it out.
            return query.admits(LANGUAGE_FACET, &[]) && query.admits(TAG_FACET, &[&tag]);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;
    use ui::search::SearchRegistry;

    fn seeded() -> Library {
        Library {
            snippets: crate::seed::starter_library(),
        }
    }

    /// Title/body search, whole-library browsing and retained updates.
    #[test]
    fn snippet_search_finds_browses_and_updates_in_place() {
        // Case `provider_finds_a_snippet_by_title_or_body`.
        {
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

        // Case `empty_query_browses_the_whole_library`.
        {
            let lib = seeded();
            let provider = SnippetSearchProvider::new(&lib);
            assert_eq!(
                provider.search("").len(),
                lib.snippets.len() + lib.tags().len()
            );
        }

        // Case `retained_provider_updates_one_resource_without_palette_rebuild`.
        {
            let mut before = Library::new();
            let mut snippet = Snippet::blank("");
            snippet.title = "Before title".into();
            let id = snippet.id;
            before.insert(snippet);
            let provider = SnippetSearchProvider::new(&before);
            let palette_handle = provider.clone();
            let generation = provider.stats().generation;

            let mut after = before.clone();
            after.get_mut(id).unwrap().title = "After title".into();
            provider.update(&before, &after);
            assert_eq!(provider.stats().generation, generation + 1);
            assert!(palette_handle.search_limited("Before title", 4).is_empty());
            assert_eq!(palette_handle.search_limited("After title", 4).len(), 1);

            provider.update(&after, &Library::new());
            assert!(palette_handle.search_limited("After title", 4).is_empty());
        }
    }

    /// Snippet and tag hits address and resolve the exported open/reveal actions.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn search_hits_address_exported_actions_through_the_broker() {
        // Case `snippet_result_addresses_the_exported_open_action`.
        {
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

        // Case `tag_result_addresses_the_exported_reveal_action`.
        {
            let lib = seeded();
            let (tag, count) = lib.tags().into_iter().next().expect("a seeded tag");
            let hit = SnippetSearchProvider::new(&lib)
                .search(&tag)
                .into_iter()
                .find(|hit| hit.kind == TAG_KIND && hit.title == tag)
                .expect("tag is independently searchable");
            assert_eq!(
                hit.action_address(),
                Some(crate::actions::reveal_tag_address(&tag))
            );
            let expected_subtitle = snippet_count(count);
            assert_eq!(hit.subtitle.as_deref(), Some(expected_subtitle.as_str()));
        }

        // Case `canonical_search_hit_resolves_and_opens_through_the_broker`.
        {
            use automerge::{AutoCommit, ROOT};
            use ui::widget::Widget;

            let mut app = crate::app::SnippetsApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);

            let (address, id) = {
                let providers = ui::search::capability::search_providers(&app);
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

            let resources = vec![
                ui::resources::resource(&app, &doc, &address.resource)
                    .expect("search address lazily resolves against the live app"),
            ];
            let spec = tilo_actions::door::resolve(&resources, &address)
                .expect("search address resolves against the live resource tree");
            let encoded = match &spec.input {
                tilo_actions::ActionInput::Immediate(encoded) => encoded,
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("open takes no argument")
                }
            };
            assert_ne!(
                app.open_id_for_test(),
                Some(id),
                "precondition: the selected search hit is not already open"
            );
            assert_eq!(spec.kind, tilo_actions::OperationKind::View);
            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        spec.kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .expect("brokered open succeeds")
            );
            assert_eq!(
                app.open_id_for_test(),
                Some(id),
                "the exported view operation opened the chosen snippet"
            );
        }

        // Case `canonical_tag_hit_resolves_and_reveals_the_filtered_library`.
        {
            use automerge::{AutoCommit, ROOT};
            use ui::widget::Widget;

            let mut app = crate::app::SnippetsApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            let (tag, expected_count) = seeded().tags().into_iter().next().expect("a seeded tag");
            let hit = ui::search::capability::search_providers(&app)[0]
                .search(&tag)
                .into_iter()
                .find(|hit| hit.kind == TAG_KIND && hit.title == tag)
                .expect("the tag provider hit");
            let address = hit.action_address().expect("canonical tag address");
            let resources = vec![
                ui::resources::resource(&app, &doc, &address.resource)
                    .expect("tag address lazily resolves against the live app"),
            ];
            let spec = tilo_actions::door::resolve(&resources, &address)
                .expect("tag address resolves against the exported tree");
            let encoded = match &spec.input {
                tilo_actions::ActionInput::Immediate(encoded) => encoded,
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("reveal takes no argument")
                }
            };

            assert_eq!(spec.kind, tilo_actions::OperationKind::View);
            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        spec.kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .expect("brokered tag reveal succeeds")
            );
            assert_eq!(
                app.visible_count_for_test(),
                expected_count,
                "the reveal operation selects exactly the tagged snippets"
            );
        }
    }

    /// `language:` and `tag:` narrow the platform search to matching snippets.
    #[test]
    fn language_and_tag_facets_narrow_hits() {
        let lib = seeded();
        let provider = SnippetSearchProvider::new(&lib);
        let facets = SearchProvider::facets(&provider);
        assert!(facets.iter().any(|facet| facet.key == LANGUAGE_FACET));
        assert!(
            facets
                .iter()
                .find(|facet| facet.key == TAG_FACET)
                .is_some_and(|facet| facet.values.iter().any(|value| value.value == "k8s"))
        );
        let page = provider.search_page(
            &SearchQuery::parse("language:shell tag:k8s", &facets),
            None,
            1000,
            100,
        );
        let titles: Vec<&str> = page.results.iter().map(|hit| hit.title.as_str()).collect();
        let expected: Vec<&str> = lib
            .snippets
            .iter()
            .filter(|s| s.language == Language::Shell && s.tags.iter().any(|t| t == "k8s"))
            .map(|s| s.title.as_str())
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(titles.len(), expected.len(), "{titles:?}");
        assert!(expected.iter().all(|title| titles.contains(title)));
    }
}
