//! Path-free text presentation for native continuation, anchored in the CRDT.
//! This contains view state only. Resource contents retain their existing sync
//! route, and restoring a view neither changes focus nor grants input authority.

use automerge::{AutoCommit, Cursor, CursorPosition, ObjId, ReadDoc, TextEncoding};
use serde::{Deserialize, Serialize};

const MAX_CARETS: usize = 32;
const MAX_CURSOR_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextCaret {
    pub cursor: Vec<u8>,
    pub anchor: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPosition {
    pub resource_id: String,
    pub carets: Vec<TextCaret>,
    pub top: Vec<u8>,
    pub vertical_fraction: f64,
    pub horizontal_columns: f64,
}

/// Character-addressed presentation before encoding, or after resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTextPosition {
    pub carets: Vec<(usize, Option<usize>)>,
    pub top_character: usize,
    pub vertical_fraction: f64,
    pub horizontal_columns: f64,
}

impl TextPosition {
    /// Project a view without serializing text, local paths or pixel dimensions.
    pub fn capture(
        doc: &AutoCommit,
        object: &ObjId,
        resource_id: &str,
        view: &ResolvedTextPosition,
    ) -> Option<Self> {
        if !(1..=MAX_CARETS).contains(&view.carets.len()) {
            return None;
        }
        let text = doc.text(object).ok()?;
        let character_count = text.chars().count();
        let anchor = |character| {
            let offset = character_to_unit(&text, doc.text_encoding(), character)?;
            let position = if character == character_count {
                CursorPosition::End
            } else {
                CursorPosition::Index(offset)
            };
            Some(doc.get_cursor(object, position, None).ok()?.to_bytes())
        };
        let carets = view
            .carets
            .iter()
            .map(|(cursor, selection)| {
                Some(TextCaret {
                    cursor: anchor(*cursor)?,
                    anchor: match selection {
                        Some(position) => Some(anchor(*position)?),
                        None => None,
                    },
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let position = Self {
            resource_id: resource_id.to_owned(),
            carets,
            top: anchor(view.top_character)?,
            vertical_fraction: view.vertical_fraction,
            horizontal_columns: view.horizontal_columns,
        };
        position.valid().then_some(position)
    }

    /// Validate the portable envelope before it reaches an application.
    pub fn valid(&self) -> bool {
        let cursor_valid = |bytes: &[u8]| {
            !bytes.is_empty() && bytes.len() <= MAX_CURSOR_BYTES && Cursor::try_from(bytes).is_ok()
        };
        uuid::Uuid::parse_str(&self.resource_id).is_ok()
            && (1..=MAX_CARETS).contains(&self.carets.len())
            && self.carets.iter().all(|caret| {
                cursor_valid(&caret.cursor) && caret.anchor.as_deref().is_none_or(cursor_valid)
            })
            && cursor_valid(&self.top)
            && self.vertical_fraction.is_finite()
            && (0.0..1.0).contains(&self.vertical_fraction)
            && self.horizontal_columns.is_finite()
            && (0.0..=1_000_000.0).contains(&self.horizontal_columns)
    }

    /// Resolve anchors against the merged text. Missing operations defer the
    /// restore instead of guessing a position in a different document version.
    pub fn resolve(&self, doc: &AutoCommit, object: &ObjId) -> Option<ResolvedTextPosition> {
        if !self.valid() {
            return None;
        }
        let text = doc.text(object).ok()?;
        let resolve = |bytes: &[u8]| {
            let cursor = Cursor::try_from(bytes).ok()?;
            let unit = doc.get_cursor_position(object, &cursor, None).ok()?;
            unit_to_character(&text, doc.text_encoding(), unit)
        };
        let carets = self
            .carets
            .iter()
            .map(|caret| {
                Some((
                    resolve(&caret.cursor)?,
                    match &caret.anchor {
                        Some(anchor) => Some(resolve(anchor)?),
                        None => None,
                    },
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(ResolvedTextPosition {
            carets,
            top_character: resolve(&self.top)?,
            vertical_fraction: self.vertical_fraction,
            horizontal_columns: self.horizontal_columns,
        })
    }
}

fn character_units(character: char, encoding: TextEncoding) -> usize {
    match encoding {
        TextEncoding::Utf8CodeUnit => character.len_utf8(),
        TextEncoding::Utf16CodeUnit => character.len_utf16(),
        _ => 1,
    }
}

fn character_to_unit(text: &str, encoding: TextEncoding, position: usize) -> Option<usize> {
    if encoding == TextEncoding::GraphemeCluster {
        let byte = if position == text.chars().count() {
            text.len()
        } else {
            text.char_indices().nth(position)?.0
        };
        return if byte == text.len() {
            Some(crate::text_break::grapheme_indices(text).count())
        } else {
            crate::text_break::grapheme_indices(text).position(|(start, _)| start == byte)
        };
    }
    let mut characters = 0usize;
    let mut units = 0usize;
    for character in text.chars() {
        if characters == position {
            return Some(units);
        }
        characters = characters.checked_add(1)?;
        units = units.checked_add(character_units(character, encoding))?;
    }
    (characters == position).then_some(units)
}

fn unit_to_character(text: &str, encoding: TextEncoding, position: usize) -> Option<usize> {
    if encoding == TextEncoding::GraphemeCluster {
        return match crate::text_break::grapheme_indices(text).nth(position) {
            Some((byte, _)) => Some(text.get(..byte)?.chars().count()),
            None if position == crate::text_break::grapheme_indices(text).count() => {
                Some(text.chars().count())
            }
            None => None,
        };
    }
    let mut units = 0usize;
    let mut characters = 0usize;
    for character in text.chars() {
        if units == position {
            return Some(characters);
        }
        units = units.checked_add(character_units(character, encoding))?;
        characters = characters.checked_add(1)?;
        if units > position {
            return None;
        }
    }
    (units == position).then_some(characters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ObjType, ROOT, transaction::Transactable};
    use tilo_automerge::ApplyIncoming;

    #[test]
    fn anchored_selection_and_viewport_follow_concurrent_unicode_edits() {
        for encoding in [
            TextEncoding::UnicodeCodePoint,
            TextEncoding::Utf8CodeUnit,
            TextEncoding::Utf16CodeUnit,
            TextEncoding::GraphemeCluster,
        ] {
            let mut original = AutoCommit::new_with_encoding(encoding);
            let text = original.put_object(ROOT, "body", ObjType::Text).unwrap();
            original
                .splice_text(&text, 0, 0, "zero\né🐈éx\nlast")
                .unwrap();
            let view = ResolvedTextPosition {
                carets: vec![(9, Some(6)), (3, None)],
                top_character: 5,
                vertical_fraction: 0.25,
                horizontal_columns: 7.5,
            };
            let position = TextPosition::capture(
                &original,
                &text,
                "11111111-2222-4333-8444-555555555555",
                &view,
            )
            .unwrap();
            assert_eq!(position.resolve(&original, &text), Some(view));
            let mut first = original.fork();
            let mut second = original.fork();
            first.splice_text(&text, 0, 0, "α\n").unwrap();
            second
                .splice_text(&text, second.length(&text), 0, "\npeer")
                .unwrap();
            first.merge_incoming(&mut second).unwrap();
            second.merge_incoming(&mut first).unwrap();
            assert_eq!(first.text(&text).unwrap(), second.text(&text).unwrap());
            let resolved = position.resolve(&first, &text).unwrap();
            assert_eq!(resolved.carets, vec![(11, Some(8)), (5, None)]);
            assert_eq!(resolved.top_character, 7);
            assert_eq!(resolved.vertical_fraction, 0.25);
            assert_eq!(resolved.horizontal_columns, 7.5);
            assert_eq!(position.resolve(&second, &text), Some(resolved));
            let encoded = serde_json::to_string(&position).unwrap();
            assert!(!encoded.contains("zero") && !encoded.contains("last"));
        }
    }

    #[test]
    fn invalid_or_unavailable_view_anchors_are_refused() {
        let mut doc = AutoCommit::new();
        let text = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "abcdef").unwrap();
        let view = ResolvedTextPosition {
            carets: vec![(3, Some(1))],
            top_character: 0,
            vertical_fraction: 0.0,
            horizontal_columns: 0.0,
        };
        assert!(TextPosition::capture(&doc, &text, "/private/path", &view).is_none());
        let position =
            TextPosition::capture(&doc, &text, "11111111-2222-4333-8444-555555555555", &view)
                .unwrap();
        let mut invalid = position.clone();
        invalid.horizontal_columns = f64::NAN;
        assert!(!invalid.valid());
        invalid = position.clone();
        invalid.carets[0].cursor = vec![255];
        assert!(!invalid.valid());
        invalid = position.clone();
        invalid.carets = vec![invalid.carets[0].clone(); 33];
        assert!(!invalid.valid());
        let mut other = AutoCommit::new();
        let unrelated = other.put_object(ROOT, "body", ObjType::Text).unwrap();
        other.splice_text(&unrelated, 0, 0, "unrelated").unwrap();
        assert!(position.resolve(&other, &unrelated).is_none());
        let other_text = doc.put_object(ROOT, "other", ObjType::Text).unwrap();
        doc.splice_text(&other_text, 0, 0, "different sequence")
            .unwrap();
        let foreign_cursor = Cursor::try_from(position.carets[0].cursor.as_slice()).unwrap();
        assert!(matches!(
            doc.get_cursor_position(&other_text, &foreign_cursor, None),
            Err(automerge::AutomergeError::InvalidCursor(_))
        ));
        assert!(
            position.resolve(&doc, &other_text).is_none(),
            "an operation in the same document is not an anchor in another text"
        );
        doc.splice_text(&text, 1, 3, "").unwrap();
        let resolved = position.resolve(&doc, &text).unwrap();
        assert_eq!(
            resolved.carets,
            vec![(1, Some(1))],
            "deleted anchors resolve at the next surviving character"
        );
    }
}
