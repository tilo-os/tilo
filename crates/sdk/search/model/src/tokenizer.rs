//! How text is cut for search: folded and marked at value and word
//! boundaries ([`marked`]), and into ICU words ([`words`]). The engine's
//! tokenizers index these, and the search layer reads small indexes with
//! them directly, so both see the same text.

use std::sync::LazyLock;

use icu_segmenter::options::WordBreakInvariantOptions;
use icu_segmenter::{WordSegmenter, WordSegmenterBorrowed};

use crate::fold::fold_char;

/// Scalars per indexed gram.
pub const GRAM_WIDTH: usize = 3;

/// Marks the start of a field value.
pub const VALUE_START: char = '\u{1}';
/// Marks the start of a word.
pub const WORD_START: char = '\u{2}';
/// Marks the end of a field value. Values end with two, so every scalar of
/// the value starts a full gram.
pub const VALUE_END: char = '\u{3}';
/// Marks the end of a word.
pub const WORD_END: char = '\u{4}';

fn is_marker(scalar: char) -> bool {
    matches!(scalar, VALUE_START | WORD_START | VALUE_END | WORD_END)
}

/// One scalar of the source text, folded.
struct Unit {
    folded: Vec<char>,
    /// A letter (`Some(false)`) or digit (`Some(true)`); `None` otherwise.
    word: Option<bool>,
}

/// Folded `text` with word markers, as a scalar sequence. A word is a run
/// of letters or a run of digits.
///
/// A value (`anchored`) is also wrapped in value markers, and its first
/// and last words are marked. A query is not: whether its ends are word
/// boundaries in the text is the question a tier asks, so the caller adds
/// those markers itself.
pub fn marked(text: &str, anchored: bool) -> Vec<char> {
    let units: Vec<Unit> = text
        .chars()
        .filter_map(|scalar| {
            let mut folded = Vec::new();
            fold_char(scalar, &mut |part| {
                // Marker scalars in the text itself would forge a boundary.
                folded.push(if is_marker(part) { ' ' } else { part });
            });
            let first = *folded.first()?;
            let word = first.is_alphanumeric().then(|| first.is_numeric());
            Some(Unit { folded, word })
        })
        .collect();
    let mut out = Vec::with_capacity(text.len().saturating_add(8));
    if anchored {
        out.push(VALUE_START);
    }
    let last = units.len().saturating_sub(1);
    for (at, unit) in units.iter().enumerate() {
        let neighbour = |other: Option<&Unit>| other.and_then(|other| other.word);
        let before = at.checked_sub(1).and_then(|previous| units.get(previous));
        let after = units.get(at.saturating_add(1));
        if let Some(digit) = unit.word
            && (if at == 0 {
                anchored
            } else {
                neighbour(before) != Some(digit)
            })
        {
            out.push(WORD_START);
        }
        out.extend(&unit.folded);
        if let Some(digit) = unit.word
            && (if at == last {
                anchored
            } else {
                neighbour(after) != Some(digit)
            })
        {
            out.push(WORD_END);
        }
    }
    if anchored {
        out.push(VALUE_END);
        out.push(VALUE_END);
    }
    out
}

/// Byte ranges of the words in already folded `text`.
pub fn word_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    // No dictionary or LSTM models in the browser build.
    #[cfg(not(target_arch = "wasm32"))]
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
        LazyLock::new(|| WordSegmenter::new_auto(WordBreakInvariantOptions::default()));
    #[cfg(target_arch = "wasm32")]
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> = LazyLock::new(|| {
        WordSegmenter::new_for_non_complex_scripts(WordBreakInvariantOptions::default())
    });
    let boundaries: Vec<usize> = SEGMENTER.segment_str(text).collect();
    boundaries
        .windows(2)
        .filter_map(|window| {
            let [start, end] = window else {
                return None;
            };
            let range = *start..*end;
            text.get(range.clone())
                .is_some_and(|word| word.chars().any(char::is_alphanumeric))
                .then_some(range)
        })
        .collect()
}

/// Folded words of `text`, the terms the engine's word tokenizer indexes.
pub fn words(text: &str) -> Vec<String> {
    let folded = crate::fold::fold(text);
    word_ranges(&folded)
        .into_iter()
        .filter_map(|range| folded.get(range).map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_mark_inner_words_only() {
        assert_eq!(
            marked("e Du", false),
            ['e', WORD_END, ' ', WORD_START, 'd', 'u']
        );
        assert_eq!(marked("\u{2}x", false), [' ', WORD_START, 'x']);
        // Letters and digits are separate words.
        assert_eq!(marked("v2", false), ['v', WORD_END, WORD_START, '2']);
    }

    #[test]
    fn cjk_text_without_spaces_yields_words() {
        // The browser build carries no dictionary models for scripts
        // written without spaces.
        #[cfg(not(target_family = "wasm"))]
        {
            let words = words("東京都に住んでいます");
            assert!(words.len() >= 2, "ICU splits the sentence: {words:?}");
        }
        let words = super::words("msg-4411@Example.org sent");
        assert!(words.iter().any(|word| word == "msg"), "{words:?}");
        assert!(words.iter().any(|word| word == "sent"), "{words:?}");
    }
}
