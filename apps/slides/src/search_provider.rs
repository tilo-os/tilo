//! Slides as a qOS global-search source.
//!
//! Every row names an exported resource `View` action. Slides therefore has no
//! private activation-token namespace: pointer, keyboard, assistive technology,
//! search and agents all resolve the same stable slide/element/text address
//! through the shared broker.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{Deck, Element, ElementKind, Slide};

pub const SLIDE_KIND: &str = "Slides";
pub const ELEMENT_KIND: &str = "Slide elements";
pub const TEXT_KIND: &str = "Slide text";
pub const NOTES_KIND: &str = "Speaker notes";

/// A synchronous snapshot of the open collaborative deck.
pub struct SlideSearchProvider {
    slides: Vec<Slide>,
}

impl SlideSearchProvider {
    pub fn new(deck: &Deck) -> Self {
        Self {
            slides: deck
                .slides
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned()
                .collect(),
        }
    }

    fn slide_title(slide: &Slide, index: usize) -> String {
        slide
            .elements
            .iter()
            .filter(|element| element.kind == ElementKind::Text)
            .map(|element| element.text.trim())
            .find(|text| !text.is_empty())
            .map_or_else(
                || format!("Slide {}", index + 1),
                |text| text.lines().next().unwrap_or(text).to_string(),
            )
    }

    fn slide_haystack(slide: &Slide) -> String {
        let mut text = slide
            .elements
            .iter()
            .filter(|element| element.kind == ElementKind::Text)
            .map(|element| element.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        text.push_str(&slide.notes);
        text
    }

    fn preview(text: &str) -> String {
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() > 72 {
            collapsed.chars().take(69).collect::<String>() + "…"
        } else {
            collapsed
        }
    }

    fn element_haystack(element: &Element) -> String {
        format!("{} {}", element.kind.label(), element.src).to_lowercase()
    }
}

impl SearchProvider for SlideSearchProvider {
    fn name(&self) -> &str {
        SLIDE_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.trim().to_lowercase();
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        let mut rows = Vec::with_capacity(limit.min(32));
        for (index, slide) in self.slides.iter().enumerate() {
            let slide_title = Self::slide_title(slide, index);
            if query.is_empty() || Self::slide_haystack(slide).to_lowercase().contains(&query) {
                rows.push(
                    SearchResult::for_action(
                        SLIDE_KIND,
                        slide_title.clone(),
                        crate::actions::open_slide_address(&slide.id),
                    )
                    .with_subtitle(format!("Slide {}", index + 1))
                    .with_icon(Icon::Layers),
                );
                if rows.len() == limit {
                    return rows;
                }
            }

            // Empty search remains a compact slide browser. Once the user has
            // intent, return the exact nested target as well.
            if query.is_empty() {
                continue;
            }
            for element in &slide.elements {
                if Self::element_haystack(element).contains(&query) {
                    rows.push(
                        SearchResult::for_action(
                            ELEMENT_KIND,
                            element.kind.label(),
                            crate::actions::open_element_address(&element.id),
                        )
                        .with_subtitle(format!("{slide_title} · Slide {}", index + 1))
                        .with_icon(Icon::Layers),
                    );
                    if rows.len() == limit {
                        return rows;
                    }
                }
                if element.kind == ElementKind::Text
                    && !element.text.trim().is_empty()
                    && element.text.to_lowercase().contains(&query)
                {
                    rows.push(
                        SearchResult::for_action(
                            TEXT_KIND,
                            Self::preview(&element.text),
                            crate::actions::open_text_address(&element.id),
                        )
                        .with_subtitle(format!("{slide_title} · Slide {}", index + 1))
                        .with_icon(Icon::FileText),
                    );
                    if rows.len() == limit {
                        return rows;
                    }
                }
            }
            if !slide.notes.trim().is_empty() && slide.notes.to_lowercase().contains(&query) {
                rows.push(
                    SearchResult::for_action(
                        NOTES_KIND,
                        Self::preview(&slide.notes),
                        crate::actions::open_notes_address(&slide.id),
                    )
                    .with_subtitle(format!("{slide_title} · Slide {}", index + 1))
                    .with_icon(Icon::FileText),
                );
                if rows.len() == limit {
                    return rows;
                }
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};
    use ui::search::SearchRegistry;
    use ui::widget::Widget;

    #[test]
    fn provider_finds_slide_and_exact_text_resource() {
        let deck = crate::model::seed_deck();
        let first = &deck.slides[0];
        let element = first
            .elements
            .iter()
            .find(|element| element.kind == ElementKind::Text && !element.text.trim().is_empty())
            .expect("seeded deck has text");
        let word = element.text.split_whitespace().next().unwrap();
        let hits = SlideSearchProvider::new(&deck).search(word);
        assert!(hits.iter().any(|hit| hit.kind == SLIDE_KIND));
        let text = hits
            .iter()
            .find(|hit| hit.kind == TEXT_KIND)
            .expect("matching text is an exact nested hit");
        assert_eq!(
            text.action_address(),
            Some(crate::actions::open_text_address(&element.id))
        );
    }

    #[test]
    fn empty_query_browses_every_slide_without_flooding_nested_content() {
        let deck = crate::model::seed_deck();
        assert_eq!(
            SlideSearchProvider::new(&deck).search("").len(),
            deck.slides.len()
        );
    }

    #[test]
    fn every_hit_carries_a_canonical_action_address() {
        let deck = crate::model::seed_deck();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SlideSearchProvider::new(&deck)));
        let word = deck.slides[0].elements[0]
            .text
            .split_whitespace()
            .next()
            .unwrap();
        let hits = registry.query(word);
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.action_address().is_some()));
    }

    #[test]
    fn search_address_resolves_through_the_published_resource_tree() {
        let mut app = crate::app::SlidesApp::demo(None);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let target = app.resources(&doc)[0].children[2].id.0.clone();
        let slide_id = target.strip_prefix("slides.slide:").unwrap();
        let hit = app.search_providers()[0]
            .search("")
            .into_iter()
            .find(|hit| hit.action_address() == Some(crate::actions::open_slide_address(slide_id)))
            .expect("target slide is searchable");
        let address = hit.action_address().unwrap();
        let encoded = qos_actions::door::encode(&app.resources(&doc), &address, None)
            .expect("search address resolves against Slides resources");
        assert!(
            app.invoke_resource_operation(
                qos_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .unwrap()
        );
        assert_eq!(app.current_slide(), 2);
    }
}
