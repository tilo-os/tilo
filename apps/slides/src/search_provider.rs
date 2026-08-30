//! Slides as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the deck editor declares
//! its searchable type by implementing [`SearchProvider`] over a snapshot of
//! the deck, so a slide is findable from the unified Ctrl+K palette by any of
//! its text — the title/body of its text boxes and its speaker notes.
//! Activating a row hands back an opaque `action_id` (the slide's stable id)
//! the app resolves to an index and jumps to.
//!
//! Slides carry no dedicated title field; the "title" shown for a result is the
//! first non-empty text box, falling back to the slide number.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{Deck, ElementKind, Slide};

/// The kind label (and result-group header) Slides produces.
pub const SLIDE_KIND: &str = "Slides";

/// The `action_id` prefix. A chosen row's `action_id` is `slide:<id>`;
/// [`parse_action_id`] turns it back into the slide id.
const ACTION_PREFIX: &str = "slide:";

/// A [`SearchProvider`] over a snapshot of the deck's slides.
pub struct SlideSearchProvider {
    slides: Vec<Slide>,
}

impl SlideSearchProvider {
    /// Snapshot the current deck into a provider.
    pub fn new(deck: &Deck) -> Self {
        Self {
            slides: deck.slides.clone(),
        }
    }

    /// The `action_id` for a slide — what a chosen row hands back.
    pub fn action_id(id: &str) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back into a slide id.
    pub fn parse_action_id(action_id: &str) -> Option<&str> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .filter(|s| !s.is_empty())
    }

    /// The first non-empty text box on the slide — used as its title.
    fn slide_title(slide: &Slide, index: usize) -> String {
        slide
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Text)
            .map(|e| e.text.trim())
            .find(|t| !t.is_empty())
            .map_or_else(
                || format!("Slide {}", index + 1),
                |t| t.lines().next().unwrap_or(t).to_string(),
            )
    }

    /// All searchable text on a slide: every text box plus the speaker notes.
    fn slide_haystack(slide: &Slide) -> String {
        let mut s = String::new();
        for e in &slide.elements {
            if e.kind == ElementKind::Text && !e.text.is_empty() {
                s.push_str(&e.text);
                s.push('\n');
            }
        }
        s.push_str(&slide.notes);
        s
    }
}

impl SearchProvider for SlideSearchProvider {
    fn name(&self) -> &str {
        SLIDE_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.slides
            .iter()
            .enumerate()
            .filter(|(_, s)| q.is_empty() || Self::slide_haystack(s).to_lowercase().contains(&q))
            .map(|(i, s)| {
                let subtitle = if s.notes.trim().is_empty() {
                    format!("Slide {}", i + 1)
                } else {
                    format!("Slide {} · notes", i + 1)
                };
                SearchResult::new(SLIDE_KIND, Self::slide_title(s, i), Self::action_id(&s.id))
                    .with_subtitle(subtitle)
                    .with_icon(Icon::Layers)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn seeded() -> Deck {
        crate::model::seed_deck()
    }

    #[test]
    fn provider_finds_a_slide_by_its_text() {
        let deck = seeded();
        let provider = SlideSearchProvider::new(&deck);
        // Pull a real word from the first slide's first text box.
        let first = &deck.slides[0];
        let word = first
            .elements
            .iter()
            .find(|e| e.kind == ElementKind::Text && !e.text.trim().is_empty())
            .and_then(|e| e.text.split_whitespace().next())
            .expect("the seeded deck has text");
        let hits = provider.search(word);
        assert!(!hits.is_empty(), "a real slide word surfaces a slide row");
        assert!(hits.iter().all(|r| r.kind == SLIDE_KIND));
    }

    #[test]
    fn empty_query_browses_every_slide() {
        let deck = seeded();
        let provider = SlideSearchProvider::new(&deck);
        assert_eq!(provider.search("").len(), deck.slides.len());
    }

    #[test]
    fn action_id_round_trips_to_the_chosen_slide() {
        let deck = seeded();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SlideSearchProvider::new(&deck)));
        let target = &deck.slides[2];
        let hit = registry
            .query("")
            .into_iter()
            .find(|r| r.action_id == SlideSearchProvider::action_id(&target.id))
            .expect("the slide is browsable through the registry");
        let id = SlideSearchProvider::parse_action_id(&hit.action_id).unwrap();
        assert_eq!(
            deck.index_of(id),
            Some(2),
            "the id resolves back to the right slide index"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(SlideSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(SlideSearchProvider::parse_action_id("slide:").is_none());
    }

    #[test]
    fn widget_hooks_find_then_navigate_to_the_slide() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::SlidesApp::demo(None);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Browse the app's OWN provider: it emits one row per slide in deck
        // order, so the 3rd row is the app's real slide-index 2 (its ids are
        // whatever the CRDT reload produced).
        let target_idx = 2usize;
        let action_id = {
            let providers = app.search_providers();
            assert!(!providers.is_empty(), "slides publishes a search provider");
            let rows = providers[0].search("");
            assert!(
                rows.len() > target_idx,
                "the seeded deck has several slides"
            );
            rows[target_idx].action_id.clone()
        };
        assert_ne!(
            app.current_slide(),
            target_idx,
            "not already on the target slide"
        );

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.current_slide(),
            target_idx,
            "activation navigated to the chosen slide"
        );
    }
}
