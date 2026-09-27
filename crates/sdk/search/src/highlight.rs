//! Where a hit's text shows the query, for quoting it in a result row.
//!
//! The index finds the hit; this only points at the match in text already
//! found, folding it the way the index does.

use std::ops::Range;

use crate::fold::{fold, fold_char};

/// The byte range in raw `text` of the best occurrence of `query` under
/// folding: the first one starting a word, else the first. `None` when the
/// query is empty or absent.
#[must_use]
pub fn locate(query: &str, text: &str) -> Option<Range<usize>> {
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
    let starts_word = |at: usize| {
        folded
            .get(..at)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|previous| !previous.is_alphanumeric())
    };
    let mut occurrences = folded.match_indices(query.as_str()).map(|(at, _)| at);
    let first = occurrences.next()?;
    let at = if starts_word(first) {
        first
    } else {
        occurrences.find(|at| starts_word(*at)).unwrap_or(first)
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
        assert_eq!(
            locate("creme", text).and_then(|range| text.get(range)),
            Some("Crème")
        );
        assert_eq!(
            locate("afé", text).and_then(|range| text.get(range)),
            Some("afé")
        );
        assert_eq!(locate("dun", "redundant dune"), Some(10..13));
        assert_eq!(locate("zzz", text), None);
        assert_eq!(locate("", text), None);
    }
}
