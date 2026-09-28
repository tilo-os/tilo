//! Name/keyword search for [`Catalogue::search`] and [`Catalogue::suggest`].

use tilo_search::{Document, FieldWeight, MatchTier, Matching, Page, SearchIndex, Signals, fold};

use crate::{Catalogue, Emoji};

/// How a query meets an emoji's name: the whole name, then a whole word of
/// it wherever it sits ("heart" in "red heart"), then the start of a longer
/// word ("heart" in "hearts"), then anywhere. Keywords rank below any name
/// match.
fn name_rank(tier: MatchTier, weight: FieldWeight) -> Option<i32> {
    if weight != FieldWeight::TITLE {
        return tier.rank_in(weight);
    }
    Some(match tier {
        MatchTier::Exact => 100,
        MatchTier::PrefixWord | MatchTier::Word => WHOLE_WORD,
        MatchTier::Prefix | MatchTier::WordPrefix => 70,
        MatchTier::Contains => 60,
        MatchTier::Spanning | MatchTier::Typo => tier.percent(),
    })
}

/// The rank of a name holding the query as a whole word.
const WHOLE_WORD: i32 = 90;

/// Longest name length that still orders equally ranked hits.
const NAME_LENGTH_STEPS: i32 = 64;

/// Among names holding the query as a whole word, the shorter is the closer
/// one: "red heart" before "heart with arrow". Other ties keep Unicode
/// order, which lists the common faces and symbols first.
fn score(signals: Signals) -> i32 {
    let rank = signals.rank.unwrap_or(0);
    let length = if rank == WHOLE_WORD {
        i32::try_from(signals.length)
            .unwrap_or(i32::MAX)
            .min(NAME_LENGTH_STEPS.saturating_sub(1))
    } else {
        0
    };
    rank.saturating_mul(NAME_LENGTH_STEPS)
        .saturating_sub(length)
}

/// Every emoji's name and keywords, keyed by catalogue position. Each word
/// of a query must match the name or a keyword, or a name word with a typo.
pub(crate) fn index(emoji: &[Emoji]) -> SearchIndex {
    let mut index = SearchIndex::new(Matching::LOOSE).with_ranks(name_rank);
    index.replace_all(emoji.iter().enumerate().map(|(at, emoji)| {
        let length = u32::try_from(emoji.name().chars().count()).unwrap_or(u32::MAX);
        emoji
            .keywords()
            .fold(
                Document::new(at as u64).field(emoji.name(), FieldWeight::TITLE),
                |document, keyword| document.field(keyword, FieldWeight::METADATA),
            )
            .signals(0, length)
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index emoji names");
    }
    index
}

/// Catalogue positions matching `query`, best first.
fn positions(cat: &Catalogue, query: &str, limit: usize) -> impl Iterator<Item = usize> {
    cat.search_index()
        .search_scored(query, Page::first(limit), score)
        .hits
        .into_iter()
        .filter_map(|hit| usize::try_from(hit.key).ok())
}

pub(crate) fn search<'a>(cat: &'a Catalogue, query: &str, limit: usize) -> Vec<&'a Emoji> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<usize> = positions(cat, query, limit).collect();
    let english = Catalogue::english();
    if found.len() < limit && !std::ptr::eq(cat, english) {
        // Every catalogue shares one structure, so a position names the same
        // emoji in both; English only adds what the native names missed.
        let extra: Vec<usize> = positions(english, query, limit.saturating_add(found.len()))
            .filter(|at| !found.contains(at))
            .collect();
        found.extend(extra);
    }
    found
        .into_iter()
        .filter_map(|at| cat.emoji.get(at))
        .take(limit)
        .collect()
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '+' || c == '\''))
        .filter(|word| !word.is_empty())
}

/// How exactly `emoji` stands for folded `word`: its whole name, a whole
/// keyword, or one word of its name.
fn stands_for(emoji: &Emoji, word: &str) -> Option<u8> {
    let name = fold(emoji.name());
    if name == word {
        Some(3)
    } else if emoji.keywords().any(|keyword| fold(keyword) == word) {
        Some(2)
    } else {
        words(&name).any(|name_word| name_word == word).then_some(1)
    }
}

pub(crate) fn suggest<'a>(cat: &'a Catalogue, word: &str, limit: usize) -> Vec<&'a Emoji> {
    let word = fold(word.trim());
    if word.chars().count() < 3 || word.contains(char::is_whitespace) {
        return Vec::new();
    }
    let exact = |word: &str| -> Vec<&'a Emoji> {
        let mut found: Vec<(u8, &'a Emoji)> = positions(cat, word, usize::MAX)
            .filter_map(|at| cat.emoji.get(at))
            .filter_map(|emoji| Some((stands_for(emoji, word)?, emoji)))
            .collect();
        // Stable: equally exact matches keep the index's order.
        found.sort_by_key(|(exactness, _)| std::cmp::Reverse(*exactness));
        found
            .into_iter()
            .map(|(_, emoji)| emoji)
            .take(limit)
            .collect()
    };
    let found = exact(&word);
    match word.strip_suffix('s') {
        Some(singular) if found.is_empty() => exact(singular),
        _ => found,
    }
}
