//! The deck as text: what `read` answers and what the deck's edits return,
//! so a caller without a screen sees the deck it produced without a
//! screenshot.
//!
//! One block per slide, in deck order, each line ending in the `{#id}` the
//! slide and element actions take:
//!
//! ```text
//! Slide 2 (title-content) {#6b1e…}
//! # Agenda {#91c2…}
//! Goals↵Timeline {#0a7f…}
//! [image] {#c4d0…}
//! Notes: Keep this short
//! ```
//!
//! The first text box is the title and the second the body (what `add-slide`
//! and `set-slide-text` fill); a line break inside a text box reads as `↵`.
//! Other elements appear by kind, and speaker notes close the block.
//!
//! [`facts`] states the same deck as data to check it against a request:
//! each slide's title, the rest of its text, how many pictures and shapes
//! it holds, and its notes, with no ids.

use std::fmt::Write as _;

use automerge::AutoCommit;
use ui::tilo_resource::Resource as _;

use crate::model::{Deck, ElementKind};

/// The outline of the deck in `doc`, or `None` when it holds no deck.
pub fn of_document(doc: &AutoCommit) -> Option<String> {
    crate::deck::Deck::read(doc)
        .ok()
        .map(|deck| of_deck(&crate::deck::mirror(&deck)))
}

pub fn of_deck(deck: &Deck) -> String {
    let mut out = format!("{} ({} slides)\n", deck.title.trim(), deck.slides.len());
    for (index, slide) in deck.slides.iter().enumerate() {
        let _ = writeln!(
            out,
            "\nSlide {} ({}) {{#{}}}",
            index + 1,
            slide.layout.as_str(),
            slide.id
        );
        let mut titled = false;
        for element in &slide.elements {
            if element.kind != ElementKind::Text {
                let _ = writeln!(out, "[{}] {{#{}}}", element.kind.as_str(), element.id);
                continue;
            }
            let text = element.text.trim();
            if text.is_empty() {
                continue;
            }
            if !titled {
                titled = true;
                out.push_str("# ");
            }
            out.push_str(&text.replace('\n', "↵"));
            let _ = writeln!(out, " {{#{}}}", element.id);
        }
        let notes = slide.notes.trim();
        if !notes.is_empty() {
            out.push_str("Notes: ");
            out.push_str(&notes.replace('\n', "↵"));
            out.push('\n');
        }
    }
    out
}

/// The deck's facts (`#[resource(facts = path)]`): its title and each
/// slide in order, as [`facts_of_deck`] states them.
///
/// # Errors
/// Never; the signature is the one the resource option takes.
pub fn facts(deck: &crate::deck::Deck) -> Result<serde_json::Value, String> {
    Ok(facts_of_deck(&crate::deck::mirror(deck)))
}

/// `deck` as a brief would state it: per slide, the first text box as its
/// title, the other text boxes' words, the pictures and shapes it holds and
/// its speaker notes.
#[must_use]
pub fn facts_of_deck(deck: &Deck) -> serde_json::Value {
    let slides: Vec<serde_json::Value> = deck
        .slides
        .iter()
        .map(|slide| {
            let mut texts = slide
                .elements
                .iter()
                .filter(|element| element.kind == ElementKind::Text)
                .map(|element| element.text.trim().to_owned())
                .filter(|text| !text.is_empty());
            let title = texts.next().unwrap_or_default();
            let count = |kind: ElementKind| {
                slide
                    .elements
                    .iter()
                    .filter(|element| element.kind == kind)
                    .count()
            };
            let shapes = slide
                .elements
                .iter()
                .filter(|element| !matches!(element.kind, ElementKind::Text | ElementKind::Image))
                .count();
            serde_json::json!({
                "layout": slide.layout.as_str(),
                "title": title,
                "text": texts.collect::<Vec<_>>(),
                "pictures": count(ElementKind::Image),
                "shapes": shapes,
                "notes": slide.notes.trim(),
            })
        })
        .collect();
    serde_json::json!({
        "title": deck.title.trim(),
        "slides": slides,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::authored_slide;
    use crate::model::Element;

    #[test]
    fn a_deck_reads_in_order_as_an_outline_with_ids_and_as_facts_without() {
        let mut cover = authored_slide(
            Some("Product Launch"),
            Some("October 2026"),
            Some(crate::model::Layout::Title),
        );
        cover.notes = "Welcome everyone".into();
        let mut agenda = authored_slide(Some("Agenda"), Some("Goals\nTimeline"), None);
        agenda
            .elements
            .push(Element::shape(ElementKind::Image, 0.0, 0.0, 10.0, 10.0));
        let deck = Deck {
            title: "Launch".into(),
            palette: crate::model::PaletteKind::ALL[0],
            look: crate::model::DeckLook::default(),
            slides: vec![cover.clone(), agenda.clone()],
        };
        let ids = |slide: &crate::model::Slide| -> Vec<String> {
            slide
                .elements
                .iter()
                .map(|element| element.id.clone())
                .collect()
        };
        let (c, a) = (ids(&cover), ids(&agenda));
        let expected = format!(
            "Launch (2 slides)\n\
             \nSlide 1 (title) {{#{}}}\n# Product Launch {{#{}}}\nOctober 2026 {{#{}}}\nNotes: Welcome everyone\n\
             \nSlide 2 (title-content) {{#{}}}\n# Agenda {{#{}}}\n[rect] {{#{}}}\nGoals↵Timeline {{#{}}}\n[image] {{#{}}}\n",
            cover.id, c[0], c[1], agenda.id, a[0], a[1], a[2], a[3]
        );
        assert_eq!(of_deck(&deck), expected);
        assert_eq!(
            facts_of_deck(&deck),
            serde_json::json!({
                "title": "Launch",
                "slides": [
                    {
                        "layout": "title", "title": "Product Launch", "text": ["October 2026"],
                        "pictures": 0, "shapes": 0, "notes": "Welcome everyone",
                    },
                    {
                        "layout": "title-content", "title": "Agenda", "text": ["Goals\nTimeline"],
                        "pictures": 1, "shapes": 1, "notes": "",
                    },
                ],
            })
        );
    }
}
