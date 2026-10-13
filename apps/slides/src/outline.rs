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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::authored_slide;
    use crate::model::Element;

    #[test]
    fn titles_bodies_shapes_and_notes_read_in_deck_order_with_their_ids() {
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
    }
}
