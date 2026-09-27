//! The two tantivy tokenizers every index registers.
//!
//! [`GramTokenizer`] finds substrings. It folds a field value, marks where
//! the value and each of its words start and where the value ends, and
//! emits every [`GRAM_WIDTH`]-scalar window at its scalar position. A phrase
//! of windows is then an exact substring match, and a phrase that starts
//! with a marker is a prefix, word-prefix or whole-value match: the tiers
//! the ranking needs, all answered by tantivy's `PhraseQuery`.
//!
//! [`WordTokenizer`] finds words, for BM25 and typo-tolerant
//! `FuzzyTermQuery`. It segments the folded value with ICU word breaking
//! so scripts written without spaces still yield words.

use std::sync::LazyLock;

use icu_segmenter::options::WordBreakInvariantOptions;
use icu_segmenter::{WordSegmenter, WordSegmenterBorrowed};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

use crate::fold::fold_char;

/// Scalars per indexed gram.
pub const GRAM_WIDTH: usize = 3;

/// Marks the start of a field value.
pub(crate) const VALUE_START: char = '\u{1}';
/// Marks the start of a word: a letter or digit after anything else.
pub(crate) const WORD_START: char = '\u{2}';
/// Marks the end of a field value. Values end with two, so every scalar of
/// the value starts a full gram.
pub(crate) const VALUE_END: char = '\u{3}';

fn is_marker(scalar: char) -> bool {
    matches!(scalar, VALUE_START | WORD_START | VALUE_END)
}

/// Folded `text` with word-start markers, as a scalar sequence.
///
/// A value (`anchored`) is also wrapped in value markers and its first word
/// is marked. A query is not: whether its first scalar starts a word is the
/// question a tier asks, so the caller adds that marker itself.
pub(crate) fn marked(text: &str, anchored: bool) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len().saturating_add(4));
    if anchored {
        out.push(VALUE_START);
    }
    let mut previous_is_word = false;
    let mut first = true;
    for scalar in text.chars() {
        fold_char(scalar, &mut |folded| {
            // Marker scalars in the text itself would forge a boundary.
            let folded = if is_marker(folded) { ' ' } else { folded };
            let is_word = folded.is_alphanumeric();
            if is_word && !previous_is_word && (anchored || !first) {
                out.push(WORD_START);
            }
            out.push(folded);
            previous_is_word = is_word;
            first = false;
        });
    }
    if anchored {
        out.push(VALUE_END);
        out.push(VALUE_END);
    }
    out
}

/// Tokenizer emitting the marked [`GRAM_WIDTH`]-scalar windows of a value.
#[derive(Clone, Debug, Default)]
pub struct GramTokenizer {
    token: Token,
    scalars: Vec<char>,
}

pub struct GramTokenStream<'a> {
    token: &'a mut Token,
    scalars: &'a [char],
    next: usize,
}

impl Tokenizer for GramTokenizer {
    type TokenStream<'a> = GramTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> GramTokenStream<'a> {
        self.token.reset();
        self.scalars = marked(text, true);
        GramTokenStream {
            token: &mut self.token,
            scalars: &self.scalars,
            next: 0,
        }
    }
}

impl TokenStream for GramTokenStream<'_> {
    fn advance(&mut self) -> bool {
        let start = self.next;
        let Some(window) = self.scalars.get(start..start.saturating_add(GRAM_WIDTH)) else {
            return false;
        };
        if window.len() < GRAM_WIDTH {
            return false;
        }
        self.token.text.clear();
        self.token.text.extend(window);
        self.token.position = start;
        self.token.offset_from = start;
        self.token.offset_to = start.saturating_add(GRAM_WIDTH);
        self.next = start.saturating_add(1);
        true
    }

    fn token(&self) -> &Token {
        self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        self.token
    }
}

/// Byte ranges of the words in already folded `text`.
pub(crate) fn word_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
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

/// Folded words of `text`, the terms [`WordTokenizer`] indexes.
pub(crate) fn words(text: &str) -> Vec<String> {
    let folded = crate::fold::fold(text);
    word_ranges(&folded)
        .into_iter()
        .filter_map(|range| folded.get(range).map(str::to_owned))
        .collect()
}

/// Tokenizer emitting the folded ICU words of a value.
#[derive(Clone, Debug, Default)]
pub struct WordTokenizer {
    token: Token,
    folded: String,
    ranges: Vec<std::ops::Range<usize>>,
}

pub struct WordTokenStream<'a> {
    token: &'a mut Token,
    folded: &'a str,
    ranges: std::slice::Iter<'a, std::ops::Range<usize>>,
    position: usize,
}

impl Tokenizer for WordTokenizer {
    type TokenStream<'a> = WordTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> WordTokenStream<'a> {
        self.token.reset();
        self.folded = crate::fold::fold(text);
        self.ranges = word_ranges(&self.folded);
        WordTokenStream {
            token: &mut self.token,
            folded: &self.folded,
            ranges: self.ranges.iter(),
            position: 0,
        }
    }
}

impl TokenStream for WordTokenStream<'_> {
    fn advance(&mut self) -> bool {
        for range in self.ranges.by_ref() {
            let Some(word) = self.folded.get(range.clone()) else {
                continue;
            };
            self.token.text.clear();
            self.token.text.push_str(word);
            self.token.offset_from = range.start;
            self.token.offset_to = range.end;
            self.token.position = self.position;
            self.position = self.position.saturating_add(1);
            return true;
        }
        false
    }

    fn token(&self) -> &Token {
        self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grams(text: &str) -> Vec<(usize, String)> {
        let mut tokenizer = GramTokenizer::default();
        let mut stream = tokenizer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push((stream.token().position, stream.token().text.clone()));
        }
        out
    }

    #[test]
    fn values_are_marked_and_every_scalar_starts_a_gram() {
        let grams = grams("Ab-c");
        let texts: Vec<_> = grams.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "\u{1}\u{2}a",
                "\u{2}ab",
                "ab-",
                "b-\u{2}",
                "-\u{2}c",
                "\u{2}c\u{3}",
                "c\u{3}\u{3}"
            ]
        );
        assert!(
            grams
                .iter()
                .enumerate()
                .all(|(at, (position, _))| at == *position)
        );
    }

    #[test]
    fn queries_mark_inner_words_only() {
        assert_eq!(marked("e Du", false), ['e', ' ', WORD_START, 'd', 'u']);
        assert_eq!(marked("\u{2}x", false), [' ', WORD_START, 'x']);
    }

    #[test]
    fn cjk_text_without_spaces_yields_words() {
        let words = words("東京都に住んでいます");
        assert!(words.len() >= 2, "ICU splits the sentence: {words:?}");
        let words = super::words("msg-4411@Example.org sent");
        assert!(words.iter().any(|word| word == "msg"), "{words:?}");
        assert!(words.iter().any(|word| word == "sent"), "{words:?}");
    }
}
