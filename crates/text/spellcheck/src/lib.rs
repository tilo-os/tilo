//! The spelling engine behind every Tilo text field.
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
//! English uses the embedded word list. Other languages use the system's
//! Hunspell dictionaries, parsed and checked in-process
//! ([`spans_for_language`], [`suggestions_for_language`]), so checking
//! behaves the same inside an app sandbox as outside it.
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

mod contractions;
mod dictionary;
mod distance;
mod hunspell;
mod suggest;
mod token;

use hunspell::{results as hunspell_results, suggestions as hunspell_suggestions};

pub use contractions::contraction;
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
    for tok in checkable_words(text) {
        if dict.contains(&english_lookup_form(tok.text)) || is_likely_proper_name(text, &tok) {
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
    checkable_words(text)
        .into_iter()
        .filter(|tok| {
            !dict.contains(&english_lookup_form(tok.text)) && !is_likely_proper_name(text, tok)
        })
        .map(|tok| (tok.start, tok.end))
        .collect()
}

/// The words of `text` a checker judges, with their byte offsets: every
/// spellable token, a hyphenated compound split into its parts, which are
/// judged one by one ("ten-minute" is "ten" and "minute"; "well-knwon"
/// flags only "knwon").
fn checkable_words(text: &str) -> Vec<Token<'_>> {
    let mut words = Vec::new();
    for token in tokenize(text) {
        if !is_spellable(token.text) {
            continue;
        }
        let mut start = token.start;
        for part in token.text.split('-') {
            let end = start.saturating_add(part.len());
            if is_spellable(part) {
                words.push(Token {
                    text: part,
                    start,
                    end,
                });
            }
            start = end.saturating_add(1);
        }
    }
    words
}

/// How an English word is looked up: with a typographic apostrophe made
/// plain, and a possessive judged by the word it belongs to ("studio's" is
/// spelled right when "studio" is).
fn english_lookup_form(word: &str) -> Cow<'_, str> {
    let word = normalize_lookup_word(word);
    let stem = word
        .strip_suffix("'s")
        .or_else(|| word.strip_suffix("'S"))
        .filter(|stem| stem.chars().any(char::is_alphabetic))
        .map(str::to_owned);
    match stem {
        Some(stem) => Cow::Owned(stem),
        None => word,
    }
}

/// Which of `words` English spells right: the system's Hunspell dictionary
/// for the language (affix rules, plurals, possessives, which words take a
/// capital), else the embedded word list; the product names and common
/// first names the embedded list adds count in both. `None` while neither
/// is ready.
fn english_results(language: &str, words: &[&str]) -> Option<Vec<bool>> {
    let lookups: Vec<Cow<'_, str>> = words.iter().map(|word| english_lookup_form(word)).collect();
    let lookups: Vec<&str> = lookups.iter().map(AsRef::as_ref).collect();
    // A program building the embedded list in idle steps (a browser page,
    // without threads) never loads a system dictionary on a keystroke.
    let system = if BUILT_IN_STEPS.load(Ordering::Acquire) {
        None
    } else {
        hunspell_results(english_dictionary_name(language), &lookups)
    };
    let known = if let Some(known) = system {
        known
    } else {
        let english = shared_english()?;
        lookups.iter().map(|word| english.contains(word)).collect()
    };
    Some(
        known
            .into_iter()
            .zip(&lookups)
            .map(|(known, word)| known || dictionary::is_extra_word(word))
            .collect(),
    )
}

/// The Hunspell dictionary English is checked with: the language's own
/// region, and American English for a bare `en`.
fn english_dictionary_name(language: &str) -> &str {
    if language.len() <= 2 {
        "en_US"
    } else {
        language
    }
}

/// Spell-check text in a BCP-47-ish language such as en-US or de-DE.
///
/// English uses the embedded dictionary (its expanded forms plus a
/// conversational overlay), so it works in hermetic builds too. Other
/// languages use the system's Hunspell dictionary for that language, checked
/// in-process with its affix rules; without one, nothing is flagged. Nothing
/// is flagged either while the dictionaries' code is still to come (the
/// `lazy-dictionaries` feature).
pub fn spans_for_language(language: &str, text: &str) -> Vec<(usize, usize)> {
    loading::dictionaries().map_or_else(Vec::new, |dictionaries| {
        (dictionaries.spans)(language, text)
    })
}

fn dictionary_spans(language: &str, text: &str) -> Vec<(usize, usize)> {
    let tokens = checkable_words(text);
    if tokens.is_empty() {
        return Vec::new();
    }
    if is_english(language) {
        let words: Vec<&str> = tokens.iter().map(|token| token.text).collect();
        let Some(correct) = english_results(language, &words) else {
            return Vec::new();
        };
        return tokens
            .into_iter()
            .zip(correct)
            .filter(|(token, correct)| !correct && !is_likely_proper_name(text, token))
            .map(|(token, _)| (token.start, token.end))
            .collect();
    }

    // Hunspell dictionaries conventionally use ASCII apostrophes even when a
    // typographic editor inserts U+2019. Normalise only for lookup; token byte
    // ranges still refer to the untouched source text.
    let lookup_words: Vec<Cow<'_, str>> = tokens
        .iter()
        .map(|token| normalize_lookup_word(token.text))
        .collect();
    let words: Vec<&str> = lookup_words.iter().map(AsRef::as_ref).collect();
    let Some(correct) = hunspell_results(language, &words) else {
        return Vec::new();
    };
    tokens
        .into_iter()
        .zip(correct)
        .filter_map(|(token, is_correct)| (!is_correct).then_some((token.start, token.end)))
        .collect()
}

/// Corrections for one word, best first: the embedded edit-distance
/// suggester for English, the system Hunspell dictionary otherwise. None
/// while the dictionaries' code is still to come.
pub fn suggestions_for_language(language: &str, word: &str, max: usize) -> Vec<String> {
    if word.is_empty() || max == 0 {
        return Vec::new();
    }
    loading::dictionaries().map_or_else(Vec::new, |dictionaries| {
        (dictionaries.suggestions)(language, word, max)
    })
}

fn dictionary_suggestions(language: &str, word: &str, max: usize) -> Vec<String> {
    if is_english(language) {
        return shared_english().map_or_else(Vec::new, |english| suggest(english, word, max));
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
/// known high-frequency typos and English contractions typed without their
/// apostrophe ([`contraction`]) are safe-listed, while the generic path accepts
/// only an adjacent transposition matching the dictionary's first candidate.
pub fn probable_correction(language: &str, word: &str) -> Option<String> {
    if let Some(candidate) = sure_correction(language, word) {
        return Some(candidate);
    }
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
        contraction(&lower).or(match lower.as_str() {
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
        })
    } else {
        None
    };
    // Curated corrections are deliberately independent of dictionary startup,
    // so their preview and boundary replacement are instant on the first word.
    if let Some(candidate) = known {
        return Some(match_word_case(word, candidate));
    }
    (loading::dictionaries()?.correction)(language, word, &lower)
}

/// A correction sure enough to apply although the word list holds the
/// typed word: the English pronoun in lowercase ("i" is "I", "im" is "I'm")
/// and "cant" / "wont", which people type for "can't" / "won't" far more
/// often than they mean the rare nouns (as phone keyboards assume). Word
/// lists are lowercase and hold "i", "cant" and "wont", and "i" / "im" are
/// too short for [`probable_correction`]'s length floor. Backspace right
/// after a correction takes it back.
#[must_use]
pub fn sure_correction(language: &str, word: &str) -> Option<String> {
    if !is_english(language) {
        return None;
    }
    let candidate = match word {
        "i" => "I",
        "im" | "Im" => "I'm",
        _ => match word.to_lowercase().as_str() {
            "cant" => "can't",
            "wont" => "won't",
            _ => return None,
        },
    };
    Some(match word {
        "i" | "im" | "Im" => candidate.to_owned(),
        _ => match_word_case(word, candidate),
    })
}

/// [`probable_correction`] past the curated list: an adjacent
/// transposition the dictionary agrees on.
fn dictionary_correction(language: &str, word: &str, lower: &str) -> Option<String> {
    if dictionary_spans(language, word).is_empty() {
        return None;
    }
    if is_english(language) {
        let candidate = unique_adjacent_transposition(shared_english()?, lower)?;
        return Some(match_word_case(word, &candidate));
    }
    let candidate = dictionary_suggestions(language, word, 1)
        .into_iter()
        .next()
        .filter(|candidate| is_adjacent_transposition(lower, &candidate.to_lowercase()))?;
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

/// The checks that need a language's dictionary: the embedded English
/// list and the system's Hunspell dictionaries.
#[derive(Clone, Copy)]
pub struct Dictionaries {
    spans: fn(&str, &str) -> Vec<(usize, usize)>,
    suggestions: fn(&str, &str, usize) -> Vec<String>,
    correction: fn(&str, &str, &str) -> Option<String>,
    prepare: fn(&mut dyn FnMut() -> bool) -> bool,
}

/// Hands over [`Dictionaries`], for `install_dictionaries`.
pub type MakeDictionaries = fn() -> Dictionaries;

/// The dictionaries this crate checks with.
#[must_use]
pub fn builtin_dictionaries() -> Dictionaries {
    Dictionaries {
        spans: dictionary_spans,
        suggestions: dictionary_suggestions,
        correction: dictionary_correction,
        prepare: prepare_english,
    }
}

#[cfg(not(feature = "lazy-dictionaries"))]
mod loading {
    use super::Dictionaries;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the lazy-dictionaries build has none until they are fetched"
    )]
    pub(crate) fn dictionaries() -> Option<Dictionaries> {
        Some(super::builtin_dictionaries())
    }

    /// How many times the dictionaries arrived: never, as they were always
    /// there.
    pub(crate) fn loaded_generation() -> u64 {
        0
    }
}

/// The dictionaries are fetched the first time something checks spelling:
/// the program hands over a loader ([`defer_dictionaries`]) that fetches
/// their code and passes back [`builtin_dictionaries`]
/// ([`install_dictionaries`]). Until then nothing is flagged or suggested;
/// a field checks again once [`dictionaries_generation`] moved.
#[cfg(feature = "lazy-dictionaries")]
mod loading {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::{Dictionaries, MakeDictionaries};

    static DICTIONARIES: OnceLock<Dictionaries> = OnceLock::new();
    static LOAD: OnceLock<fn()> = OnceLock::new();
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    /// The dictionaries, or `None` while their code is still to come (and
    /// then fetched).
    pub(crate) fn dictionaries() -> Option<Dictionaries> {
        let dictionaries = DICTIONARIES.get().copied();
        if dictionaries.is_none() && !REQUESTED.swap(true, Ordering::AcqRel) {
            match LOAD.get() {
                Some(load) => load(),
                None => REQUESTED.store(false, Ordering::Release),
            }
        }
        dictionaries
    }

    /// Bumped when the dictionaries arrive.
    pub(crate) fn loaded_generation() -> u64 {
        GENERATION.load(Ordering::Acquire)
    }

    /// How the program fetches the dictionaries' code: `load` starts the
    /// fetch and, once the code is in, calls [`install_dictionaries`] (or
    /// [`dictionaries_load_failed`]).
    pub fn defer_dictionaries(load: fn()) {
        let _ = LOAD.set(load);
    }

    /// The dictionaries' code is in: spelling is checked from now on.
    pub fn install_dictionaries(make: MakeDictionaries) {
        if DICTIONARIES.set(make()).is_ok() {
            GENERATION.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The fetch failed: the next check fetches again.
    pub fn dictionaries_load_failed() {
        REQUESTED.store(false, Ordering::Release);
    }
}

#[cfg(feature = "lazy-dictionaries")]
pub use loading::{defer_dictionaries, dictionaries_load_failed, install_dictionaries};

/// Bumped when the dictionaries arrive, or the English one is built in steps
/// ([`prepare_dictionaries`]): a field that checked before checks again once
/// this moved.
#[must_use]
pub fn dictionaries_generation() -> u64 {
    loading::loaded_generation().wrapping_add(ENGLISH_GENERATION.load(Ordering::Acquire))
}

static ENGLISH: OnceLock<Dictionary> = OnceLock::new();
static ENGLISH_GENERATION: AtomicU64 = AtomicU64::new(0);
static BUILT_IN_STEPS: AtomicBool = AtomicBool::new(false);

/// The program builds the English dictionary in steps, in its idle time
/// ([`prepare_dictionaries`]), instead of on the first check: a check before
/// it is built flags nothing, and the fields check again once it is.
///
/// For a browser page, which has no thread to build it on: built on the
/// first check, its 89,000 words held the keystroke that asked for well
/// over a hundred milliseconds.
pub fn build_dictionaries_in_steps() {
    BUILT_IN_STEPS.store(true, Ordering::Release);
}

/// Build the English dictionary while `more` answers true. Fetches the
/// dictionaries' code first when it is still to come (`None` until it is
/// in); then true once the dictionary is built.
pub fn prepare_dictionaries(more: &mut dyn FnMut() -> bool) -> Option<bool> {
    loading::dictionaries().map(|dictionaries| (dictionaries.prepare)(more))
}

fn prepare_english(more: &mut dyn FnMut() -> bool) -> bool {
    static BUILDER: Mutex<Option<dictionary::BuiltinBuilder>> = Mutex::new(None);
    if ENGLISH.get().is_some() {
        return true;
    }
    let Ok(mut builder) = BUILDER.lock() else {
        return false;
    };
    if !builder.get_or_insert_with(Default::default).step(more) {
        return false;
    }
    if let Some(built) = builder.take()
        && ENGLISH.set(built.finish()).is_ok()
    {
        ENGLISH_GENERATION.fetch_add(1, Ordering::AcqRel);
    }
    true
}

/// The English dictionary, built on first use unless the program builds it
/// in steps ([`build_dictionaries_in_steps`]); then `None` until it is.
fn shared_english() -> Option<&'static Dictionary> {
    if BUILT_IN_STEPS.load(Ordering::Acquire) {
        return ENGLISH.get();
    }
    Some(ENGLISH.get_or_init(Dictionary::builtin))
}

/// Begin loading `language`'s dictionary before the first field is focused.
/// The background warm-up is best-effort: callers can still use the checker
/// immediately and wait on the same `OnceLock` only if they beat this head
/// start. Each language warms once.
pub fn warm_up_language(language: &str) {
    static STARTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    // A browser page has no threads; its first check builds the data.
    if cfg!(target_family = "wasm") {
        return;
    }
    let key = if is_english(language) {
        "en".to_owned()
    } else {
        match normalize_language(language) {
            Some(name) => name,
            None => return,
        }
    };
    let Ok(mut started) = STARTED.get_or_init(Mutex::default).lock() else {
        return;
    };
    if !started.insert(key) {
        return;
    }
    let language = language.to_owned();
    let _ = std::thread::Builder::new()
        .name("spellcheck-warmup".to_string())
        .spawn(move || {
            if is_english(&language) {
                if !hunspell::warm_up(english_dictionary_name(&language)) {
                    let _ = shared_english();
                }
            } else {
                hunspell::warm_up(&language);
            }
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
/// useful than guessing at a correction. A sentence-initial word carries no
/// such signal and is still checked, unless the next word is title-cased
/// too: "Lena Hoffmann" opening a line is a name.
fn is_likely_proper_name(text: &str, token: &Token<'_>) -> bool {
    if !is_title_case(token.text) {
        return false;
    }
    if opens_a_name(text, token) {
        return true;
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

/// Whether `word` is capitalised and otherwise lowercase ("Lena").
fn is_title_case(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(char::is_uppercase) && chars.all(|character| !character.is_uppercase())
}

/// Whether the word right after `token`, on the same line, is title-cased:
/// the two are a name ("Lena Hoffmann", "Northwind Studio").
fn opens_a_name(text: &str, token: &Token<'_>) -> bool {
    let Some(rest) = text.get(token.end..) else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t', '\u{a0}']);
    let next: String = rest
        .chars()
        .take_while(|character| character.is_alphabetic() || matches!(character, '\'' | '\u{2019}'))
        .collect();
    next.chars()
        .filter(|character| character.is_alphabetic())
        .count()
        >= 2
        && is_title_case(&next)
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
        {
            let dict = builtin();
            let text = "the quick brown fox jumps over the lazy dog";
            let result = check(&dict, text);
            assert!(
                result.is_empty(),
                "expected zero misspellings, got {result:?}"
            );
        }
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
        {
            let dict = builtin();
            assert!(spans(&dict, "I met Qwordz today").is_empty());
            assert_eq!(spans(&dict, "Qwordz arrived"), vec![(0, 6)]);
            assert_eq!(spans(&dict, "Hello. Qwordz arrived"), vec![(7, 13)]);
            assert_eq!(spans(&dict, "Hello\nQwordz arrived"), vec![(6, 12)]);
        }
        {
            let dict = builtin();
            assert!(spans(&dict, "TextInput iPhone GitHub").is_empty());
        }
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
        {
            let dict = builtin();
            assert!(check(&dict, "NASA").is_empty());
            assert!(check(&dict, "HTML").is_empty());
            // A single capital is a real one-letter word, still checked (and valid).
            assert!(check(&dict, "I").is_empty());
        }
        {
            let dict = builtin();
            for n in ["0", "42", "3.14", "1,000", "2024", "100th"] {
                assert!(
                    check(&dict, n).is_empty(),
                    "number-like token {n:?} should not be flagged"
                );
            }
        }
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
        {
            let dict = builtin();
            // Mixed/upper case of a known word must not be flagged.
            assert!(check(&dict, "The Quick Brown Fox").is_empty());
        }
        {
            let mut dict = Dictionary::from_words(["the", "quick", "brown", "fox"]);
            // "tilo" is unknown → flagged.
            let before = check(&dict, "the tilo fox");
            assert_eq!(before.len(), 1, "got {before:?}");
            assert_eq!(before[0].word, "tilo");
            assert!(dict.add_word("tilo"));
            // Re-check: no longer flagged.
            let after = check(&dict, "the tilo fox");
            assert!(
                after.is_empty(),
                "expected no misspellings after add_word, got {after:?}"
            );
        }
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
        {
            // Build a small dictionary where ordering is deterministic and checkable.
            let dict = Dictionary::from_words(["cat", "cart", "cats", "scat", "cataract"]);
            let s = suggest(&dict, "cot", 5);
            // "cat" is distance 1; the 4-letter ones are distance >= 1 too but longer.
            assert_eq!(s.first().map(String::as_str), Some("cat"));
        }
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
            // A contraction typed without its apostrophe gets it back;
            // a bare form that is a word of its own stays.
            for (typed, corrected) in [
                ("doesnt", Some("doesn't")),
                ("Doesnt", Some("Doesn't")),
                ("ive", Some("I've")),
                ("im", Some("I'm")),
                ("Im", Some("I'm")),
                ("i", Some("I")),
                ("I", None),
                ("IM", None),
                ("cant", Some("can't")),
                ("Wont", Some("Won't")),
            ] {
                assert_eq!(
                    probable_correction("en_US", typed).as_deref(),
                    corrected,
                    "{typed}"
                );
            }
            assert_eq!(probable_correction("fr_FR", "doesnt"), None);
        }
        {
            let dict = builtin();
            assert!(
                dict.len() > 10_000,
                "builtin dictionary unexpectedly small: {}",
                dict.len()
            );
        }
    }

    #[test]
    fn correct_words_names_compounds_and_possessives_are_not_flagged() {
        let letter = "Dear Ms Okafor,\n\
            I saw the position advertised on your website last week. \
            Your studio\u{2019}s work is the reason I am writing; the office is a ten-minute walk away.\n\
            Yours sincerely,\n\
            Lena Hoffmann";
        let dict = builtin();
        assert!(
            spans(&dict, letter).is_empty(),
            "{:?}",
            spans(&dict, letter)
        );
        assert!(
            spans_for_language("en_US", letter).is_empty(),
            "{:?}",
            spans_for_language("en_US", letter)
        );
        assert!(spans(&dict, "Website first. Minutes later.").is_empty());

        // A compound is judged by its parts: only the misspelled part is marked.
        let compound = "a well-knwon fact";
        assert_eq!(spans(&dict, compound), vec![(7, 12)]);
        assert_eq!(&compound[7..12], "knwon");
        // A possessive is judged by its word, so a misspelled one still shows.
        assert_eq!(spans(&dict, "the wrold's end"), vec![(4, 11)]);
        // A capitalised misspelling opening a sentence is still marked; a run
        // of capitalised words opening a line is a name.
        assert_eq!(spans(&dict, "Wrold peace"), vec![(0, 5)]);
        assert!(spans(&dict, "Qwordz Brightwater sent it").is_empty());
    }
}
