//! The two tantivy tokenizers every index registers.
//!
//! [`GramTokenizer`] finds substrings. It folds a field value, marks where
//! the value and each of its words start and where the value ends, and
//! emits every [`GRAM_WIDTH`]-scalar window at its scalar position. A phrase
//! of windows is then an exact substring match, and a phrase that starts
//! with a marker is a prefix, word-prefix or whole-value match: the tiers
//! the ranking needs, all answered by tantivy's `PhraseQuery`.
//!
//! [`WordTokenizer`] finds words, for BM25 and for matching a typed word
//! within a typo or two of them. It segments the folded value with ICU word
//! breaking so scripts written without spaces still yield words.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tilo_search_model::fold::fold;
use tilo_search_model::tokenizer::{GRAM_WIDTH, marked, word_ranges};

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
        self.folded = fold(text);
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
                "ab\u{4}",
                "b\u{4}-",
                "\u{4}-\u{2}",
                "-\u{2}c",
                "\u{2}c\u{4}",
                "c\u{4}\u{3}",
                "\u{4}\u{3}\u{3}"
            ]
        );
        assert!(
            grams
                .iter()
                .enumerate()
                .all(|(at, (position, _))| at == *position)
        );
    }
}
