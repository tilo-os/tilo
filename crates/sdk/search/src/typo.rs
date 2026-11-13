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
    /// edit), the parts of a dotted name among them.
    pub(crate) fn near<'a>(
        &'a self,
        query: &'a [char],
        distance: usize,
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.words
            .iter()
            .filter(move |(_, (_, word))| matches_word(query, word, distance))
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

/// Shortest typed word whose typo may be a wrong letter. Below it a typo is
/// only a dropped, extra or swapped letter: a short word with one letter
/// changed is most often another real word ("cats" is not "cuts" mistyped),
/// while "nots" is still "notes" with a letter dropped.
const SUBSTITUTION_SCALARS: usize = 5;

/// Whether `word`, or one of the parts of a dotted name it is, is within
/// `distance` edits of `query`: a person remembers "github" of
/// "github.com", and mistypes that. A dot joins letters into one word
/// ([`crate::tokenizer::words`]), so the part is what they are typing.
#[must_use]
pub(crate) fn matches_word(query: &[char], word: &[char], distance: usize) -> bool {
    within(query, word, distance)
        || word
            .split(|scalar| *scalar == '.')
            .any(|part| within(query, part, distance))
}

/// Whether `word` is within `distance` optimal-string-alignment edits of
/// `query`: the same word, mistyped. A query shorter than
/// [`SUBSTITUTION_SCALARS`] pays two edits for a wrong letter.
///
/// A typo is a whole word, never a word merely beginning alike: a prefix of
/// a longer word is what the ranking's prefix tier is for, and letting one
/// edit stand in for it answers every word that starts the same — "hello"
/// would land on "helligkeit" (brightness) and on "hellmodus" (light mode),
/// which are different words, not mistyped ones. For the same reason a word
/// shorter than the query is only a typo of it when it is long enough for a
/// mistyped letter to be a slip at all ("hell" is not "hello" with its last
/// letter left off).
///
/// The table runs over `word` one scalar per row; a row whose every cell,
/// and the row before it (a transposition reaches back two), exceeds
/// `distance` ends the comparison, since no later row can come back under.
pub(crate) fn within(query: &[char], word: &[char], distance: usize) -> bool {
    let (n, m) = (query.len(), word.len());
    if n.abs_diff(m) > distance || (m < n && m < SUBSTITUTION_SCALARS) {
        return false;
    }
    let substitution = if n < SUBSTITUTION_SCALARS { 2 } else { 1 };
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
                .saturating_add(if wanted == scalar { 0 } else { substitution })
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
        if lowest > distance && previous.iter().copied().min().unwrap_or(usize::MAX) > distance {
            return false;
        }
        std::mem::swap(&mut before, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous.get(n).is_some_and(|&last| last <= distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(query: &str, word: &str, distance: usize) -> bool {
        let query: Vec<char> = query.chars().collect();
        let word: Vec<char> = word.chars().collect();
        within(&query, &word, distance)
    }

    #[test]
    fn edits_and_transpositions_count_one_each() {
        assert!(near("terminl", "terminal", 1));
        assert!(near("emial", "email", 1), "a swap is one edit");
        assert!(near("calender", "calendar", 1));
        assert!(!near("calender", "calendar", 0));
        assert!(near("kalendar", "calendar", 1));
        assert!(!near("netflix", "network", 2));
        assert!(near("einstelungen", "einstellungen", 1));
        assert!(!near("abcdef", "uvwxyz", 2));
    }

    #[test]
    fn a_dotted_name_is_mistyped_in_any_of_its_parts() {
        let part = |query: &str, word: &str| {
            let query: Vec<char> = query.chars().collect();
            let word: Vec<char> = word.chars().collect();
            matches_word(&query, &word, 1)
        };
        assert!(part("githbu", "github.com"), "a mistyped host");
        assert!(part("github", "github.com"), "the host typed whole");
        assert!(
            !part("hello", "helligkeit.com"),
            "a part is a whole word too"
        );
        assert!(!part("zzz", "abc.de"));
    }

    #[test]
    fn a_short_word_is_not_another_short_word_with_one_letter_changed() {
        assert!(!near("cats", "cuts", 1));
        assert!(!near("cats", "hats", 1));
        assert!(near("cats", "cast", 1), "a swap is still one edit");
        assert!(near("nots", "notes", 1), "a dropped letter too");
        assert!(near("catz", "cats", 2));
        assert!(near("kittem", "kitten", 1), "longer words keep it");
    }

    #[test]
    fn a_typo_is_a_whole_word_never_one_merely_beginning_alike() {
        // "hello" is a word of its own, not a mistyping of "helligkeit"
        // (brightness) or of "hellmodus" (light mode).
        assert!(!near("hello", "helligkeit", 1));
        assert!(!near("hello", "hellmodus", 1));
        assert!(!near("yotub", "youtube", 1), "still typing, not a typo");
        // A short word is not a longer word with its last letters left off.
        assert!(!near("hello", "hell", 1));
        assert!(!near("helo", "hell", 1));
        // Mistyped words are found however the letters slipped.
        assert!(near("helo", "hello", 1), "a letter dropped");
        assert!(near("helloo", "hello", 1), "a letter doubled");
        assert!(near("terminl", "termin", 1), "one letter too many");
        assert!(near("setings", "settings", 1));
        assert!(near("posteingnag", "posteingang", 2));
        assert!(!near("screenshot", "scr", 2));
    }

    /// The table against a plain recursion over every alignment.
    #[test]
    fn the_bounded_table_agrees_with_the_definition() {
        fn osa(a: &[char], b: &[char]) -> usize {
            let substitution = if a.len() < SUBSTITUTION_SCALARS { 2 } else { 1 };
            let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
            for (i, row) in table.iter_mut().enumerate() {
                row[0] = i;
            }
            for (j, cell) in table[0].iter_mut().enumerate() {
                *cell = j;
            }
            for i in 1..=a.len() {
                for j in 1..=b.len() {
                    let cost = if a[i - 1] == b[j - 1] {
                        0
                    } else {
                        substitution
                    };
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
                    let typo = w.len().abs_diff(q.len()) <= distance
                        && (w.len() >= q.len() || w.len() >= SUBSTITUTION_SCALARS);
                    assert_eq!(
                        within(&q, &w, distance),
                        osa(&q, &w) <= distance && typo,
                        "{query} {word} {distance}"
                    );
                }
            }
        }
    }
}
