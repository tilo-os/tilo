//! What the deck offers search: every slide, element, text box and set of
//! speaker notes, each hit showing its slide in the editor.

use ui::tilo_resource::SearchEntry;

use crate::deck::{Deck, Element, Slide, blocks_of};
use crate::model::ElementKind;

/// The search entries of the deck as it stands.
pub fn entries(deck: &Deck) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for (index, (id, slide)) in deck.slides.iter().enumerate() {
        let anchor = crate::actions::slide_resource_id(id.as_str());
        let number = index + 1;
        let title = slide_title(slide, index);
        let place = format!("slide {number}, {title}");
        entries
            .push(SearchEntry::new(title.clone(), slide_haystack(slide)).anchored(anchor.clone()));
        for element in slide.elements.values() {
            entries.push(
                SearchEntry::new(
                    format!("{} in {place}", element.kind.label()),
                    element_haystack(element),
                )
                .anchored(anchor.clone()),
            );
            let text = words(element);
            if element.kind == ElementKind::Text && !text.trim().is_empty() {
                entries.push(SearchEntry::new(preview(&text), text).anchored(anchor.clone()));
            }
        }
        if !slide.notes.trim().is_empty() {
            entries.push(
                SearchEntry::new(
                    format!("{} in {place}", preview(&slide.notes)),
                    slide.notes.clone(),
                )
                .anchored(anchor.clone()),
            );
        }
    }
    entries
}

/// A text box's words, a line per paragraph.
fn words(element: &Element) -> String {
    blocks_of(&element.body)
        .into_iter()
        .map(|block| block.text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A slide's first line of text, else its number.
fn slide_title(slide: &Slide, index: usize) -> String {
    slide
        .elements
        .values()
        .filter(|element| element.kind == ElementKind::Text)
        .map(words)
        .find(|text| !text.trim().is_empty())
        .map_or_else(
            || format!("Slide {}", index + 1),
            |text| text.trim().lines().next().unwrap_or_default().to_string(),
        )
}

fn slide_haystack(slide: &Slide) -> String {
    let mut text = slide
        .elements
        .values()
        .filter(|element| element.kind == ElementKind::Text)
        .map(words)
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text.push_str(&slide.notes);
    text
}

fn element_haystack(element: &Element) -> String {
    format!("{} {}", element.kind.label(), element.src.0)
}

fn preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 72 {
        collapsed.chars().take(69).collect::<String>() + "…"
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::seed_deck;
    use ui::framework::testing::TestApp;

    fn seeded() -> TestApp<crate::SlidesShell> {
        let mut app = TestApp::<crate::SlidesShell>::new().unwrap();
        app.invoke(
            ui::tilo_resource::Invoke::apply(&crate::actions::replace_deck_action(&seed_deck()))
                .unwrap(),
        )
        .unwrap();
        app
    }

    /// Text, slides and notes are found, each hit anchored at its slide.
    #[test]
    fn hits_are_anchored_at_their_slide() {
        let app = seeded();
        let deck = app.state();
        let found = entries(&deck);
        let (first_id, first) = deck.slides.iter().next().unwrap();
        let anchor = crate::actions::slide_resource_id(first_id.as_str());
        let element = first
            .elements
            .values()
            .find(|element| element.kind == ElementKind::Text && !words(element).trim().is_empty())
            .unwrap();
        let text = found
            .iter()
            .find(|entry| entry.text == words(element))
            .expect("the words are an entry");
        assert_eq!(text.anchor.as_deref(), Some(anchor.as_str()));
        let slide = found
            .iter()
            .find(|entry| {
                entry.anchor.as_deref() == Some(anchor.as_str())
                    && entry.text.contains(&first.notes)
            })
            .expect("a slide is found by its notes");
        assert!(!slide.title.is_empty());
        assert!(
            found.iter().all(|entry| entry.anchor.is_some()),
            "every hit shows what it found"
        );
    }

    /// A hit opens through the deck's `open` and shows its slide in the
    /// editor.
    #[test]
    fn a_hit_reveals_its_slide_in_the_editor() {
        let mut app = seeded();
        let slide = app.state().slides.ids().nth(2).unwrap().clone();
        let anchor = crate::actions::slide_resource_id(slide.as_str());
        let hit = entries(&app.state())
            .into_iter()
            .find(|entry| entry.anchor.as_deref() == Some(anchor.as_str()))
            .unwrap();
        app.call("open", serde_json::json!({ "anchor": hit.anchor.unwrap() }))
            .unwrap();
        let current = app
            .with_surface::<crate::SlidesApp, _>(|editor| editor.current_slide())
            .unwrap();
        assert_eq!(current, 2);
    }
}
