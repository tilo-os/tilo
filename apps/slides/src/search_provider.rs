//! Slides' complete, cursor-paged global-search index.
//!
//! The index retains the entire durable deck corpus, while the palette only
//! receives bounded pages. Every row names a canonical resource action and is
//! lazily re-resolved against the current deck before activation.

use std::collections::BTreeSet;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use tilo_actions::ActionAddress;
use ui::icons::Icon;

use crate::actions::{self, ElementResource, SlideResource, SlidesResourceCtx};
use crate::model::{Deck, Element, ElementKind, Slide};

pub const SLIDE_KIND: &str = "Slides";
pub const ELEMENT_KIND: &str = "Slide elements";
pub const TEXT_KIND: &str = "Slide text";
pub const NOTES_KIND: &str = "Speaker notes";

/// A cheap-to-clone, incrementally refreshed full-deck catalogue. The app owns
/// one instance and refreshes it only when its CRDT mirror changes; providers
/// handed to Ctrl+K therefore share the cached postings rather than rebuilding
/// a large index for each frame.
#[derive(Clone)]
pub struct SlideSearchProvider {
    catalog: SearchCatalog,
    deck: Deck,
    indexed_ids: BTreeSet<String>,
}

impl SlideSearchProvider {
    pub fn new(deck: &Deck) -> Self {
        let mut provider = Self {
            catalog: SearchCatalog::new(SLIDE_KIND),
            deck: Deck {
                title: String::new(),
                palette: crate::model::PaletteKind::Indigo,
                slides: Vec::new(),
            },
            indexed_ids: BTreeSet::new(),
        };
        provider.refresh(deck);
        provider
    }

    /// Apply a complete deck mirror to the catalogue incrementally. Existing
    /// ids retain their order; only changed, inserted, or removed records touch
    /// postings. This runs off the CRDT reload boundary, never per keystroke.
    pub fn refresh(&mut self, deck: &Deck) {
        let mut live = BTreeSet::new();
        for (index, slide) in deck.slides.iter().enumerate() {
            let number = index + 1;
            let title = Self::slide_title(slide, index);
            self.upsert(
                &mut live,
                actions::slide_resource_id(&slide.id),
                Self::slide_haystack(slide),
                SearchResult::for_action(
                    SLIDE_KIND,
                    title.clone(),
                    actions::open_slide_address(&slide.id),
                )
                .with_subtitle(format!("Slide {number}"))
                .with_icon(Icon::Layers),
            );
            for element in &slide.elements {
                self.upsert(
                    &mut live,
                    actions::element_resource_id(&element.id),
                    Self::element_haystack(element),
                    SearchResult::for_action(
                        ELEMENT_KIND,
                        element.kind.label(),
                        actions::open_element_address(&element.id),
                    )
                    .with_subtitle(format!("Slide {number}"))
                    .with_icon(Icon::Layers),
                );
                if element.kind == ElementKind::Text && !element.text.trim().is_empty() {
                    self.upsert(
                        &mut live,
                        actions::text_resource_id(&element.id),
                        &element.text,
                        SearchResult::for_action(
                            TEXT_KIND,
                            Self::preview(&element.text),
                            actions::open_text_address(&element.id),
                        )
                        .with_subtitle(format!("Slide {number}"))
                        .with_icon(Icon::FileText),
                    );
                }
            }
            if !slide.notes.trim().is_empty() {
                self.upsert(
                    &mut live,
                    actions::notes_resource_id(&slide.id),
                    &slide.notes,
                    SearchResult::for_action(
                        NOTES_KIND,
                        Self::preview(&slide.notes),
                        actions::open_notes_address(&slide.id),
                    )
                    .with_subtitle(format!("Slide {number}"))
                    .with_icon(Icon::FileText),
                );
            }
        }
        for removed in self.indexed_ids.difference(&live) {
            self.catalog.remove(removed);
        }
        self.indexed_ids = live;
        self.deck = deck.clone();
    }

    fn upsert(
        &self,
        live: &mut BTreeSet<String>,
        id: String,
        text: impl AsRef<str>,
        result: SearchResult,
    ) {
        // The id and action are assembled together above. Refuse an invalid
        // record rather than offering a private navigation route if that ever
        // changes.
        if self.catalog.upsert(&id, text, result).is_ok() {
            live.insert(id);
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
        format!("{} {}", element.kind.label(), element.src)
    }

    fn resource_context(&self) -> SlidesResourceCtx {
        let slides = self
            .deck
            .slides
            .iter()
            .enumerate()
            .map(|(idx, slide)| SlideResource {
                idx,
                data: actions::SlideData::from_slide(slide),
                elements: slide
                    .z_sorted()
                    .into_iter()
                    .map(|element| ElementResource {
                        id: element.id.clone(),
                        kind: element.kind,
                        text: element.text.clone(),
                        z: element.z,
                    })
                    .collect(),
            })
            .collect();
        SlidesResourceCtx {
            deck_title: self.deck.title.clone(),
            slides,
            selection: None,
        }
    }
}

impl CatalogSearchProvider for SlideSearchProvider {
    fn provider_name(&self) -> &str {
        SLIDE_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<tilo_actions::Resource> {
        actions::resolve_resource(&self.resource_context(), &address.resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};
    use ui::search::SearchProvider;
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

    /// Slide search: exact text resources, bounded cursors, refresh, canonical addresses and resolution.
    #[test]
    fn slide_search_pages_refreshes_and_addresses_canonically() {
        // Case `provider_finds_slide_and_exact_text_resource`.
        {
            let deck = crate::model::seed_deck();
            let first = &deck.slides[0];
            let element = first
                .elements
                .iter()
                .find(|element| {
                    element.kind == ElementKind::Text && !element.text.trim().is_empty()
                })
                .unwrap();
            let word = element.text.split_whitespace().next().unwrap();
            let hits = SlideSearchProvider::new(&deck).search(word);
            assert!(hits.iter().any(|hit| hit.kind == SLIDE_KIND));
            assert_eq!(
                hits.iter()
                    .find(|hit| hit.kind == TEXT_KIND)
                    .unwrap()
                    .action_address(),
                Some(actions::open_text_address(&element.id))
            );
        }

        // Case `late_match_is_reachable_through_bounded_cursor_pages`.
        {
            let provider = SlideSearchProvider::new(&large_deck(
                4_200,
                "common opening nebula lantern finale",
            ));
            let mut cursor = None;
            let mut found = false;
            for _ in 0..240 {
                let page = provider.search_page(
                    &ui::search::SearchQuery::new(""),
                    cursor.as_ref(),
                    64,
                    64,
                );
                assert!(page.inspected <= 64);
                found |= page
                    .results
                    .iter()
                    .any(|hit| hit.kind == TEXT_KIND && hit.title.contains("nebula"));
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
            }
            assert!(found, "a late durable record is eventually reachable");
        }

        // Case `refresh_removes_stale_resources_instead_of_rebinding_them`.
        {
            let mut deck = large_deck(2, "needle");
            let stale_id = deck.slides[1].elements[0].id.clone();
            let mut provider = SlideSearchProvider::new(&deck);
            assert!(
                provider
                    .resolve_action_resource(&actions::open_text_address(&stale_id))
                    .is_some()
            );
            deck.slides.pop();
            provider.refresh(&deck);
            assert!(provider.search("needle").is_empty());
            assert!(
                provider
                    .resolve_action_resource(&actions::open_text_address(&stale_id))
                    .is_none()
            );
        }

        // Case `every_hit_carries_a_canonical_action_address`.
        {
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

        // Case `search_address_resolves_through_the_published_resource_tree`.
        {
            let mut app = crate::app::SlidesApp::demo(None);
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            let target = ui::resources::resources(&app, &doc)[0].children[2]
                .id
                .0
                .clone();
            let slide_id = target.strip_prefix("slides.slide:").unwrap();
            let hit = ui::search::capability::search_providers(&app)[0]
                .search("")
                .into_iter()
                .find(|hit| hit.action_address() == Some(actions::open_slide_address(slide_id)))
                .unwrap();
            let address = hit.action_address().unwrap();
            let resource = ui::resources::resource(&app, &doc, &address.resource).unwrap();
            let encoded = tilo_actions::door::encode(&[resource], &address, None).unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.current_slide(), 2);
        }
    }
}
