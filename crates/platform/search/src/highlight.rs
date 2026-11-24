//! Where a hit's text shows the query, for quoting it in a result row.
//!
//! The index finds the hit; this only points at the match in text already
//! found, folding it the way the index does.

use std::ops::Range;

use crate::fold::{fold, fold_char};
use crate::query::{infix_allowed, names_only};
use crate::weight::{FieldWeight, MatchTier};

/// The byte range in raw `text`, a field of `weight`, of the occurrence of
/// `query` the index matches there under folding: the first one starting a
/// word, or inside a word where a field of `weight` matches inside words (a
/// name or an identity, the query long enough to be a fragment, or a script
/// written without word spaces). A query only beginning a name
/// ([`MIN_ALIAS_SCALARS`]) shows in any other field only as a whole word.
/// `None` where the index finds no match.
///
/// [`MIN_ALIAS_SCALARS`]: crate::MIN_ALIAS_SCALARS
#[must_use]
pub fn locate(query: &str, text: &str, weight: FieldWeight) -> Option<Range<usize>> {
    let query = fold(query.trim());
    if query.is_empty() {
        return None;
    }
    // Folded text alongside the raw byte offset each folded byte came from.
    let mut folded = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len());
    for (offset, scalar) in text.char_indices() {
        fold_char(scalar, &mut |part| {
            folded.push(part);
            origin.resize(folded.len(), offset);
        });
    }
    let scalars: Vec<char> = query.chars().collect();
    // Punctuation the query starts or ends with is a word's edge itself.
    let opens = scalars
        .first()
        .is_some_and(|first| !first.is_alphanumeric());
    let closes = scalars.last().is_some_and(|last| !last.is_alphanumeric());
    let starts_word = |at: usize| {
        opens
            || folded
                .get(..at)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|previous| !previous.is_alphanumeric())
    };
    let ends_word = |at: usize| {
        closes
            || folded
                .get(at.saturating_add(query.len())..)
                .and_then(|after| after.chars().next())
                .is_none_or(|next| !next.is_alphanumeric())
    };
    let at = if names_only(&scalars) && weight != FieldWeight::TITLE {
        folded
            .match_indices(query.as_str())
            .map(|(at, _)| at)
            .find(|at| starts_word(*at) && ends_word(*at))?
    } else {
        let inside_run = infix_allowed(&scalars) && MatchTier::Contains.rank_in(weight).is_some();
        let mut occurrences = folded.match_indices(query.as_str()).map(|(at, _)| at);
        let first = occurrences.next()?;
        let at = if starts_word(first) {
            first
        } else if let Some(at) = occurrences.find(|at| starts_word(*at)) {
            at
        } else if inside_run {
            first
        } else {
            return None;
        };
        // The tier this occurrence is, so a field that refuses it in the ranking
        // (`rank_in`) is not marked: a highlight shows exactly what a search found.
        let field_start = at == 0;
        let tier = if !starts_word(at) {
            MatchTier::Contains
        } else if ends_word(at) {
            if field_start {
                MatchTier::PrefixWord
            } else {
                MatchTier::Word
            }
        } else if field_start {
            MatchTier::Prefix
        } else {
            MatchTier::WordPrefix
        };
        tier.rank_in(weight)?;
        at
    };
    // The match covers every raw scalar that contributed a folded byte.
    let start = *origin.get(at)?;
    let last = *origin.get(at.saturating_add(query.len()).saturating_sub(1))?;
    let last_len = text.get(last..)?.chars().next()?.len_utf8();
    Some(start..last.saturating_add(last_len))
}

/// How far back from where it would start an excerpt looks for a word's
/// start.
const EXCERPT_REACH: usize = 96;

/// How many characters an excerpt shows in all.
const EXCERPT_LENGTH: usize = 180;

/// The part of `passage` around the first word of `query` it holds, the
/// match in its middle, cut at word boundaries and marked where it was cut:
/// what a hit inside prose shows of it.
#[must_use]
pub fn excerpt(passage: &str, query: &str) -> String {
    let chars: Vec<(usize, char)> = passage.char_indices().collect();
    // The passage folded a character at a time, each folded character
    // pointing back at the one it came from.
    let mut folded = String::new();
    let mut origin = Vec::new();
    for (at, (_, ch)) in chars.iter().enumerate() {
        fold_char(*ch, &mut |folded_char| {
            folded.push(folded_char);
            origin.extend(std::iter::repeat_n(at, folded_char.len_utf8()));
        });
    }
    let (found, word_len) = query
        .split_whitespace()
        .map(fold)
        .filter(|word| !word.is_empty() && !word.contains(':'))
        .find_map(|word| {
            let byte = folded.find(&word)?;
            Some((*origin.get(byte)?, word.chars().count()))
        })
        .unwrap_or((0, 0));
    excerpt_around(passage, found, word_len)
}

/// The part of `passage` around the `len` characters at character `at`, them in its middle, cut at
/// word boundaries and marked where it was cut.
#[must_use]
pub fn excerpt_around(passage: &str, at: usize, len: usize) -> String {
    let chars: Vec<(usize, char)> = passage.char_indices().collect();
    // The match in the middle, or as near it as the passage's ends allow.
    let before = EXCERPT_LENGTH.saturating_sub(len) / 2;
    let mut start = at
        .saturating_sub(before)
        .min(chars.len().saturating_sub(EXCERPT_LENGTH));
    let planned = start;
    // Back to the word's start, unless the word runs on for long.
    while start > 0
        && planned.saturating_sub(start) < EXCERPT_REACH
        && !chars
            .get(start.saturating_sub(1))
            .is_some_and(|(_, ch)| ch.is_whitespace())
    {
        start = start.saturating_sub(1);
    }
    let mut end = start.saturating_add(EXCERPT_LENGTH).min(chars.len());
    while end < chars.len() && !chars.get(end).is_some_and(|(_, ch)| ch.is_whitespace()) {
        end = end.saturating_add(1);
    }
    let byte = |at: usize| chars.get(at).map_or(passage.len(), |(byte, _)| *byte);
    let mut shown = passage
        .get(byte(start)..byte(end))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if start > 0 {
        shown.insert(0, '\u{2026}');
    }
    if end < chars.len() {
        shown.push('\u{2026}');
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_the_raw_range_of_a_folded_match() {
        let text = "Le Café Crème, déjà vu";
        let title = |query: &str, text: &str| locate(query, text, FieldWeight::TITLE);
        assert_eq!(
            title("creme", text).and_then(|range| text.get(range)),
            Some("Crème")
        );
        assert_eq!(title("dun", "redundant dune"), Some(10..13));
        assert_eq!(title("zzz", text), None);
        assert_eq!(title("", text), None);
    }
}
