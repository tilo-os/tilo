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
/// word, or inside a word where a field of `weight` matches inside words
/// (a name or an identity, the query long enough to be a fragment). A query
/// only beginning a name ([`MIN_ALIAS_SCALARS`]) shows in any other field
/// only as a whole word. `None` where the index finds no match.
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
        let inside_words = infix_allowed(&scalars) && MatchTier::Contains.rank_in(weight).is_some();
        let mut occurrences = folded.match_indices(query.as_str()).map(|(at, _)| at);
        let first = occurrences.next()?;
        if starts_word(first) {
            first
        } else if let Some(at) = occurrences.find(|at| starts_word(*at)) {
            at
        } else if inside_words {
            first
        } else {
            return None;
        }
    };
    // The match covers every raw scalar that contributed a folded byte.
    let start = *origin.get(at)?;
    let last = *origin.get(at.saturating_add(query.len()).saturating_sub(1))?;
    let last_len = text.get(last..)?.chars().next()?.len_utf8();
    Some(start..last.saturating_add(last_len))
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
