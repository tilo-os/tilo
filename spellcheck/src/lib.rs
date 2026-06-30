//! A self-contained English spelling engine.
//!
//! The engine has three stages, mirroring the public API:
//!
//! 1. [`tokenize`] splits text into word [`Token`]s carrying byte offsets, so a
//!    caller can map a flagged word back to a span in the original string.
//! 2. [`Dictionary`] holds a set of known words. [`Dictionary::builtin`] loads an
//!    embedded English word list; [`Dictionary::from_words`] builds one from any
//!    iterator of strings.
//! 3. [`check`] walks the tokens, skips things that are not really spellable
//!    (numbers, ALL-CAPS acronyms, URLs, emails), and flags the rest that are not
//!    in the dictionary, attaching a ranked list of [`suggest`]ions.
//!
//! Everything is Unicode-aware at the `char` level and uses no external crates,
//! so adding this crate does not perturb the workspace lockfile.
//!
//! ```
//! use spellcheck::{Dictionary, check};
//!
//! let dict = Dictionary::from_words(["the", "quick", "brown", "fox"]);
//! let misspellings = check(&dict, "teh quick brown fox");
//! assert_eq!(misspellings.len(), 1);
//! assert_eq!(misspellings[0].word, "teh");
//! assert!(misspellings[0].suggestions.iter().any(|s| s == "the"));
//! ```

mod dictionary;
mod distance;
mod suggest;
mod token;

pub use dictionary::Dictionary;
pub use distance::damerau_levenshtein_within;
pub use suggest::suggest;
pub use token::{Token, tokenize};

/// A word that was flagged as not present in the dictionary, with its location
/// in the source text and a ranked list of correction candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misspelling {
    /// The offending word, exactly as it appeared in the source (original case).
    pub word: String,
    /// Byte offset of the first byte of the word in the source string.
    pub start: usize,
    /// Byte offset one past the last byte of the word in the source string.
    pub end: usize,
    /// Suggested corrections, best first (closest edit distance, then shortest).
    pub suggestions: Vec<String>,
}

/// Maximum number of suggestions attached to each [`Misspelling`] by [`check`].
const DEFAULT_MAX_SUGGESTIONS: usize = 5;

/// Spell-check `text` against `dict`, returning one [`Misspelling`] per flagged
/// word in source order.
///
/// A token is *not* flagged when it is:
/// - present in the dictionary (compared case-insensitively), or
/// - a pure number (e.g. `42`, `3.14`, `1,000`), or
/// - an ALL-CAPS acronym of length >= 2 (e.g. `NASA`, `qOS` is *not* all-caps so
///   it would still be checked), or
/// - a URL or email-looking token.
pub fn check(dict: &Dictionary, text: &str) -> Vec<Misspelling> {
    let mut out = Vec::new();
    for tok in tokenize(text) {
        if !is_spellable(tok.text) {
            continue;
        }
        if dict.contains(tok.text) {
            continue;
        }
        let suggestions = suggest(dict, tok.text, DEFAULT_MAX_SUGGESTIONS);
        out.push(Misspelling {
            word: tok.text.to_string(),
            start: tok.start,
            end: tok.end,
            suggestions,
        });
    }
    out
}

/// Returns `true` if `word` is the kind of token a spell checker should judge.
///
/// Filters out pure numbers, ALL-CAPS acronyms, and URL/email-shaped tokens. The
/// tokenizer already strips most punctuation, but it intentionally keeps `.`, `@`,
/// `:` and `/` inside a run so that URLs and emails arrive here intact and can be
/// recognised and skipped as a unit.
fn is_spellable(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }

    // URLs / emails: presence of these structural characters means "not a word".
    if word.contains('@') || word.contains('/') || word.contains("://") {
        return false;
    }
    // A scheme-less domain such as `example.com` or `foo.bar.org`: a token that
    // contains an internal dot with alphanumerics on both sides is URL-ish.
    if looks_like_domain(word) {
        return false;
    }

    // A token that begins with a digit is number-like: ordinals ("100th",
    // "1st"), versions ("3a"), measurements ("12px"). These are not dictionary
    // words, so skip them rather than flag them.
    if word.chars().next().is_some_and(|c| c.is_numeric()) {
        return false;
    }

    let mut has_alpha = false;
    let mut has_digit = false;
    let mut all_upper = true;
    let mut upper_count = 0usize;
    for c in word.chars() {
        if c.is_alphabetic() {
            has_alpha = true;
            if c.is_uppercase() {
                upper_count += 1;
            } else {
                all_upper = false;
            }
        } else if c.is_numeric() {
            has_digit = true;
        }
    }

    // Pure numbers (and number-like tokens with no letters): skip.
    if !has_alpha {
        return false;
    }
    // Mixed alphanumerics that are not plain words (e.g. "h4ck3r", "v2") are not
    // worth flagging as spelling errors and have no meaningful suggestions.
    if has_digit {
        return false;
    }

    // ALL-CAPS acronyms of length >= 2 (e.g. NASA, HTML). A single capital letter
    // ("I", "A") is still a real word and stays spellable.
    let char_count = word.chars().count();
    if all_upper && upper_count >= 2 && char_count >= 2 {
        return false;
    }

    true
}

/// Heuristic: does the token look like a bare domain name (`name.tld`)?
fn looks_like_domain(word: &str) -> bool {
    let Some(dot) = word.find('.') else {
        return false;
    };
    // Internal dot only — leading/trailing dots are punctuation the tokenizer
    // would normally have stripped.
    if dot == 0 || dot + 1 >= word.len() {
        return false;
    }
    let before = &word[..dot];
    let after = &word[dot + 1..];
    let alnum = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '.');
    // Require the trailing label to be alphabetic (a TLD-ish), so that decimals
    // like `3.14` (handled by the no-alpha path anyway) and ordinary sentence
    // text never reach here.
    alnum(before) && alnum(after) && after.chars().any(|c| c.is_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Dictionary {
        Dictionary::builtin()
    }

    #[test]
    fn teh_is_flagged_and_suggests_the() {
        let dict = builtin();
        let result = check(&dict, "teh");
        assert_eq!(result.len(), 1, "‘teh’ should be flagged once");
        assert_eq!(result[0].word, "teh");
        assert!(
            result[0].suggestions.iter().any(|s| s == "the"),
            "expected ‘the’ among suggestions, got {:?}",
            result[0].suggestions
        );
    }

    #[test]
    fn correct_sentence_has_no_misspellings() {
        let dict = builtin();
        let text = "the quick brown fox jumps over the lazy dog";
        let result = check(&dict, text);
        assert!(
            result.is_empty(),
            "expected zero misspellings, got {:?}",
            result
        );
    }

    #[test]
    fn numbers_urls_and_allcaps_are_skipped() {
        let dict = builtin();
        let text = "Visit https://example.com or email me@example.com about NASA and 12345 or 3.14";
        let result = check(&dict, text);
        let flagged: Vec<&str> = result.iter().map(|m| m.word.as_str()).collect();
        assert!(
            flagged.is_empty(),
            "numbers/URLs/emails/ALL-CAPS should be skipped, but flagged: {:?}",
            flagged
        );
    }

    #[test]
    fn allcaps_acronym_alone_is_skipped() {
        let dict = builtin();
        assert!(check(&dict, "NASA").is_empty());
        assert!(check(&dict, "HTML").is_empty());
        // A single capital is a real one-letter word, still checked (and valid).
        assert!(check(&dict, "I").is_empty());
    }

    #[test]
    fn pure_numbers_skipped() {
        let dict = builtin();
        for n in ["0", "42", "3.14", "1,000", "2024", "100th"] {
            assert!(
                check(&dict, n).is_empty(),
                "number-like token {n:?} should not be flagged"
            );
        }
    }

    #[test]
    fn tokenize_offsets_round_trip_a_slice() {
        let text = "Hello, world! It's a   test.";
        for tok in tokenize(text) {
            // The recorded span must slice back to exactly the token text.
            assert_eq!(&text[tok.start..tok.end], tok.text);
        }
        // And the specific words we expect are present, in order, with apostrophe.
        let words: Vec<&str> = tokenize(text).iter().map(|t| t.text).collect();
        assert_eq!(words, vec!["Hello", "world", "It's", "a", "test"]);
    }

    #[test]
    fn case_insensitive_membership() {
        let dict = builtin();
        // Mixed/upper case of a known word must not be flagged.
        assert!(check(&dict, "The Quick Brown Fox").is_empty());
    }

    #[test]
    fn unknown_word_with_suggestions_records_span() {
        let dict = builtin();
        let text = "a wrold of color";
        let result = check(&dict, text);
        assert_eq!(result.len(), 1, "got {result:?}");
        let m = &result[0];
        assert_eq!(m.word, "wrold");
        assert_eq!(&text[m.start..m.end], "wrold");
        assert!(
            m.suggestions.iter().any(|s| s == "world"),
            "expected ‘world’, got {:?}",
            m.suggestions
        );
    }

    #[test]
    fn from_words_builds_a_usable_dictionary() {
        let dict = Dictionary::from_words(["alpha", "beta", "gamma"]);
        assert!(dict.contains("ALPHA"));
        assert!(!dict.contains("delta"));
        let result = check(&dict, "alpha delat");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].word, "delat");
        // None of the three short, distant words is within edit distance 2.
        assert!(result[0].suggestions.is_empty());
    }

    #[test]
    fn suggestions_are_ranked_by_distance_then_length() {
        // Build a small dictionary where ordering is deterministic and checkable.
        let dict = Dictionary::from_words(["cat", "cart", "cats", "scat", "cataract"]);
        let s = suggest(&dict, "cot", 5);
        // "cat" is distance 1; the 4-letter ones are distance >= 1 too but longer.
        assert_eq!(s.first().map(String::as_str), Some("cat"));
    }

    #[test]
    fn builtin_dictionary_is_substantial() {
        let dict = builtin();
        assert!(
            dict.len() > 10_000,
            "builtin dictionary unexpectedly small: {}",
            dict.len()
        );
    }
}
