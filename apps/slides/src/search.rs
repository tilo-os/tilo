//! What the deck offers search: every slide, element, text box and set of
//! speaker notes, each hit revealing it in the editor through the deck's
//! published `open` operation.

use ui::tilo_resource::SearchEntry;

use crate::actions::{self, ElementData, SlideData, SlidesResourceCtx};
use crate::model::ElementKind;

/// The search entries of the deck as it stands.
pub fn entries(state: &SlidesResourceCtx) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for (index, slide) in state.slides.iter().enumerate() {
        let slide = &slide.data;
        let number = index + 1;
        let title = slide_title(slide, index);
        let place = format!("{title} · Slide {number}");
        entries.push(
            SearchEntry::new(title.clone(), slide_haystack(slide))
                .anchored(actions::slide_resource_id(&slide.id))
                .invoking(actions::open_slide_address(&slide.id)),
        );
        for element in &slide.elements {
            let kind = ElementKind::from_str(&element.kind);
            entries.push(
                SearchEntry::new(
                    format!("{} · {place}", kind.label()),
                    element_haystack(kind, element),
                )
                .anchored(actions::element_resource_id(&element.id))
                .invoking(actions::open_element_address(&element.id)),
            );
            if kind == ElementKind::Text && !element.text.trim().is_empty() {
                entries.push(
                    SearchEntry::new(preview(&element.text), element.text.clone())
                        .anchored(actions::text_resource_id(&element.id))
                        .invoking(actions::open_text_address(&element.id)),
                );
            }
        }
        if !slide.notes.trim().is_empty() {
            entries.push(
                SearchEntry::new(
                    format!("{} · {place}", preview(&slide.notes)),
                    slide.notes.clone(),
                )
                .anchored(actions::notes_resource_id(&slide.id))
                .invoking(actions::open_notes_address(&slide.id)),
            );
        }
    }
    entries
}

/// A slide's first line of text, else its number.
fn slide_title(slide: &SlideData, index: usize) -> String {
    slide
        .elements
        .iter()
        .filter(|element| ElementKind::from_str(&element.kind) == ElementKind::Text)
        .map(|element| element.text.trim())
        .find(|text| !text.is_empty())
        .map_or_else(
            || format!("Slide {}", index + 1),
            |text| text.lines().next().unwrap_or(text).to_string(),
        )
}

fn slide_haystack(slide: &SlideData) -> String {
    let mut text = slide
        .elements
        .iter()
        .filter(|element| ElementKind::from_str(&element.kind) == ElementKind::Text)
        .map(|element| element.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text.push_str(&slide.notes);
    text
}

fn element_haystack(kind: ElementKind, element: &ElementData) -> String {
    format!("{} {}", kind.label(), element.src)
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

    /// Text, slides and notes are found, each hit naming the operation that
    /// reveals it.
    #[test]
    fn hits_name_the_operation_that_reveals_them() {
        let deck = seed_deck();
        let found = entries(&SlidesResourceCtx::from_deck(&deck));
        let first = &deck.slides[0];
        let element = first
            .elements
            .iter()
            .find(|element| element.kind == ElementKind::Text && !element.text.trim().is_empty())
            .unwrap();
        let text = found
            .iter()
            .find(|entry| entry.anchor.as_deref() == Some(&actions::text_resource_id(&element.id)))
            .expect("the words are an entry");
        assert_eq!(text.action, Some(actions::open_text_address(&element.id)));
        assert_eq!(text.text, element.text);
        let slide = found
            .iter()
            .find(|entry| entry.anchor.as_deref() == Some(&actions::slide_resource_id(&first.id)))
            .expect("the slide is an entry");
        assert!(
            slide.text.contains(&first.notes),
            "a slide is found by its notes"
        );
        assert_eq!(slide.action, Some(actions::open_slide_address(&first.id)));
        let notes = found
            .iter()
            .find(|entry| entry.anchor.as_deref() == Some(&actions::notes_resource_id(&first.id)))
            .expect("the notes are an entry");
        assert_eq!(notes.action, Some(actions::open_notes_address(&first.id)));
        assert!(
            found.iter().all(|entry| entry.action.is_some()),
            "every hit reveals what it found"
        );
    }

    /// A hit's operation resolves through the published tree and reveals the
    /// slide in the editor.
    #[test]
    fn a_hit_reveals_its_slide_in_the_editor() {
        let mut app = ui::framework::testing::TestApp::<crate::SlidesShell>::new().unwrap();
        app.invoke(
            ui::tilo_resource::Invoke::apply(&actions::replace_deck_action(&seed_deck())).unwrap(),
        )
        .unwrap();
        let slide_id = app.state().slides[2].data.id.clone();
        let hit = entries(&app.state())
            .into_iter()
            .find(|entry| entry.action == Some(actions::open_slide_address(&slide_id)))
            .unwrap();
        let instance_id = app.harness().component_instance_id().unwrap();
        app.harness()
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id,
                    address: hit.action.unwrap(),
                },
                &serde_json::Map::new(),
            )
            .unwrap();
        let current = app
            .with_surface::<crate::SlidesApp, _>(|editor| editor.current_slide())
            .unwrap();
        assert_eq!(current, 2);
    }
}
