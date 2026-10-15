//! What a person has selected, carried to whoever is asked to act on it.
//!
//! A person selects a sentence in a document, a range of cells or a row in a
//! list, and asks an agent about it. The agent must act at exactly that place
//! through the actions the resource publishes, so the selection travels in
//! the resource's own terms: a [`SelectionPlace`] is the same value an action
//! takes as its place argument (`{"item": id, "start": n, "end": n}`, in
//! characters), an [`InteractionAnchor`] names selections that are not text
//! (cells, objects, rows), and a short excerpt lets a reader see what was
//! selected without opening the resource. None of it grants authority: the
//! agent still reaches the resource only through permission-filtered actions.

use serde::{Deserialize, Serialize};

use crate::InteractionAnchor;

/// The most characters an excerpt keeps. A selection is context for a
/// request, not its payload: a longer one is cut and ends in `…`.
pub const MAX_EXCERPT_CHARS: usize = 1000;
/// The most bytes an excerpt may take at a trust boundary.
pub const MAX_EXCERPT_BYTES: usize = 4 * 1024;
/// The most bytes a place's item id may take.
pub const MAX_PLACE_ITEM_BYTES: usize = 512;
/// The most bytes an interaction anchor may take, encoded.
pub const MAX_SELECTION_ANCHOR_BYTES: usize = 8 * 1024;
/// The most items a selection of several items (files, rows, cards) names
/// one by one; the excerpt counts the rest.
pub const MAX_SELECTED_ITEMS: usize = 64;

/// Where a selection is inside a resource: an item, a caret or a character
/// range inside one item. Its JSON form is exactly what an action's place
/// parameter takes (`tilo_resource::Place`), so an agent hands it back
/// unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionPlace {
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<usize>,
}

impl SelectionPlace {
    /// The whole of `item`.
    #[must_use]
    pub fn item(item: impl Into<String>) -> Self {
        Self {
            item: item.into(),
            at: None,
            start: None,
            end: None,
        }
    }

    /// A caret at character `at` of `item`.
    #[must_use]
    pub fn caret(item: impl Into<String>, at: usize) -> Self {
        Self {
            at: Some(at),
            ..Self::item(item)
        }
    }

    /// Characters `start..end` of `item`; the bounds are put in order.
    #[must_use]
    pub fn range(item: impl Into<String>, start: usize, end: usize) -> Self {
        Self {
            start: Some(start.min(end)),
            end: Some(start.max(end)),
            ..Self::item(item)
        }
    }

    /// Whether this is a well-formed place, the shape an action's place
    /// parameter accepts.
    ///
    /// # Errors
    ///
    /// Names what is wrong: an empty or oversized item, or offsets that are
    /// neither one caret nor one ordered range.
    pub fn validate(&self) -> Result<(), String> {
        if self.item.is_empty() || self.item.len() > MAX_PLACE_ITEM_BYTES {
            return Err("a selection place names one item of bounded length".into());
        }
        if self.item.chars().any(char::is_control) {
            return Err("a selection place item contains control characters".into());
        }
        match (self.at, self.start, self.end) {
            (None | Some(_), None, None) => Ok(()),
            (None, Some(start), Some(end)) if start <= end => Ok(()),
            _ => Err("a selection place is an item, a caret or an ordered range".into()),
        }
    }
}

/// What was selected: where (a place, an app-native anchor, or both) and a
/// short excerpt of it. Every part is optional; an empty anchor says
/// nothing and is dropped by [`Self::non_empty`].
///
/// Encoded as a JSON object in JSON, and as that object's JSON text in a
/// binary format, because an [`InteractionAnchor`] reads only from JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionAnchor {
    pub place: Option<SelectionPlace>,
    /// Every item of a selection of several (files in a folder, rows of a
    /// list), each the whole of one item, in the order shown. Empty for a
    /// selection inside one item, which [`Self::place`] names.
    pub items: Vec<SelectionPlace>,
    pub interaction: Option<InteractionAnchor>,
    pub excerpt: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionAnchorWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    place: Option<SelectionPlace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    items: Vec<SelectionPlace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interaction: Option<InteractionAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    excerpt: Option<String>,
}

impl Serialize for SelectionAnchor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = SelectionAnchorWire {
            place: self.place.clone(),
            items: self.items.clone(),
            interaction: self.interaction.clone(),
            excerpt: self.excerpt.clone(),
        };
        if serializer.is_human_readable() {
            wire.serialize(serializer)
        } else {
            serde_json::to_string(&wire)
                .map_err(serde::ser::Error::custom)?
                .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for SelectionAnchor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = if deserializer.is_human_readable() {
            SelectionAnchorWire::deserialize(deserializer)?
        } else {
            let text = String::deserialize(deserializer)?;
            serde_json::from_str(&text).map_err(serde::de::Error::custom)?
        };
        Ok(Self {
            place: wire.place,
            items: wire.items,
            interaction: wire.interaction,
            excerpt: wire.excerpt,
        })
    }
}

impl SelectionAnchor {
    /// An anchor of `excerpt` alone: what is known of a selection in an app
    /// that publishes no resources, read from the primary selection.
    #[must_use]
    pub fn of_text(excerpt: &str) -> Self {
        Self {
            excerpt: excerpt_of(excerpt),
            ..Self::default()
        }
    }

    /// A selection of several whole items, each `(id, label)` in the order
    /// shown: every item named as a place (at most [`MAX_SELECTED_ITEMS`]),
    /// and an excerpt listing their labels, counting any beyond that.
    #[must_use]
    pub fn of_items<I, L>(items: impl IntoIterator<Item = (I, L)>) -> Self
    where
        I: Into<String>,
        L: AsRef<str>,
    {
        let mut places = Vec::new();
        let mut labels = Vec::new();
        let mut total = 0_usize;
        for (id, label) in items {
            total = total.saturating_add(1);
            if places.len() == MAX_SELECTED_ITEMS {
                continue;
            }
            let id = id.into();
            let label = label.as_ref().trim();
            labels.push(if label.is_empty() {
                id.clone()
            } else {
                label.to_owned()
            });
            places.push(SelectionPlace::item(id));
        }
        // List names while they fit the excerpt, leaving room to count the
        // rest, so the excerpt never ends cut mid-name.
        let budget = MAX_EXCERPT_CHARS.saturating_sub(32);
        let mut listed = String::new();
        let mut shown = 0_usize;
        for label in &labels {
            let line = format!("- {label}");
            let used = listed
                .chars()
                .count()
                .saturating_add(line.chars().count())
                .saturating_add(1);
            if used > budget {
                break;
            }
            if !listed.is_empty() {
                listed.push('\n');
            }
            listed.push_str(&line);
            shown = shown.saturating_add(1);
        }
        let more = total.saturating_sub(shown);
        if more > 0 {
            if !listed.is_empty() {
                listed.push('\n');
            }
            listed.push_str("- and ");
            listed.push_str(&more.to_string());
            listed.push_str(" more");
        }
        Self {
            items: places,
            excerpt: excerpt_of(&listed),
            ..Self::default()
        }
    }

    /// The same anchor with `excerpt` as its excerpt, bounded.
    #[must_use]
    pub fn with_excerpt(mut self, excerpt: &str) -> Self {
        self.excerpt = excerpt_of(excerpt);
        self
    }

    /// The same anchor at `place`.
    #[must_use]
    pub fn at(mut self, place: SelectionPlace) -> Self {
        self.place = Some(place);
        self
    }

    /// The same anchor noting the whole of a selection that spans several
    /// blocks of a text, `ranges` in document order, of which [`Self::place`]
    /// is one block's part (an action's place names one block). One range
    /// says nothing more than the place, so it is left out; a long
    /// selection keeps its first [`crate::interaction::MAX_ANCHOR_ITEMS`].
    #[must_use]
    pub fn spanning(mut self, region: &str, mut ranges: Vec<crate::InteractionTextRange>) -> Self {
        if ranges.len() > 1 {
            ranges.truncate(crate::interaction::MAX_ANCHOR_ITEMS);
            self.interaction = Some(InteractionAnchor::Text {
                region: region.to_owned(),
                ranges,
            });
        }
        self
    }

    /// The block ranges of a selection spanning several blocks
    /// ([`Self::spanning`]).
    #[must_use]
    pub fn spanned_ranges(&self) -> Option<&[crate::InteractionTextRange]> {
        match &self.interaction {
            Some(InteractionAnchor::Text { ranges, .. }) if ranges.len() > 1 => Some(ranges),
            _ => None,
        }
    }

    /// Whether the anchor says nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.place.is_none()
            && self.items.is_empty()
            && self.interaction.is_none()
            && self.excerpt.is_none()
    }

    /// The anchor, unless it says nothing.
    #[must_use]
    pub fn non_empty(self) -> Option<Self> {
        (!self.is_empty()).then_some(self)
    }

    /// Whether the anchor is within bounds, so a receiver may keep it.
    ///
    /// # Errors
    ///
    /// Names the first oversized or malformed part.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(place) = &self.place {
            place.validate()?;
        }
        if self.items.len() > MAX_SELECTED_ITEMS {
            return Err("a selection names too many items".into());
        }
        for item in &self.items {
            item.validate()?;
        }
        if let Some(excerpt) = &self.excerpt
            && (excerpt.len() > MAX_EXCERPT_BYTES
                || excerpt.chars().count() > MAX_EXCERPT_CHARS + 1
                || excerpt
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t')))
        {
            return Err("a selection excerpt is oversized or contains control characters".into());
        }
        if let Some(interaction) = &self.interaction {
            let encoded = serde_json::to_vec(interaction).map_err(|error| error.to_string())?;
            if encoded.len() > MAX_SELECTION_ANCHOR_BYTES {
                return Err("a selection's interaction anchor is oversized".into());
            }
        }
        Ok(())
    }
}

/// What a request is about: the resource a person had selected something in,
/// and the selection. Only the subject of the request: it carries no
/// authority, so a run about a selection may act anywhere its starter and
/// its agent may (other resources, other apps), as any task may.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionSubject {
    /// The application the resource is in, by package id.
    pub app_id: String,
    pub app_label: String,
    pub resource: crate::ActionResource,
    pub anchor: SelectionAnchor,
}

impl SelectionSubject {
    /// Whether the subject is within bounds, so a receiver may keep it.
    ///
    /// # Errors
    ///
    /// Names the first oversized or malformed part.
    pub fn validate(&self) -> Result<(), String> {
        for (value, maximum, name) in [
            (self.app_id.as_str(), 256, "app"),
            (self.app_label.as_str(), 512, "app label"),
            (self.resource.id.as_str(), 2048, "resource id"),
            (self.resource.kind.as_str(), 128, "resource kind"),
            (self.resource.label.as_str(), 512, "resource label"),
        ] {
            if value.len() > maximum || value.chars().any(char::is_control) {
                return Err(format!("a selection's {name} is oversized or malformed"));
            }
        }
        if self.resource.id.trim().is_empty() || self.app_id.trim().is_empty() {
            return Err("a selection names its application and resource".into());
        }
        if self.anchor.is_empty() {
            return Err("a selection says what was selected".into());
        }
        self.anchor.validate()
    }
}

/// `text` as a selection excerpt: trimmed, at most [`MAX_EXCERPT_CHARS`]
/// characters (a longer one is cut and ends in `…`), without control
/// characters other than line breaks and tabs. `None` when nothing is left.
#[must_use]
pub fn excerpt_of(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut kept = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'));
    let mut excerpt: String = kept.by_ref().take(MAX_EXCERPT_CHARS).collect();
    let mut cut = kept.next().is_some();
    while excerpt.len() > MAX_EXCERPT_BYTES.saturating_sub('…'.len_utf8()) {
        excerpt.pop();
        cut = true;
    }
    if cut {
        excerpt.push('…');
    }
    Some(excerpt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> SelectionAnchor {
        SelectionAnchor {
            place: Some(SelectionPlace::range("block-1", 4, 9)),
            items: Vec::new(),
            interaction: Some(InteractionAnchor::Objects {
                ids: vec!["block-1".into()],
            }),
            excerpt: Some("quick".into()),
        }
    }

    #[test]
    fn a_selection_of_several_items_names_each_and_lists_them() {
        let anchor = SelectionAnchor::of_items(
            (0..MAX_SELECTED_ITEMS + 2).map(|n| (format!("file:{n}"), format!("report-{n}.pdf"))),
        );
        assert_eq!(anchor.items.len(), MAX_SELECTED_ITEMS);
        assert_eq!(anchor.items[1], SelectionPlace::item("file:1"));
        let excerpt = anchor.excerpt.as_deref().unwrap();
        assert!(excerpt.starts_with("- report-0.pdf\n- report-1.pdf\n"));
        assert!(excerpt.ends_with(" more"), "{excerpt}");
        assert!(!excerpt.contains('…'), "the list is never cut mid-name");
        let listed = excerpt
            .lines()
            .filter(|line| line.starts_with("- report-"))
            .count();
        assert!(
            excerpt.ends_with(&format!("- and {} more", MAX_SELECTED_ITEMS + 2 - listed)),
            "{excerpt}"
        );
        assert!(anchor.validate().is_ok());
        let json = serde_json::to_value(&anchor).unwrap();
        assert_eq!(json["items"][0], serde_json::json!({"item": "file:0"}));
        assert_eq!(
            serde_json::from_value::<SelectionAnchor>(json).unwrap(),
            anchor
        );
        let encoded = postcard::to_allocvec(&anchor).unwrap();
        assert_eq!(
            postcard::from_bytes::<SelectionAnchor>(&encoded).unwrap(),
            anchor
        );
        assert!(
            serde_json::to_value(SelectionAnchor::of_text("x"))
                .unwrap()
                .get("items")
                .is_none(),
            "a selection inside one item writes what it wrote before"
        );
        let too_many = SelectionAnchor {
            items: vec![SelectionPlace::item("f"); MAX_SELECTED_ITEMS + 1],
            ..SelectionAnchor::default()
        };
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn place_travels_as_the_action_place_parameter_does() {
        let place = SelectionPlace::range("block-1", 9, 4);
        assert_eq!(
            serde_json::to_value(&place).unwrap(),
            serde_json::json!({"item": "block-1", "start": 4, "end": 9})
        );
        assert_eq!(
            serde_json::to_value(SelectionPlace::caret("b", 2)).unwrap(),
            serde_json::json!({"item": "b", "at": 2})
        );
        assert!(
            crate::validate_json_schema(
                &serde_json::to_value(&place).unwrap(),
                &crate::place_schema()
            )
            .is_ok()
        );
    }

    #[test]
    fn malformed_places_are_refused() {
        assert!(SelectionPlace::item("").validate().is_err());
        let both = SelectionPlace {
            at: Some(1),
            ..SelectionPlace::range("b", 1, 2)
        };
        assert!(both.validate().is_err());
        let open = SelectionPlace {
            start: Some(3),
            ..SelectionPlace::item("b")
        };
        assert!(open.validate().is_err());
        let backwards = SelectionPlace {
            start: Some(3),
            end: Some(1),
            ..SelectionPlace::item("b")
        };
        assert!(backwards.validate().is_err());
        assert!(
            serde_json::from_value::<SelectionPlace>(serde_json::json!({"item":"b","x":1}))
                .is_err()
        );
    }

    #[test]
    fn anchor_round_trips_through_json_and_postcard() {
        let anchor = anchor();
        let json = serde_json::to_value(&anchor).unwrap();
        assert_eq!(json["place"]["start"], 4);
        assert_eq!(json["interaction"]["kind"], "objects");
        assert_eq!(
            serde_json::from_value::<SelectionAnchor>(json).unwrap(),
            anchor
        );
        let bytes = postcard::to_allocvec(&anchor).unwrap();
        assert_eq!(
            postcard::from_bytes::<SelectionAnchor>(&bytes).unwrap(),
            anchor
        );
    }

    #[test]
    fn absent_parts_are_omitted_and_older_readers_default_them() {
        let only_text = SelectionAnchor::of_text("  hello  ");
        assert_eq!(
            serde_json::to_value(&only_text).unwrap(),
            serde_json::json!({"excerpt": "hello"})
        );
        assert_eq!(
            serde_json::from_value::<SelectionAnchor>(serde_json::json!({})).unwrap(),
            SelectionAnchor::default()
        );
        assert!(SelectionAnchor::of_text("   ").non_empty().is_none());
    }

    #[test]
    fn excerpts_are_bounded_and_cut_visibly() {
        let long = "word ".repeat(400);
        let excerpt = excerpt_of(&long).unwrap();
        assert_eq!(excerpt.chars().count(), MAX_EXCERPT_CHARS + 1);
        assert!(excerpt.ends_with('…'));
        assert!(SelectionAnchor::of_text(&long).validate().is_ok());
        let wide = "界".repeat(MAX_EXCERPT_CHARS * 2);
        let excerpt = excerpt_of(&wide).unwrap();
        assert!(excerpt.len() <= MAX_EXCERPT_BYTES);
        assert!(excerpt.ends_with('…'));
        assert_eq!(excerpt_of("a\u{7}b\nc").as_deref(), Some("ab\nc"));
        let forged = SelectionAnchor {
            excerpt: Some("x".repeat(MAX_EXCERPT_BYTES + 1)),
            ..SelectionAnchor::default()
        };
        assert!(forged.validate().is_err());
    }
}
