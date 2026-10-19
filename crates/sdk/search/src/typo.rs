//! Typo matching against the words an index holds.
//!
//! A typed word matches a title's or a keyword's word within a small edit
//! distance. tantivy's fuzzy query builds a Levenshtein automaton for every
//! query, field and index, which for a long word at distance two costs more
//! than the rest of a keystroke together. The words of a field are few (a
//! palette's apps, actions and pages hold some thousands), so the index keeps
//! them and a query compares against each directly: a distance bounded by
//! the allowed typos, given up as soon as no alignment can stay within it.

use std::collections::BTreeMap;

use crate::index::Document;
use crate::tokenizer::words;
use crate::weight::KEYWORD_SLOT;

/// The distinct folded words of one field, as [`crate::WordTokenizer`]
/// indexes them, each with how many field values hold it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Vocabulary {
    words: BTreeMap<String, (usize, Box<[char]>)>,
}

impl Vocabulary {
    fn add(&mut self, text: &str) {
        for word in words(text) {
            let scalars = word.chars().collect();
            let held = self.words.entry(word).or_insert((0, scalars));
            held.0 = held.0.saturating_add(1);
        }
    }

    fn remove(&mut self, text: &str) {
        for word in words(text) {
            if let Some(held) = self.words.get_mut(&word) {
                held.0 = held.0.saturating_sub(1);
                if held.0 == 0 {
                    self.words.remove(&word);
                }
            }
        }
    }

    /// The words within `distance` edits of `query` (a transposition is one
    /// edit) or, with `prefix`, that begin with such a word: what tantivy's
    /// fuzzy term query, and its prefix form, accept.
    pub(crate) fn near<'a>(
        &'a self,
        query: &'a [char],
        distance: usize,
        prefix: bool,
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.words
            .iter()
            .filter(move |(_, (_, word))| within(query, word, distance, prefix))
            .map(|(text, _)| text.as_str())
    }
}

/// The words typos are matched against: those of title fields and those of
/// keyword fields.
#[derive(Clone, Debug, Default)]
pub(crate) struct Typos {
    pub title: Vocabulary,
    pub keyword: Vocabulary,
}

impl Typos {
    fn vocabulary(&mut self, slot: usize) -> Option<&mut Vocabulary> {
        match slot {
            0 => Some(&mut self.title),
            KEYWORD_SLOT => Some(&mut self.keyword),
            _ => None,
        }
    }

    /// Count the words of `document`'s title and keyword fields.
    pub(crate) fn add(&mut self, document: &Document) {
        for (text, weight) in document.fields() {
            if let Some(vocabulary) = self.vocabulary(weight.slot()) {
                vocabulary.add(text);
            }
        }
    }

    /// Stop counting the words of `document`, as [`Self::add`] counted them.
    pub(crate) fn remove(&mut self, document: &Document) {
        for (text, weight) in document.fields() {
            if let Some(vocabulary) = self.vocabulary(weight.slot()) {
                vocabulary.remove(text);
            }
        }
    }
}

/// Whether `word`, or with `prefix` some prefix of it, is within `distance`
/// optimal-string-alignment edits of `query`.
///
/// The table runs over `word` one scalar per row; a row whose every cell,
/// and the row before it (a transposition reaches back two), exceeds
/// `distance` ends the comparison, since no later row can come back under.
pub(crate) fn within(query: &[char], word: &[char], distance: usize, prefix: bool) -> bool {
    let (n, m) = (query.len(), word.len());
    if prefix {
        if m.saturating_add(distance) < n {
            return false;
        }
    } else if n.abs_diff(m) > distance {
        return false;
    }
    if prefix && n <= distance {
        return true;
    }
    let width = n.saturating_add(1);
    let mut before: Vec<usize> = vec![usize::MAX; width];
    let mut previous: Vec<usize> = (0..width).collect();
    let mut current: Vec<usize> = vec![0; width];
    for (row, &scalar) in word.iter().enumerate() {
        let row = row.saturating_add(1);
        if let Some(first) = current.first_mut() {
            *first = row;
        }
        let mut lowest = row;
        for column in 1..width {
            let (Some(&wanted), Some(&diagonal), Some(&above), Some(&left)) = (
                query.get(column.saturating_sub(1)),
                previous.get(column.saturating_sub(1)),
                previous.get(column),
                current.get(column.saturating_sub(1)),
            ) else {
                continue;
            };
            let mut cell = diagonal
                .saturating_add(usize::from(wanted != scalar))
                .min(above.saturating_add(1))
                .min(left.saturating_add(1));
            if row > 1
                && column > 1
                && word.get(row.saturating_sub(2)) == Some(&wanted)
                && query.get(column.saturating_sub(2)) == Some(&scalar)
                && let Some(&swap) = before.get(column.saturating_sub(2))
            {
                cell = cell.min(swap.saturating_add(1));
            }
            if let Some(slot) = current.get_mut(column) {
                *slot = cell;
            }
            lowest = lowest.min(cell);
        }
        if prefix && current.get(n).is_some_and(|&last| last <= distance) {
            return true;
        }
        let previous_lowest = previous.iter().copied().min().unwrap_or(usize::MAX);
        if lowest > distance && previous_lowest > distance {
            return false;
        }
        std::mem::swap(&mut before, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    !prefix && previous.get(n).is_some_and(|&last| last <= distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(query: &str, word: &str, distance: usize, prefix: bool) -> bool {
        let query: Vec<char> = query.chars().collect();
        let word: Vec<char> = word.chars().collect();
        within(&query, &word, distance, prefix)
    }

    #[test]
    fn edits_and_transpositions_count_one_each() {
        assert!(near("terminl", "terminal", 1, false));
        assert!(near("emial", "email", 1, false), "a swap is one edit");
        assert!(near("calender", "calendar", 1, false));
        assert!(!near("calender", "calendar", 0, false));
        assert!(near("kalendar", "calendar", 1, false));
        assert!(!near("netflix", "network", 2, false));
        assert!(near("einstelungen", "einstellungen", 1, false));
        assert!(!near("abcdef", "uvwxyz", 2, false));
    }

    #[test]
    fn a_prefix_match_accepts_a_word_that_begins_near_the_query() {
        assert!(near("setin", "settings", 1, true));
        assert!(near("yotub", "youtube", 1, true));
        assert!(!near("yotub", "youtube", 1, false));
        assert!(near("posteingnag", "posteingang", 2, true));
        assert!(!near("screenshot", "scr", 2, true));
    }

    /// The table against a plain recursion over every alignment.
    #[test]
    fn the_bounded_table_agrees_with_the_definition() {
        fn osa(a: &[char], b: &[char]) -> usize {
            let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
            for (i, row) in table.iter_mut().enumerate() {
                row[0] = i;
            }
            for (j, cell) in table[0].iter_mut().enumerate() {
                *cell = j;
            }
            for i in 1..=a.len() {
                for j in 1..=b.len() {
                    let cost = usize::from(a[i - 1] != b[j - 1]);
                    let mut cell = (table[i - 1][j] + 1)
                        .min(table[i][j - 1] + 1)
                        .min(table[i - 1][j - 1] + cost);
                    if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                        cell = cell.min(table[i - 2][j - 2] + 1);
                    }
                    table[i][j] = cell;
                }
            }
            table[a.len()][b.len()]
        }
        let words = [
            "screenshot",
            "screen",
            "scheenshot",
            "screensh",
            "einstellungen",
            "einstellung",
            "einstelungen",
            "ab",
            "ba",
            "abc",
            "acb",
            "settings",
            "setings",
            "",
            "x",
        ];
        for query in words {
            for word in words {
                let q: Vec<char> = query.chars().collect();
                let w: Vec<char> = word.chars().collect();
                for distance in 0..=2 {
                    let whole = osa(&q, &w) <= distance;
                    assert_eq!(
                        within(&q, &w, distance, false),
                        whole,
                        "{query} {word} {distance}"
                    );
                    let prefixed = (0..=w.len()).any(|end| osa(&q, &w[..end]) <= distance);
                    assert_eq!(
                        within(&q, &w, distance, true),
                        prefixed,
                        "{query} {word} {distance} prefix"
                    );
                }
            }
        }
    }
}
