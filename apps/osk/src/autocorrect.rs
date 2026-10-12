//! Word-boundary autocorrection.
//!
//! When a word ends (space or punctuation) and it is not a word the keyboard
//! or the user knows, [`Autocorrector::correct`] proposes the most likely
//! intended word, or nothing when no candidate is convincing enough.
//!
//! * Candidates come from a SymSpell-style index of single-character deletes
//!   over the lexicon, which finds every word within one insertion, deletion,
//!   substitution or transposition of the typed word without scanning.
//! * Each candidate is scored with a weighted edit distance
//!   ([`edit_cost`]) where substituting a physically adjacent key
//!   ([`KeyAdjacency`], derived from the on-screen key rectangles) or swapping
//!   two neighbouring letters is cheaper than an arbitrary edit.
//! * A candidate wins only if it clearly dominates both the other candidates
//!   and keeping the word as typed, whose weight grows with the typed word's
//!   own frequency when it is in the lexicon.
//!
//! Words the user has explicitly kept (by reverting a correction) are never
//! corrected again. Everything here is pure: no widget, no I/O.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use vello::kurbo::Rect;

use crate::dict::{Dictionary, weight_to_count};
use crate::layout::{KeyboardLayout, ShiftState, key_rects};

/// Cost of substituting a key for one of its physical neighbours.
pub const ADJACENT_SUBSTITUTION_COST: f64 = 0.5;
/// Cost of swapping two neighbouring letters.
pub const TRANSPOSITION_COST: f64 = 0.7;
/// Cost of any other insertion, deletion or substitution.
pub const EDIT_COST: f64 = 1.0; // edit-distance unit, not a length

/// Candidates costing more than this are not considered.
const MAX_CANDIDATE_COST: f64 = 1.0; // edit-distance units, not a length
/// How steeply each unit of edit cost discounts a candidate's frequency.
const COST_PENALTY: f64 = 4.0; // log-probability per edit, not a length
/// Pseudo-count for keeping an unknown typed word unchanged: a candidate must
/// beat this (after its edit penalty) as well as its rivals.
const KEEP_AS_TYPED_COUNT: f64 = 1.0e5;
/// A typed word that is itself in the lexicon keeps this multiple of its own
/// count, so it is only replaced by a neighbour thousands of times more
/// frequent, which leaves genuine rare words ("foe", "jot") alone.
const KNOWN_WORD_MARGIN: f64 = 2_000.0;
/// Minimum share of the total evidence the best candidate needs.
const MIN_CONFIDENCE: f64 = 0.6;
/// Words shorter than this are left alone.
const MIN_WORD_CHARS: usize = 2;
/// Words longer than this are left alone (and never indexed).
const MAX_WORD_CHARS: usize = 32;
/// Contractions typed without their apostrophe. The lexicon holds no
/// apostrophes, so these are resolved before the edit-distance search, which
/// would otherwise pick a spelling neighbour ("dont" -> "done"). Forms that
/// are words in their own right ("cant", "wont", "ill", "its") are absent.
const CONTRACTIONS: &[(&str, &str)] = &[
    ("arent", "aren't"),
    ("couldnt", "couldn't"),
    ("didnt", "didn't"),
    ("doesnt", "doesn't"),
    ("dont", "don't"),
    ("hasnt", "hasn't"),
    ("havent", "haven't"),
    ("im", "I'm"),
    ("isnt", "isn't"),
    ("ive", "I've"),
    ("shes", "she's"),
    ("shouldnt", "shouldn't"),
    ("thats", "that's"),
    ("theyre", "they're"),
    ("theyve", "they've"),
    ("wasnt", "wasn't"),
    ("werent", "weren't"),
    ("weve", "we've"),
    ("whats", "what's"),
    ("wouldnt", "wouldn't"),
    ("youre", "you're"),
    ("youve", "you've"),
];

/// Bound on remembered kept-as-typed words.
pub(crate) const MAX_KEPT_WORDS: usize = 1_024;

/// Which letter keys touch each other on screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyAdjacency {
    pairs: HashSet<(char, char)>,
}

impl KeyAdjacency {
    /// Derive adjacency from letter keys and their rectangles: two keys are
    /// neighbours when their rectangles touch, including diagonally.
    pub fn from_keys(keys: &[(char, Rect)]) -> Self {
        let mut pairs = HashSet::new();
        for (i, &(a, ra)) in keys.iter().enumerate() {
            let slack = ra.width().min(ra.height()) * 0.05;
            let grown = ra.inflate(slack, slack);
            for &(b, rb) in keys.iter().skip(i + 1) {
                if a != b && !grown.intersect(rb).is_zero_area() {
                    pairs.insert((a, b));
                    pairs.insert((b, a));
                }
            }
        }
        Self { pairs }
    }

    /// Adjacency of a layout's letter keys as laid out by [`key_rects`].
    pub fn from_layout(layout: &KeyboardLayout) -> Self {
        let area = Rect::new(0.0, 0.0, 1000.0, 320.0);
        let keys: Vec<(char, Rect)> = key_rects(layout, area, ShiftState::Off)
            .into_iter()
            .filter_map(|(id, rect)| {
                let key = layout.key(id).filter(|k| k.is_letter())?;
                Some((key.label.chars().next()?.to_lowercase().next()?, rect))
            })
            .collect();
        Self::from_keys(&keys)
    }

    pub fn adjacent(&self, a: char, b: char) -> bool {
        self.pairs.contains(&(a, b))
    }
}

/// Weighted optimal-string-alignment distance from `typed` to `candidate`.
pub fn edit_cost(typed: &str, candidate: &str, adjacency: &KeyAdjacency) -> f64 {
    let a: Vec<char> = typed.chars().collect();
    let b: Vec<char> = candidate.chars().collect();
    let cols = b.len() + 1;
    let mut d = vec![0.0f64; (a.len() + 1) * cols];
    let at = |i: usize, j: usize| i * cols + j;
    for i in 0..=a.len() {
        if let Some(c) = d.get_mut(at(i, 0)) {
            *c = i as f64 * EDIT_COST;
        }
    }
    for j in 0..=b.len() {
        if let Some(c) = d.get_mut(at(0, j)) {
            *c = j as f64 * EDIT_COST;
        }
    }
    let get = |d: &[f64], i: usize, j: usize| d.get(at(i, j)).copied().unwrap_or(f64::INFINITY);
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let (Some(&ca), Some(&cb)) = (a.get(i - 1), b.get(j - 1)) else {
                continue;
            };
            let sub = if ca == cb {
                0.0
            } else if adjacency.adjacent(ca, cb) {
                ADJACENT_SUBSTITUTION_COST
            } else {
                EDIT_COST
            };
            let mut best = (get(&d, i - 1, j) + EDIT_COST)
                .min(get(&d, i, j - 1) + EDIT_COST)
                .min(get(&d, i - 1, j - 1) + sub);
            if i > 1 && j > 1 && a.get(i - 2) == Some(&cb) && b.get(j - 2) == Some(&ca) && ca != cb
            {
                best = best.min(get(&d, i - 2, j - 2) + TRANSPOSITION_COST);
            }
            if let Some(c) = d.get_mut(at(i, j)) {
                *c = best;
            }
        }
    }
    get(&d, a.len(), b.len())
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// `word` plus every string obtained by deleting one of its characters.
fn single_deletes(word: &str) -> Vec<String> {
    let mut out = vec![word.to_string()];
    for (i, _) in word.char_indices() {
        let mut s = String::with_capacity(word.len());
        s.push_str(word.get(..i).unwrap_or(""));
        let rest = word.get(i..).unwrap_or("");
        s.push_str(
            rest.get(rest.chars().next().map_or(0, char::len_utf8)..)
                .unwrap_or(""),
        );
        out.push(s);
    }
    out
}

/// Corrects non-words at word boundaries.
#[derive(Clone, Debug)]
pub struct Autocorrector {
    /// Lexicon words with their estimated corpus counts.
    words: Vec<(String, f64)>,
    /// `(hash(delete), index into words)`, sorted by hash.
    deletes: Vec<(u64, u32)>,
    known: HashMap<String, u32>,
    adjacency: KeyAdjacency,
    kept: HashSet<String>,
}

impl Autocorrector {
    /// Index `dict` for correction with the given key adjacency.
    pub fn new(dict: &Dictionary, adjacency: KeyAdjacency) -> Self {
        let mut words = Vec::with_capacity(dict.len());
        let mut deletes = Vec::new();
        let mut known = HashMap::with_capacity(dict.len());
        for entry in dict.words() {
            let n = entry.word.chars().count();
            if !(MIN_WORD_CHARS - 1..=MAX_WORD_CHARS).contains(&n) {
                continue;
            }
            let idx = words.len() as u32;
            for d in single_deletes(&entry.word) {
                deletes.push((hash_str(&d), idx));
            }
            known.insert(entry.word.clone(), idx);
            words.push((entry.word.clone(), weight_to_count(entry.weight)));
        }
        deletes.sort_unstable();
        deletes.dedup();
        Self {
            words,
            deletes,
            known,
            adjacency,
            kept: HashSet::new(),
        }
    }

    /// Replace the key adjacency (the active layout changed).
    pub fn set_adjacency(&mut self, adjacency: KeyAdjacency) {
        self.adjacency = adjacency;
    }

    /// Never correct `word` again (the user reverted a correction of it).
    pub fn keep(&mut self, word: &str) {
        let word = word.to_lowercase();
        if self.kept.len() < MAX_KEPT_WORDS || self.kept.contains(&word) {
            self.kept.insert(word);
        }
    }

    /// Whether `word` was kept as typed.
    pub fn is_kept(&self, word: &str) -> bool {
        self.kept.contains(&word.to_lowercase())
    }

    /// Take `kept` as the words kept as typed.
    pub fn adopt_kept<'a>(&mut self, kept: impl IntoIterator<Item = &'a String>) {
        self.kept = kept.into_iter().take(MAX_KEPT_WORDS).cloned().collect();
    }

    /// Forget every kept-as-typed word.
    pub fn clear_kept(&mut self) {
        self.kept.clear();
    }

    /// The correction for `typed`, if it is a non-word with one convincing
    /// candidate. Returns the replacement with `typed`'s capitalisation.
    pub fn correct(&self, typed: &str) -> Option<String> {
        let n = typed.chars().count();
        if !(MIN_WORD_CHARS..=MAX_WORD_CHARS).contains(&n)
            || !typed.chars().all(char::is_alphabetic)
        {
            return None;
        }
        // All-capitals words are usually acronyms; leave them alone.
        if typed.chars().all(char::is_uppercase) {
            return None;
        }
        let lower = typed.to_lowercase();
        if self.kept.contains(&lower) {
            return None;
        }
        let typed_idx = self.known.get(&lower).copied();
        if typed_idx.is_none()
            && let Some(&(_, full)) = CONTRACTIONS.iter().find(|(bare, _)| *bare == lower)
        {
            return Some(match_case(typed, full));
        }
        let keep_score = typed_idx
            .and_then(|i| self.words.get(i as usize))
            .map_or(KEEP_AS_TYPED_COUNT, |(_, count)| {
                (count * KNOWN_WORD_MARGIN).max(KEEP_AS_TYPED_COUNT)
            });
        let mut seen = HashSet::new();
        let mut scored: Vec<(u32, f64)> = Vec::new();
        for d in single_deletes(&lower) {
            let h = hash_str(&d);
            let start = self.deletes.partition_point(|&(k, _)| k < h);
            for &(_, idx) in self
                .deletes
                .get(start..)
                .unwrap_or(&[])
                .iter()
                .take_while(|&&(k, _)| k == h)
            {
                if Some(idx) == typed_idx || !seen.insert(idx) {
                    continue;
                }
                let Some((word, count)) = self.words.get(idx as usize) else {
                    continue;
                };
                let cost = edit_cost(&lower, word, &self.adjacency);
                if cost <= MAX_CANDIDATE_COST {
                    scored.push((idx, count * (-COST_PENALTY * cost).exp()));
                }
            }
        }
        let total: f64 = scored.iter().map(|&(_, s)| s).sum::<f64>() + keep_score;
        let &(best, score) = scored
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        if score / total < MIN_CONFIDENCE {
            return None;
        }
        let word = &self.words.get(best as usize)?.0;
        Some(match_case(typed, word))
    }
}

/// `word` with `typed`'s leading capital, if it had one.
fn match_case(typed: &str, word: &str) -> String {
    if typed.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = word.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    } else {
        word.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english() -> Autocorrector {
        Autocorrector::new(
            &Dictionary::english(),
            KeyAdjacency::from_layout(&KeyboardLayout::qwerty()),
        )
    }

    #[test]
    fn adjacency_follows_key_geometry() {
        let adj = KeyAdjacency::from_layout(&KeyboardLayout::qwerty());
        assert!(adj.adjacent('g', 'h'));
        assert!(adj.adjacent('h', 'g'));
        assert!(adj.adjacent('g', 't'), "diagonal neighbours count");
        assert!(!adj.adjacent('q', 'p'));
        assert!(!adj.adjacent('g', 'q'));
    }

    #[test]
    fn adjacent_substitution_is_cheaper() {
        let adj = KeyAdjacency::from_layout(&KeyboardLayout::qwerty());
        assert_eq!(edit_cost("tge", "the", &adj), ADJACENT_SUBSTITUTION_COST);
        assert_eq!(edit_cost("tpe", "the", &adj), EDIT_COST);
        assert_eq!(edit_cost("teh", "the", &adj), TRANSPOSITION_COST);
        assert_eq!(edit_cost("the", "the", &adj), 0.0);
    }

    #[test]
    fn corrects_transposition() {
        assert_eq!(english().correct("teh").as_deref(), Some("the"));
    }

    #[test]
    fn corrects_adjacent_key_slip() {
        assert_eq!(english().correct("tge").as_deref(), Some("the"));
    }

    #[test]
    fn restores_a_missing_apostrophe() {
        let ac = english();
        assert_eq!(ac.correct("dont").as_deref(), Some("don't"));
        assert_eq!(ac.correct("Dont").as_deref(), Some("Don't"));
        assert_eq!(ac.correct("im").as_deref(), Some("I'm"));
        // A word in its own right stays.
        assert_eq!(ac.correct("cant"), None);
    }

    #[test]
    fn keeps_capitalisation() {
        assert_eq!(english().correct("Teh").as_deref(), Some("The"));
    }

    #[test]
    fn leaves_real_words_acronyms_and_noise_alone() {
        let ac = english();
        assert_eq!(ac.correct("the"), None);
        assert_eq!(ac.correct("keyboard"), None);
        // Rare but real words next to far more common ones.
        assert_eq!(ac.correct("foe"), None);
        assert_eq!(ac.correct("fir"), None);
        assert_eq!(ac.correct("jot"), None);
        assert_eq!(ac.correct("mot"), None);
        assert_eq!(ac.correct("TEH"), None);
        assert_eq!(ac.correct("a1"), None);
        assert_eq!(ac.correct("qzxjvkw"), None);
    }

    #[test]
    fn kept_words_are_never_corrected() {
        let mut ac = english();
        ac.keep("teh");
        assert!(ac.is_kept("Teh"));
        assert_eq!(ac.correct("teh"), None);
        ac.clear_kept();
        assert_eq!(ac.correct("teh").as_deref(), Some("the"));
    }
}
