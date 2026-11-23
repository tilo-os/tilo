//! Matching and ranking text for a short list a widget filters as a word is
//! typed: folding that ignores case, compatibility forms and alphabetic
//! diacritics, and the order a list's own ranks give.

pub use tilo_search::{
    FieldWeight, Frecency, ListIndex, MAX_FIELD_RANK, Matching, fold as fold_search_text,
    fold_char, initials, locate,
};

/// Fold text the way every index does: case, compatibility forms and alphabetic
/// diacritics (see [`tilo_search::fold()`]).
#[must_use]
pub fn normalize_search_text(text: &str) -> String {
    tilo_search::fold(text)
}

/// The rows of a short list that answer `query`, best first, ranked by `index` the way
/// every list search ranks; rows that rank alike keep the list's order, so a list does
/// not reshuffle while a word is typed.
pub fn ranked_rows<'a, R>(
    index: &ListIndex,
    query: &str,
    rows: impl IntoIterator<Item = R>,
) -> Vec<usize>
where
    R: IntoIterator<Item = (&'a str, FieldWeight)>,
{
    use tilo_sort::CompactSort as _;
    let mut ranked: Vec<(i32, usize)> = index
        .ranks(query, rows)
        .into_iter()
        .enumerate()
        .filter_map(|(at, rank)| rank.map(|rank| (rank, at)))
        .collect();
    ranked.compact_sort_by_key(|&(rank, _)| std::cmp::Reverse(rank));
    ranked.into_iter().map(|(_, at)| at).collect()
}
