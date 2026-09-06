//! Slides' complete, bounded global-search index.
//!
//! Every hit resolves to the same resource `View` action used by the editor,
//! Ctrl+K and assistive technology. The snapshot indexes every deck entity;
//! query work is bounded before result materialisation.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use ui::icons::Icon;
use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

use crate::model::{Deck, Element, ElementKind, Slide};

pub const SLIDE_KIND: &str = "Slides";
pub const ELEMENT_KIND: &str = "Slide elements";
pub const TEXT_KIND: &str = "Slide text";
pub const NOTES_KIND: &str = "Speaker notes";

/// Candidate checks are bounded independently of result materialisation.
const MAX_CANDIDATE_INSPECTIONS: usize = 512;

#[derive(Clone)]
enum SearchTarget {
    Slide {
        id: String,
        number: usize,
        title: String,
    },
    Element {
        id: String,
        number: usize,
        title: String,
        label: String,
    },
    Text {
        id: String,
        number: usize,
        title: String,
        text: String,
    },
    Notes {
        id: String,
        number: usize,
        title: String,
        notes: String,
    },
}

#[derive(Clone)]
struct SearchEntry {
    haystack: String,
    target: SearchTarget,
}

/// An immutable full-deck snapshot. Posting lists contain every matching
/// entity; common queries intentionally use a bounded candidate window.
pub struct SlideSearchProvider {
    entries: Vec<SearchEntry>,
    slide_entries: Vec<usize>,
    postings: BTreeMap<String, BTreeSet<usize>>,
    last_candidate_inspections: Cell<usize>,
}

impl SlideSearchProvider {
    pub fn new(deck: &Deck) -> Self {
        let mut provider = Self {
            entries: Vec::new(),
            slide_entries: Vec::with_capacity(deck.slides.len()),
            postings: BTreeMap::new(),
            last_candidate_inspections: Cell::new(0),
        };
        for (index, slide) in deck.slides.iter().enumerate() {
            let title = Self::slide_title(slide, index);
            let number = index + 1;
            let entry = provider.push_entry(
                &Self::slide_haystack(slide),
                SearchTarget::Slide {
                    id: slide.id.clone(),
                    number,
                    title: title.clone(),
                },
            );
            provider.slide_entries.push(entry);
            for element in &slide.elements {
                provider.push_entry(
                    &Self::element_haystack(element),
                    SearchTarget::Element {
                        id: element.id.clone(),
                        number,
                        title: title.clone(),
                        label: element.kind.label().to_string(),
                    },
                );
                if element.kind == ElementKind::Text && !element.text.trim().is_empty() {
                    provider.push_entry(
                        &element.text,
                        SearchTarget::Text {
                            id: element.id.clone(),
                            number,
                            title: title.clone(),
                            text: element.text.clone(),
                        },
                    );
                }
            }
            if !slide.notes.trim().is_empty() {
                provider.push_entry(
                    &slide.notes,
                    SearchTarget::Notes {
                        id: slide.id.clone(),
                        number,
                        title,
                        notes: slide.notes.clone(),
                    },
                );
            }
        }
        provider
    }

    fn push_entry(&mut self, haystack: &str, target: SearchTarget) -> usize {
        let haystack = normalize(haystack);
        let index = self.entries.len();
        for token in tokens(&haystack) {
            let chars: Vec<char> = token.chars().collect();
            // Exact tokens are useful for ranking, while every 1/2/3-gram is
            // retained so short and mid-token substring queries remain
            // complete without a quadratic all-substring index.
            self.postings
                .entry(token.clone())
                .or_default()
                .insert(index);
            for width in 1..=3.min(chars.len()) {
                for gram in chars.windows(width) {
                    self.postings
                        .entry(gram.iter().collect())
                        .or_default()
                        .insert(index);
                }
            }
        }
        self.entries.push(SearchEntry { haystack, target });
        index
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
        format!("{} {}", element.kind.label(), element.src)
    }

    fn result(entry: &SearchEntry) -> SearchResult {
        match &entry.target {
            SearchTarget::Slide { id, number, title } => SearchResult::for_action(
                SLIDE_KIND,
                title.clone(),
                crate::actions::open_slide_address(id),
            )
            .with_subtitle(format!("Slide {number}"))
            .with_icon(Icon::Layers),
            SearchTarget::Element {
                id,
                number,
                title,
                label,
            } => SearchResult::for_action(
                ELEMENT_KIND,
                label.clone(),
                crate::actions::open_element_address(id),
            )
            .with_subtitle(format!("{title} · Slide {number}"))
            .with_icon(Icon::Layers),
            SearchTarget::Text {
                id,
                number,
                title,
                text,
            } => SearchResult::for_action(
                TEXT_KIND,
                Self::preview(text),
                crate::actions::open_text_address(id),
            )
            .with_subtitle(format!("{title} · Slide {number}"))
            .with_icon(Icon::FileText),
            SearchTarget::Notes {
                id,
                number,
                title,
                notes,
            } => SearchResult::for_action(
                NOTES_KIND,
                Self::preview(notes),
                crate::actions::open_notes_address(id),
            )
            .with_subtitle(format!("{title} · Slide {number}"))
            .with_icon(Icon::FileText),
        }
    }

    fn candidates(&self, query: &str) -> Vec<usize> {
        let keys = query_keys(query);
        if keys.is_empty() || keys.iter().any(|key| !self.postings.contains_key(key)) {
            self.last_candidate_inspections.set(0);
            return Vec::new();
        }
        let Some(rarest) = keys
            .iter()
            .filter_map(|key| self.postings.get(key))
            .min_by_key(|posting| posting.len())
        else {
            self.last_candidate_inspections.set(0);
            return Vec::new();
        };
        let mut candidates = Vec::new();
        let mut inspected = 0;
        for candidate in rarest.iter().take(MAX_CANDIDATE_INSPECTIONS) {
            inspected += 1;
            if keys.iter().all(|key| {
                self.postings
                    .get(key)
                    .is_some_and(|posting| posting.contains(candidate))
            }) {
                candidates.push(*candidate);
            }
        }
        self.last_candidate_inspections.set(inspected);
        candidates
    }

    #[cfg(test)]
    fn last_candidate_inspections(&self) -> usize {
        self.last_candidate_inspections.get()
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
        let query = normalize(query);
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        if query.is_empty() {
            self.last_candidate_inspections.set(0);
            return self
                .slide_entries
                .iter()
                .take(limit)
                .filter_map(|index| self.entries.get(*index))
                .map(Self::result)
                .collect();
        }
        self.candidates(&query)
            .into_iter()
            .filter_map(|index| self.entries.get(index))
            .filter(|entry| entry.haystack.contains(&query))
            .take(limit)
            .map(Self::result)
            .collect()
    }
}

fn normalize(value: &str) -> String {
    value.to_lowercase()
}

fn tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn query_keys(value: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for token in tokens(value) {
        let chars: Vec<char> = token.chars().collect();
        let width = chars.len().min(3);
        keys.extend(chars.windows(width).map(|gram| gram.iter().collect()));
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};
    use ui::search::SearchRegistry;
    use ui::widget::Widget;

    fn large_deck(count: usize, last: &str) -> Deck {
        let mut deck = Deck {
            title: "Large deck".into(),
            palette: crate::model::PaletteKind::Indigo,
            slides: Vec::new(),
        };
        for index in 0..count {
            let mut slide = Slide::new(crate::model::Layout::Blank);
            let text = if index + 1 == count {
                last
            } else {
                "common opening routine material"
            };
            slide.add(Element::text(text, 0.0, 0.0, 100.0, 50.0, 12.0));
            deck.slides.push(slide);
        }
        deck
    }

    #[test]
    fn provider_finds_slide_and_exact_text_resource() {
        let deck = crate::model::seed_deck();
        let first = &deck.slides[0];
        let element = first
            .elements
            .iter()
            .find(|element| element.kind == ElementKind::Text && !element.text.trim().is_empty())
            .unwrap();
        let word = element.text.split_whitespace().next().unwrap();
        let hits = SlideSearchProvider::new(&deck).search(word);
        assert!(hits.iter().any(|hit| hit.kind == SLIDE_KIND));
        assert_eq!(
            hits.iter()
                .find(|hit| hit.kind == TEXT_KIND)
                .unwrap()
                .action_address(),
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
    fn specific_late_partial_phrase_is_indexed_without_snapshot_truncation() {
        let provider =
            SlideSearchProvider::new(&large_deck(4_200, "common opening nebula lantern finale"));
        let hits = provider.search("nebula lan");
        assert!(hits.iter().any(|hit| hit.kind == TEXT_KIND));
        assert!(provider.last_candidate_inspections() <= MAX_CANDIDATE_INSPECTIONS);
    }

    #[test]
    fn long_partial_token_remains_findable_past_the_prefix_index() {
        let late_token = format!("{}needle", "a".repeat(72));
        let provider =
            SlideSearchProvider::new(&large_deck(4_200, &format!("common opening {late_token}")));
        let partial = &late_token[12..68];
        assert!(
            provider
                .search(partial)
                .iter()
                .any(|hit| hit.kind == TEXT_KIND),
            "a >48-character partial token finds the late indexed entity"
        );
        assert!(provider.last_candidate_inspections() <= MAX_CANDIDATE_INSPECTIONS);
    }

    #[test]
    fn short_mid_token_substrings_are_not_mistaken_for_prefixes() {
        let provider = SlideSearchProvider::new(&large_deck(
            4_200,
            "common opening a lantern with a violet interior",
        ));
        assert!(
            provider
                .search("tern")
                .iter()
                .any(|hit| hit.kind == TEXT_KIND)
        );
        assert!(
            provider
                .search("io")
                .iter()
                .any(|hit| hit.kind == TEXT_KIND)
        );
        assert!(provider.last_candidate_inspections() <= MAX_CANDIDATE_INSPECTIONS);
    }

    #[test]
    fn broad_and_missing_queries_do_not_scan_the_complete_index() {
        let provider =
            SlideSearchProvider::new(&large_deck(2_000, "common opening routine material"));
        let _ = provider.search("common");
        assert_eq!(
            provider.last_candidate_inspections(),
            MAX_CANDIDATE_INSPECTIONS
        );
        assert!(provider.search("common missing").is_empty());
        assert_eq!(provider.last_candidate_inspections(), 0);
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
            .unwrap();
        let address = hit.action_address().unwrap();
        let encoded = qos_actions::door::encode(&app.resources(&doc), &address, None).unwrap();
        assert!(
            app.invoke_resource_operation(
                qos_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT
            )
            .unwrap()
        );
        assert_eq!(app.current_slide(), 2);
    }
}
