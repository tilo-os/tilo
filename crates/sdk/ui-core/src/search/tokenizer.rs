//! Text folding and term segmentation shared by every search index.
//!
//! A catalogue, the ranker and every streamed body verification fold text
//! through one [`SearchTokenizer`], so an indexed gram, a verified substring
//! and a ranked title all agree about what "the same text" means. The default
//! [`UnicodeTokenizer`] folds case, compatibility forms and Latin, Greek and
//! Cyrillic diacritics, and splits terms with ICU word segmentation so CJK
//! text without spaces still yields whole words.

use std::fmt::Debug;
use std::sync::Arc;

/// How a search index folds and segments text.
///
/// `fold_char` must be a pure per-scalar mapping: the catalogue folds a query
/// once and streams large bodies through it one scalar at a time, and both
/// sides must produce the same sequence for the same text.
pub trait SearchTokenizer: Send + Sync + Debug {
    /// Stable identity. Indexes built by two tokenizers with the same id are
    /// interchangeable; a catalogue re-indexes a prebuilt body otherwise.
    fn id(&self) -> &'static str;

    /// Append the folded form of `scalar` (zero or more scalars) to `out`.
    fn fold_char(&self, scalar: char, out: &mut dyn FnMut(char));

    /// Whole terms of already folded text, for exact-term postings. Terms
    /// must include every whitespace-delimited chunk — a catalogue narrows a
    /// multi-word query by the chunks inside it — and may add finer words.
    /// Terms may overlap; duplicates are allowed and removed by the caller.
    fn terms<'a>(&self, folded: &'a str) -> Vec<&'a str>;

    /// Fold complete text.
    fn fold(&self, text: &str) -> String {
        let mut folded = String::with_capacity(text.len());
        for scalar in text.chars() {
            self.fold_char(scalar, &mut |folded_scalar| folded.push(folded_scalar));
        }
        folded
    }
}

/// The default tokenizer: case-insensitive, compatibility-folded and
/// diacritic-insensitive for alphabetic scripts, with ICU word terms.
///
/// Only marks from the combining-diacritics blocks are dropped. Marks that
/// carry meaning elsewhere — kana voicing, Indic vowel signs, Hebrew and
/// Arabic points — are kept, and Hangul syllables are not split into jamo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnicodeTokenizer;

impl UnicodeTokenizer {
    const fn is_folded_diacritic(scalar: char) -> bool {
        matches!(
            scalar,
            '\u{0300}'..='\u{036F}'
                | '\u{1AB0}'..='\u{1AFF}'
                | '\u{1DC0}'..='\u{1DFF}'
                | '\u{20D0}'..='\u{20FF}'
                | '\u{FE20}'..='\u{FE2F}'
        )
    }

    const fn keeps_composition(scalar: char) -> bool {
        matches!(
            scalar,
            // Hangul syllables, and kana whose voicing marks distinguish words.
            '\u{AC00}'..='\u{D7A3}' | '\u{3040}'..='\u{30FF}'
        )
    }
}

impl SearchTokenizer for UnicodeTokenizer {
    fn id(&self) -> &'static str {
        "tilo.unicode.v1"
    }

    fn fold_char(&self, scalar: char, out: &mut dyn FnMut(char)) {
        if scalar.is_ascii() {
            out(scalar.to_ascii_lowercase());
            return;
        }
        for lower in scalar.to_lowercase() {
            if Self::keeps_composition(lower) {
                out(lower);
                continue;
            }
            unicode_normalization::char::decompose_compatible(lower, |part| {
                if Self::is_folded_diacritic(part) {
                    return;
                }
                // Compatibility forms can decompose to capitals (`ℌ`), so
                // lowercase once more after decomposition.
                for folded in part.to_lowercase() {
                    out(folded);
                }
            });
        }
    }

    fn terms<'a>(&self, folded: &'a str) -> Vec<&'a str> {
        let mut terms: Vec<&'a str> = folded.split_whitespace().collect();
        terms.extend(
            crate::text_break::unicode_word_ranges(folded).filter_map(|range| folded.get(range)),
        );
        terms
    }
}

/// The shared default tokenizer handle.
#[must_use]
pub fn default_tokenizer() -> Arc<dyn SearchTokenizer> {
    Arc::new(UnicodeTokenizer)
}

/// Fold text with the default tokenizer — the form every default catalogue
/// indexes and the ranker compares.
#[must_use]
pub fn fold_search_text(text: &str) -> String {
    UnicodeTokenizer.fold(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_diacritics_and_compatibility_forms() {
        assert_eq!(fold_search_text("Crème Brûlée"), "creme brulee");
        assert_eq!(fold_search_text("ÅNGSTRÖM"), "angstrom");
        assert_eq!(fold_search_text("Ｔｏｋｙｏ ﬁle"), "tokyo file");
        assert_eq!(fold_search_text("Ἀθῆναι"), "αθηναι");
        // Canonically equivalent spellings fold alike.
        assert_eq!(fold_search_text("e\u{301}"), fold_search_text("é"));
    }

    #[test]
    fn keeps_marks_that_distinguish_words() {
        assert_eq!(fold_search_text("がか"), "がか");
        assert_eq!(fold_search_text("한국어"), "한국어");
        assert_eq!(fold_search_text("किताब"), "किताब");
    }

    #[test]
    fn cjk_text_without_spaces_yields_word_terms() {
        let folded = fold_search_text("東京都に住んでいます");
        let terms = UnicodeTokenizer.terms(&folded);
        let words: Vec<_> = terms.iter().filter(|term| **term != folded).collect();
        assert!(words.len() >= 2, "ICU splits the sentence: {terms:?}");
        assert!(words.iter().all(|word| folded.contains(**word)));
    }

    #[test]
    fn whitespace_chunks_and_inner_words_are_both_terms() {
        let terms = UnicodeTokenizer.terms("msg-4411@example.org sent");
        assert!(terms.contains(&"msg-4411@example.org"));
        assert!(terms.contains(&"msg"));
        assert!(terms.contains(&"sent"));
    }
}
