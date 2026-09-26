//! Correction suggestions via a bounded edit-distance scan.

use crate::dictionary::Dictionary;
use crate::distance::{damerau_levenshtein_within, weighted_edit_cost};

/// Largest edit distance a suggestion may sit from the queried word.
const MAX_EDIT_DISTANCE: usize = 2;

/// Suggest up to `max` corrections for `word`, drawn from `dict`.
///
/// Candidates are every dictionary word within Damerau/Levenshtein distance
/// `<= 2` of `word` (compared case-insensitively). They are ranked by:
///
/// 1. a *weighted* edit cost, ascending — the same integer distance, but with a
///    transposition charged slightly less than a substitution/indel, so the most
///    common typos (e.g. "teh" -> "the") float to the top of an otherwise
///    tied group, then
/// 2. word length, ascending (prefer the shorter, more common-looking word),
///    then
/// 3. lexicographic order, for a stable, deterministic result.
///
/// The scan is bounded two ways: the dictionary is consulted only for words
/// whose length is within `+-2` of the query (so e.g. a 12-letter typo never
/// compares against 3-letter words), and the distance function itself bails out
/// the moment a comparison exceeds the bound. That keeps a full-dictionary scan
/// fast enough for interactive use even on the ~75k-word builtin list.
///
/// If `word` is already in the dictionary it is *not* returned as its own
/// suggestion. An empty `word` or `max == 0` yields an empty list.
pub fn suggest(dict: &Dictionary, word: &str, max: usize) -> Vec<String> {
    if max == 0 || word.is_empty() {
        return Vec::new();
    }

    let needle = word.to_lowercase();
    let needle_len = needle.chars().count();
    let min_len = needle_len.saturating_sub(MAX_EDIT_DISTANCE);
    let max_len = needle_len.saturating_add(MAX_EDIT_DISTANCE);

    // (weighted_cost, length, word) — collected then sorted by the ranking above.
    let mut scored: Vec<(f64, usize, &str)> = Vec::new();

    for cand in dict.words_in_len_range(min_len, max_len) {
        if cand == needle {
            continue;
        }
        // Integer bound gates membership; weighted cost only orders the matches.
        if damerau_levenshtein_within(&needle, cand, MAX_EDIT_DISTANCE).is_some() {
            let cost = weighted_edit_cost(&needle, cand, MAX_EDIT_DISTANCE);
            scored.push((cost, cand.chars().count(), cand));
        }
    }

    scored.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(b.2))
    });

    scored
        .into_iter()
        .take(max)
        .map(|(_, _, w)| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_are_near_bounded_ranked_and_case_insensitive() {
        // Phase `suggests_the_for_teh`.
        {
            let d = Dictionary::from_words(["the", "they", "then", "tea", "hen"]);
            let s = suggest(&d, "teh", 5);
            assert!(s.contains(&"the".to_string()), "got {s:?}");
            // "the" (transposition, dist 1) should outrank longer dist-1/2 words.
            assert_eq!(s.first().map(String::as_str), Some("the"));
        }
        // Phase `empty_inputs`.
        {
            let d = Dictionary::from_words(["word"]);
            assert!(suggest(&d, "", 5).is_empty());
            assert!(suggest(&d, "word", 0).is_empty());
        }
        // Phase `does_not_suggest_itself`.
        {
            let d = Dictionary::from_words(["hello", "hella", "hullo"]);
            let s = suggest(&d, "hello", 5);
            assert!(!s.contains(&"hello".to_string()));
        }
        // Phase `respects_max`.
        {
            let d = Dictionary::from_words(["cat", "bat", "rat", "hat", "mat", "pat"]);
            let s = suggest(&d, "cat", 3);
            assert_eq!(s.len(), 3);
        }
        // Phase `far_words_excluded`.
        {
            let d = Dictionary::from_words(["cat", "elephant"]);
            let s = suggest(&d, "cot", 5);
            assert_eq!(s, vec!["cat".to_string()]);
        }
        // Phase `case_insensitive_query`.
        {
            let d = Dictionary::from_words(["world"]);
            let s = suggest(&d, "Wrold", 5);
            assert_eq!(s, vec!["world".to_string()]);
        }
    }
}
