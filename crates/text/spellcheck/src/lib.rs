//! A self-contained English spelling engine.
//!
//! The engine has three stages, mirroring the public API:
//!
//! 1. [`tokenize`] splits text into word [`Token`]s carrying byte offsets, so a
//!    caller can map a flagged word back to a span in the original string.
//! 2. [`Dictionary`] holds a set of known words. [`Dictionary::builtin`] loads an
//!    embedded English word list; [`Dictionary::from_words`] builds one from any
//!    iterator of strings.
//! 3. [`check`] walks the tokens, skips non-spellable forms
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

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::borrow::Cow;
use std::sync::OnceLock;

mod dictionary;
mod distance;
mod hunspell;
mod suggest;
mod token;

use hunspell::{results as hunspell_results, suggestions as hunspell_suggestions};

pub use dictionary::{Dictionary, user_dict_path};
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
/// - an ALL-CAPS acronym of length >= 2 (e.g. `NASA`, `Tilo` is *not* all-caps so
///   it would still be checked), or
/// - a URL or email-looking token.
pub fn check(dict: &Dictionary, text: &str) -> Vec<Misspelling> {
    let mut out = Vec::new();
    for tok in tokenize(text) {
        if !is_spellable(tok.text) {
            continue;
        }
        if dict.contains(tok.text) || is_likely_proper_name(text, &tok) {
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

/// Where the misspelled words in `text` are, without suggesting corrections.
///
/// [`check`] suggests a replacement for every flagged word and computes edit
/// distances across the dictionary. Drawing a squiggle needs only the
/// positions, so this avoids suggestion work when rendering underlines.
pub fn spans(dict: &Dictionary, text: &str) -> Vec<(usize, usize)> {
    tokenize(text)
        .into_iter()
        .filter(|tok| {
            is_spellable(tok.text) && !dict.contains(tok.text) && !is_likely_proper_name(text, tok)
        })
        .map(|tok| (tok.start, tok.end))
        .collect()
}

/// Spell-check text in a BCP-47-ish language such as en-US or de-DE.
///
/// When the system has Hunspell plus that language's dictionary, its full
/// affix-aware engine is used through a persistent per-language process.
/// English falls back to the embedded dictionary, so checking still works in
/// hermetic builds and app sandboxes. The command is invoked directly, never
/// through a shell.
pub fn spans_for_language(language: &str, text: &str) -> Vec<(usize, usize)> {
    // English typing stays entirely in-process. Spawning/querying Hunspell for
    // each prefix was measurably slower; the embedded set includes its expanded
    // forms plus a conversational overlay, and is warmed before first focus.
    if is_english(language) {
        return spans(shared_english(), text);
    }
    let tokens: Vec<_> = tokenize(text)
        .into_iter()
        .filter(|token| is_spellable(token.text))
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    // Hunspell dictionaries conventionally use ASCII apostrophes even when a
    // typographic editor inserts U+2019. Normalise only for lookup; token byte
    // ranges still refer to the untouched source text.
    let lookup_words: Vec<Cow<'_, str>> = tokens
        .iter()
        .map(|token| normalize_lookup_word(token.text))
        .collect();
    let words: Vec<&str> = lookup_words.iter().map(AsRef::as_ref).collect();
    if let Some(correct) = hunspell_results(language, &words) {
        return tokens
            .into_iter()
            .zip(correct)
            .filter_map(|(token, is_correct)| (!is_correct).then_some((token.start, token.end)))
            .collect();
    }

    if is_english(language) {
        spans(shared_english(), text)
    } else {
        Vec::new()
    }
}

/// Corrections for one word, ranked by the system Hunspell dictionary.
/// English falls back to the embedded edit-distance suggester.
pub fn suggestions_for_language(language: &str, word: &str, max: usize) -> Vec<String> {
    if word.is_empty() || max == 0 {
        return Vec::new();
    }
    if is_english(language) {
        return suggest(shared_english(), word, max);
    }
    let lookup = normalize_lookup_word(word);
    if let Some(mut suggestions) = hunspell_suggestions(language, lookup.as_ref()) {
        suggestions.truncate(max);
        return suggestions;
    }
    Vec::new()
}

/// Return a correction only when the signal is strong enough for automatic
/// replacement. This is intentionally much stricter than the suggestion menu:
/// known high-frequency typos are safe-listed, while the generic path accepts
/// only an adjacent transposition matching the dictionary's first candidate.
pub fn probable_correction(language: &str, word: &str) -> Option<String> {
    if word.chars().count() < 3 {
        return None;
    }
    if word
        .chars()
        .filter(|character| character.is_alphabetic())
        .count()
        >= 2
        && word
            .chars()
            .filter(|character| character.is_alphabetic())
            .all(char::is_uppercase)
    {
        return None;
    }
    let lower = word.to_lowercase();
    let known = if is_english(language) {
        match lower.as_str() {
            "accomodate" => Some("accommodate"),
            "acheive" => Some("achieve"),
            "adn" => Some("and"),
            "adress" => Some("address"),
            "arguement" => Some("argument"),
            "becuase" => Some("because"),
            "begining" => Some("beginning"),
            "calender" => Some("calendar"),
            "comming" => Some("coming"),
            "concious" => Some("conscious"),
            "definately" => Some("definitely"),
            "enviroment" => Some("environment"),
            "existance" => Some("existence"),
            "foriegn" => Some("foreign"),
            "freind" => Some("friend"),
            "greatful" => Some("grateful"),
            "happend" => Some("happened"),
            "independant" => Some("independent"),
            "knowlege" => Some("knowledge"),
            "maintainance" => Some("maintenance"),
            "neccessary" => Some("necessary"),
            "occassion" => Some("occasion"),
            "occured" => Some("occurred"),
            "publically" => Some("publicly"),
            "realy" => Some("really"),
            "recieve" => Some("receive"),
            "remeber" => Some("remember"),
            "seperate" => Some("separate"),
            "succesful" => Some("successful"),
            "taht" => Some("that"),
            "teh" => Some("the"),
            "tomorow" => Some("tomorrow"),
            "tounge" => Some("tongue"),
            "truely" => Some("truly"),
            "untill" => Some("until"),
            "wierd" => Some("weird"),
            _ => None,
        }
    } else {
        None
    };
    // Curated corrections are deliberately independent of dictionary startup,
    // so their preview and boundary replacement are instant on the first word.
    if let Some(candidate) = known {
        return Some(match_word_case(word, candidate));
    }
    if spans_for_language(language, word).is_empty() {
        return None;
    }
    if is_english(language) {
        let candidate = unique_adjacent_transposition(shared_english(), &lower)?;
        return Some(match_word_case(word, &candidate));
    }
    let candidate = suggestions_for_language(language, word, 1)
        .into_iter()
        .next()
        .filter(|candidate| is_adjacent_transposition(&lower, &candidate.to_lowercase()))?;
    Some(match_word_case(word, &candidate))
}

/// Adjacent transpositions have a tiny candidate space: a word of length `n`
/// has at most `n - 1` possibilities. Direct membership checks avoid running
/// the general edit-distance suggester in the typing/drawing path, and requiring
/// exactly one valid result keeps automatic replacement conservative.
fn unique_adjacent_transposition(dict: &Dictionary, word: &str) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut found: Option<String> = None;
    for index in 0..chars.len().saturating_sub(1) {
        if chars.get(index) == chars.get(index.saturating_add(1)) {
            continue;
        }
        let mut candidate = chars.clone();
        candidate.swap(index, index.saturating_add(1));
        let candidate: String = candidate.into_iter().collect();
        if dict.contains(&candidate) {
            if found
                .as_ref()
                .is_some_and(|existing| existing != &candidate)
            {
                return None;
            }
            found = Some(candidate);
        }
    }
    found
}

fn normalize_lookup_word(word: &str) -> Cow<'_, str> {
    if word.contains('\u{2019}') {
        Cow::Owned(word.replace('\u{2019}', "'"))
    } else {
        Cow::Borrowed(word)
    }
}

fn is_adjacent_transposition(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() != b.len() {
        return false;
    }
    let differences: Vec<usize> = a
        .iter()
        .zip(&b)
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect();
    let (Some(&first), Some(&second)) = (differences.first(), differences.get(1)) else {
        return false;
    };
    differences.len() == 2
        && second == first.saturating_add(1)
        && a.get(first) == b.get(second)
        && a.get(second) == b.get(first)
}

fn match_word_case(original: &str, correction: &str) -> String {
    if original.chars().all(|character| !character.is_lowercase()) {
        return correction.to_uppercase();
    }
    let mut original_chars = original.chars();
    if original_chars.next().is_some_and(char::is_uppercase)
        && original_chars.all(|character| !character.is_uppercase())
    {
        let mut chars = correction.chars();
        return chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
        });
    }
    correction.to_string()
}

fn shared_english() -> &'static Dictionary {
    static DICTIONARY: OnceLock<Dictionary> = OnceLock::new();
    DICTIONARY.get_or_init(Dictionary::builtin)
}

/// Begin building the in-process language data before the first field is
/// focused. The one background warm-up is intentionally best-effort: callers
/// can still use the checker immediately and will wait on the same `OnceLock`
/// only if they beat this short head start.
pub fn warm_up_language(language: &str) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if !is_english(language) {
        return;
    }
    STARTED.get_or_init(|| {
        // A browser page has no threads; its first check builds the data.
        if cfg!(target_family = "wasm") {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("spellcheck-warmup".to_string())
            .spawn(|| {
                let _ = shared_english();
            });
    });
}

/// The session language in Hunspell's conventional `ll_CC` form.
pub fn default_language() -> String {
    let raw = std::env::var("LC_ALL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("LC_MESSAGES")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| std::env::var("LANG").ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "en_US".to_string());
    let base = raw.split('.').next().unwrap_or("en_US");
    let base = base.split('@').next().unwrap_or(base);
    if matches!(base, "C" | "POSIX") {
        "en_US".to_string()
    } else {
        normalize_language(base).unwrap_or_else(|| "en_US".to_string())
    }
}

fn is_english(language: &str) -> bool {
    language
        .get(..2)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("en"))
}

/// Whether ordinary span checks for this language stay inside the process.
/// Callers can use this to keep external Hunspell IPC out of the immediate
/// key-event frame without special-casing language tags themselves.
pub fn checks_in_process(language: &str) -> bool {
    is_english(language)
}

fn normalize_language(language: &str) -> Option<String> {
    let normalized = language.replace('-', "_");
    (!normalized.is_empty()
        && normalized
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(normalized)
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
    if word.chars().next().is_some_and(char::is_numeric) {
        return false;
    }

    let mut has_alpha = false;
    let mut has_digit = false;
    let mut all_upper = true;
    let mut upper_count = 0usize;
    let mut internal_upper = false;
    for c in word.chars() {
        if c.is_alphabetic() {
            if has_alpha && c.is_uppercase() {
                internal_upper = true;
            }
            has_alpha = true;
            if c.is_uppercase() {
                upper_count = upper_count.saturating_add(1);
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
    // Internal capitals usually mean an identifier or brand (`TextInput`,
    // `iPhone`, `GitHub`), not a misspelling. A leading sentence/title capital
    // remains checkable because it has at most one uppercase letter.
    if internal_upper && !all_upper {
        return false;
    }

    true
}

/// Unknown title-cased words inside a sentence are overwhelmingly proper
/// nouns (people, places, products). Avoiding those noisy squiggles is more
/// useful than guessing at a correction; sentence-initial words are still
/// checked, where capitalization carries no proper-name signal.
fn is_likely_proper_name(text: &str, token: &Token<'_>) -> bool {
    let mut chars = token.text.chars();
    if !chars.next().is_some_and(char::is_uppercase)
        || !chars.all(|character| !character.is_uppercase())
    {
        return false;
    }
    let Some(prefix) = text.get(..token.start) else {
        return false;
    };
    if prefix
        .chars()
        .rev()
        .take_while(|character| character.is_whitespace())
        .any(|character| matches!(character, '\n' | '\r'))
    {
        return false;
    }
    let Some(previous) = prefix.trim_end().chars().next_back() else {
        return false;
    };
    !matches!(previous, '.' | '!' | '?' | '\n' | '\r')
}

/// Heuristic: does the token look like a bare domain name (`name.tld`)?
fn looks_like_domain(word: &str) -> bool {
    let Some((before, after)) = word.split_once('.') else {
        return false;
    };
    // Internal dot only — leading/trailing dots are punctuation the tokenizer
    // would normally have stripped.
    if before.is_empty() || after.is_empty() {
        return false;
    }
    let alnum = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '.');
    // Require the trailing label to be alphabetic (a TLD-ish), so that decimals
    // like `3.14` (handled by the no-alpha path anyway) and ordinary sentence
    // text never reach here.
    alnum(before) && alnum(after) && after.chars().any(char::is_alphabetic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Dictionary {
        Dictionary::builtin()
    }

    #[test]
    fn check_flags_misspellings_but_accepts_conversational_english_and_names() {
        // Phase `teh_is_flagged_and_suggests_the`.
        {
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
        // Phase `correct_sentence_has_no_misspellings`.
        {
            let dict = builtin();
            let text = "the quick brown fox jumps over the lazy dog";
            let result = check(&dict, text);
            assert!(
                result.is_empty(),
                "expected zero misspellings, got {result:?}"
            );
        }
        // Phase `conversational_english_accepts_contractions_smart_quotes_and_common_names`.
        {
            let dict = builtin();
            for word in ["I'm", "I\u{2019}m", "James", "Emma", "Michael", "Sophia"] {
                assert!(
                    spans(&dict, word).is_empty(),
                    "embedded fallback should accept {word:?}"
                );
                assert!(
                    spans_for_language("en_US", word).is_empty(),
                    "language checker should accept {word:?}"
                );
            }
        }
        // Phase `titlecase_unknowns_are_treated_as_names_only_inside_a_sentence`.
        {
            let dict = builtin();
            assert!(spans(&dict, "I met Qwordz today").is_empty());
            assert_eq!(spans(&dict, "Qwordz arrived"), vec![(0, 6)]);
            assert_eq!(spans(&dict, "Hello. Qwordz arrived"), vec![(7, 13)]);
            assert_eq!(spans(&dict, "Hello\nQwordz arrived"), vec![(6, 12)]);
        }
        // Phase `mixed_case_identifiers_and_brands_are_not_spellchecked`.
        {
            let dict = builtin();
            assert!(spans(&dict, "TextInput iPhone GitHub").is_empty());
        }
        // Phase `unknown_word_with_suggestions_records_span`.
        {
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
    }

    #[test]
    fn spans_agree_with_check_and_skip_numbers_urls_and_acronyms() {
        // Phase `spans_flag_the_same_words_check_does`.
        // The squiggle comes from `spans` and the correction menu from `check`, so
        // a disagreement means a word underlined with nothing to offer, or a
        // correction offered with no underline. The fixture mixes a number, an
        // acronym and a URL so every is-this-judgeable rule runs on both paths.
        {
            let dict = builtin();
            let text = "The recieve date is 2026 for NASA at https://example.com/x and teh rest.";
            let full: Vec<(usize, usize)> = check(&dict, text)
                .iter()
                .map(|m| (m.start, m.end))
                .collect();
            assert_eq!(spans(&dict, text), full);
            assert!(!full.is_empty(), "the fixture has to contain a misspelling");
            // Guards against both paths agreeing on the same wrong answer.
            let flagged: Vec<&str> = full.iter().map(|&(s, e)| &text[s..e]).collect();
            assert_eq!(flagged, vec!["recieve", "teh"]);
        }
        // Phase `spans_agree_with_check_on_every_prefix_of_a_word`.
        // Every prefix of a word is a misspelling, which is why the draw path hit
        // this on every keystroke.
        {
            let dict = builtin();
            let word = "rehearsal";
            for n in 1..=word.len() {
                let partial = &word[..n];
                let full: Vec<(usize, usize)> = check(&dict, partial)
                    .iter()
                    .map(|m| (m.start, m.end))
                    .collect();
                assert_eq!(
                    spans(&dict, partial),
                    full,
                    "the two paths disagree on {partial:?}"
                );
            }
        }
        // Phase `numbers_urls_and_allcaps_are_skipped`.
        {
            let dict = builtin();
            let text =
                "Visit https://example.com or email me@example.com about NASA and 12345 or 3.14";
            let result = check(&dict, text);
            let flagged: Vec<&str> = result.iter().map(|m| m.word.as_str()).collect();
            assert!(
                flagged.is_empty(),
                "numbers/URLs/emails/ALL-CAPS should be skipped, but flagged: {flagged:?}"
            );
        }
        // Phase `allcaps_acronym_alone_is_skipped`.
        {
            let dict = builtin();
            assert!(check(&dict, "NASA").is_empty());
            assert!(check(&dict, "HTML").is_empty());
            // A single capital is a real one-letter word, still checked (and valid).
            assert!(check(&dict, "I").is_empty());
        }
        // Phase `pure_numbers_skipped`.
        {
            let dict = builtin();
            for n in ["0", "42", "3.14", "1,000", "2024", "100th"] {
                assert!(
                    check(&dict, n).is_empty(),
                    "number-like token {n:?} should not be flagged"
                );
            }
        }
        // Phase `tokenize_offsets_round_trip_a_slice`.
        {
            let text = "Hello, world! It's a   test.";
            for tok in tokenize(text) {
                // The recorded span must slice back to exactly the token text.
                assert_eq!(&text[tok.start..tok.end], tok.text);
            }
            // And the specific words we expect are present, in order, with apostrophe.
            let words: Vec<&str> = tokenize(text).iter().map(|t| t.text).collect();
            assert_eq!(words, vec!["Hello", "world", "It's", "a", "test"]);
        }
    }

    #[test]
    fn dictionaries_and_corrections_are_usable_ranked_and_conservative() {
        // Phase `case_insensitive_membership`.
        {
            let dict = builtin();
            // Mixed/upper case of a known word must not be flagged.
            assert!(check(&dict, "The Quick Brown Fox").is_empty());
        }
        // Phase `add_word_removes_a_word_from_check_results`.
        {
            let mut dict = Dictionary::from_words(["the", "quick", "brown", "fox"]);
            // "tilo" is unknown → flagged.
            let before = check(&dict, "the tilo fox");
            assert_eq!(before.len(), 1, "got {before:?}");
            assert_eq!(before[0].word, "tilo");
            // Accept it into the user dictionary.
            assert!(dict.add_word("tilo"));
            // Re-check: no longer flagged.
            let after = check(&dict, "the tilo fox");
            assert!(
                after.is_empty(),
                "expected no misspellings after add_word, got {after:?}"
            );
        }
        // Phase `from_words_builds_a_usable_dictionary`.
        {
            let dict = Dictionary::from_words(["alpha", "beta", "gamma"]);
            assert!(dict.contains("ALPHA"));
            assert!(!dict.contains("delta"));
            let result = check(&dict, "alpha delat");
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].word, "delat");
            // None of the three short, distant words is within edit distance 2.
            assert!(result[0].suggestions.is_empty());
        }
        // Phase `suggestions_are_ranked_by_distance_then_length`.
        {
            // Build a small dictionary where ordering is deterministic and checkable.
            let dict = Dictionary::from_words(["cat", "cart", "cats", "scat", "cataract"]);
            let s = suggest(&dict, "cot", 5);
            // "cat" is distance 1; the 4-letter ones are distance >= 1 too but longer.
            assert_eq!(s.first().map(String::as_str), Some("cat"));
        }
        // Phase `probable_corrections_are_conservative_and_preserve_case`.
        {
            assert_eq!(probable_correction("en_US", "teh").as_deref(), Some("the"));
            assert_eq!(
                probable_correction("en_US", "Wrold").as_deref(),
                Some("World")
            );
            assert_eq!(
                probable_correction("en_US", "TEH"),
                None,
                "all-caps tokens may be acronyms and are never auto-corrected"
            );
            assert_eq!(probable_correction("en_US", "cot"), None);
            assert_eq!(probable_correction("en_US", "the"), None);
            assert_eq!(
                probable_correction("en_US", "adress"),
                Some("address".to_string())
            );
            assert_eq!(
                probable_correction("en_US", "Tomorow"),
                Some("Tomorrow".to_string())
            );
        }
        // Phase `builtin_dictionary_is_substantial`.
        {
            let dict = builtin();
            assert!(
                dict.len() > 10_000,
                "builtin dictionary unexpectedly small: {}",
                dict.len()
            );
        }
    }
}
