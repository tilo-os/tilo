//! Elements as they travel on the clipboard.
//!
//! Copy takes the element the person chose, whole — its box, its style and,
//! for a text box, its words as written — and puts it on the clipboard as one
//! JSON object tagged with its own name, so another app's clipboard text is
//! never mistaken for a slide's. Paste places a copy on the slide shown, under
//! a fresh id and moved a little so it does not hide the element it came from:
//! that is how a shape moves from one slide to another.
//!
//! The element it copies is the deck's, so what is pasted keeps its picture,
//! its colours and its words exactly.

use serde::{Deserialize, Serialize};

use crate::deck::ElementData;

/// The tag a slide clipboard carries: another app's JSON is not this.
const TAG: &str = "tilo-slides";

/// How far a pasted element is moved from the one it was copied from, in
/// slide units: enough to see both, small enough to stay where it was.
pub(crate) const PASTE_OFFSET: f64 = 24.0;

/// Elements as they travel on the clipboard.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Clip {
    /// Tells a slide clipboard apart from any other JSON.
    #[serde(rename = "tilo-slides")]
    pub(crate) elements: Vec<ElementData>,
}

impl Clip {
    /// `element` as the clipboard carries it.
    pub(crate) fn of(element: &crate::model::Element) -> Self {
        Self {
            elements: vec![ElementData::of(element)],
        }
    }

    /// The clipboard text.
    pub(crate) fn text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// What the clipboard holds, if it holds slide elements at all.
    pub(crate) fn read(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        value.get(TAG)?;
        let clip: Self = serde_json::from_value(value).ok()?;
        (!clip.elements.is_empty()).then_some(clip)
    }

    /// The elements moved by [`PASTE_OFFSET`] and given fresh ids by `fresh`:
    /// the copies land beside what they came from, as themselves.
    pub(crate) fn pasted(&self, fresh: impl Fn() -> String) -> Vec<ElementData> {
        self.elements
            .iter()
            .map(|element| {
                let mut copy = element.clone();
                copy.id = fresh();
                copy.x += PASTE_OFFSET;
                copy.y += PASTE_OFFSET;
                copy
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element() -> crate::model::Element {
        crate::model::Element::text("Copied words", 100.0, 120.0, 400.0, 80.0, 40.0)
    }

    #[test]
    fn what_is_copied_is_what_is_pasted_beside_it() {
        let copied = element();
        let clip = Clip::of(&copied);
        let text = clip.text();
        let read = Clip::read(&text).expect("the clipboard holds a slide element");
        let pasted = read.pasted(|| "fresh-id".to_string());
        assert_eq!(pasted.len(), 1);
        let paste = &pasted[0];
        assert_eq!(paste.id, "fresh-id", "a copy is an element of its own");
        assert_eq!(paste.x, copied.x + PASTE_OFFSET);
        assert_eq!(paste.y, copied.y + PASTE_OFFSET);
        assert_eq!(paste.w, copied.w, "the box travels whole");
        assert_eq!(
            paste.kind, copied.kind,
            "a text box stays a text box, with its words"
        );
        assert!(!paste.blocks.is_empty(), "its words came with it");
    }

    #[test]
    fn another_apps_clipboard_is_not_a_slide() {
        assert!(Clip::read("Copied words from somewhere else").is_none());
        assert!(Clip::read(r#"{"tilo-canvas":{"marks":[]}}"#).is_none());
        assert!(Clip::read(r#"{"tilo-slides":{"elements":[]}}"#).is_none());
    }
}
