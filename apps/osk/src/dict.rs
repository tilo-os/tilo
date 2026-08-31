//! English word-frequency dictionary.
//!
//! A compact, **offline**, embedded English word list with relative
//! frequencies, used by both the swipe decoder ([`crate::swipe`]) and the
//! next-word predictor ([`crate::predict`]).
//!
//! The data is bundled at build time from `apps/osk/data/words.txt`
//! (`word<TAB>weight`, sorted by frequency descending). See that directory's
//! `README.md` for provenance and license (derived from Peter Norvig's
//! MIT-licensed n-gram subset of the Google Web Trillion Word Corpus).
//!
//! ## What it exposes
//! * [`Dictionary::contains`] / [`Dictionary::weight`] — membership + freq.
//! * [`Dictionary::iter_prefix`] — every word starting with a prefix (used by
//!   prefix-completion in the predictor and as a candidate filter in swipe).
//! * [`Dictionary::words`] — the full backing slice for the decoder to scan.
//!
//! The words are stored sorted by frequency (most common first) **and** an
//! index sorted lexicographically is built once so prefix queries are a
//! binary-search range rather than a full scan.

/// The embedded word-frequency table, `word<TAB>weight` per line, sorted by
/// weight descending. ~30k common English words.
const WORDS_TXT: &str = include_str!("../data/words.txt");

/// One dictionary entry: the word plus its compact log-scaled frequency
/// weight (higher = more common).
#[derive(Clone, Debug)]
pub struct Entry {
    pub word: String,
    pub weight: u32,
}

/// An English word-frequency dictionary loaded from the embedded data.
///
/// Construction parses the embedded table once. Lookups are `O(log n)` via a
/// lexicographic index; prefix iteration yields a contiguous range of that
/// index.
#[derive(Clone, Debug)]
pub struct Dictionary {
    /// Entries in frequency order (most common first). Stable; indices into
    /// this vector are used by `by_lex`.
    entries: Vec<Entry>,
    /// Indices into `entries`, sorted by the word lexicographically. Enables
    /// binary-search prefix queries without duplicating the strings.
    by_lex: Vec<u32>,
    /// Max weight, used to normalise frequencies into 0..1.
    max_weight: u32,
}

impl Dictionary {
    /// Load the embedded English dictionary. Parses ~30k lines once.
    pub fn english() -> Self {
        Self::from_str(WORDS_TXT)
    }

    /// Parse a `word<TAB>weight` table (one entry per line). Malformed lines
    /// are skipped. Exposed for tests and for hosts that want a custom list.
    /// (Infallible, so it intentionally does not implement `std::str::FromStr`.)
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(data: &str) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for line in data.lines() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(word), Some(w)) = (it.next(), it.next()) else {
                continue;
            };
            let Ok(weight) = w.trim().parse::<u32>() else {
                continue;
            };
            if word.is_empty() {
                continue;
            }
            entries.push(Entry {
                word: word.to_string(),
                weight,
            });
        }
        Self::from_entries(entries)
    }

    /// Build from already-parsed entries (any order).
    pub fn from_entries(mut entries: Vec<Entry>) -> Self {
        // Frequency order (most common first); break ties lexicographically
        // for determinism.
        entries.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.word.cmp(&b.word)));
        let max_weight = entries.first().map_or(1, |e| e.weight).max(1);

        let mut by_lex: Vec<u32> = (0..entries.len() as u32).collect();
        // `by_lex` is built from `0..entries.len()` just above, so every
        // index is in range; `get` keeps that an invariant rather than an
        // indexing panic.
        by_lex.sort_by(|&a, &b| {
            let wa = entries.get(a as usize).map_or("", |e| e.word.as_str());
            let wb = entries.get(b as usize).map_or("", |e| e.word.as_str());
            wa.cmp(wb)
        });

        Self {
            entries,
            by_lex,
            max_weight,
        }
    }

    /// Number of words in the dictionary.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, in frequency order (most common first).
    pub fn words(&self) -> &[Entry] {
        &self.entries
    }

    /// The compact frequency weight of `word` (case-insensitive), or `None`
    /// if absent.
    pub fn weight(&self, word: &str) -> Option<u32> {
        // `lex_index_of` always returns a valid index into `entries` when it
        // returns `Some` (see its doc); `get` keeps that an invariant.
        self.lex_index_of(&word.to_lowercase())
            .and_then(|i| self.entries.get(i))
            .map(|e| e.weight)
    }

    /// Whether `word` (case-insensitive) is in the dictionary.
    pub fn contains(&self, word: &str) -> bool {
        self.weight(word).is_some()
    }

    /// Frequency normalised into `0.0..=1.0` (relative to the most common
    /// word), or `0.0` if absent. Useful for blending with geometric scores.
    pub fn frequency(&self, word: &str) -> f32 {
        match self.weight(word) {
            Some(w) => w as f32 / self.max_weight as f32,
            None => 0.0,
        }
    }

    /// Normalise an arbitrary weight into 0..1 (for callers that already hold
    /// a weight, e.g. the predictor blending prefix completions).
    pub fn normalize_weight(&self, weight: u32) -> f32 {
        weight as f32 / self.max_weight as f32
    }

    /// Iterate every entry whose word starts with `prefix` (case-insensitive),
    /// in lexicographic order. Backed by a binary-search range over the lex
    /// index — no full scan.
    pub fn iter_prefix<'a>(&'a self, prefix: &str) -> impl Iterator<Item = &'a Entry> + 'a {
        let prefix = prefix.to_lowercase();
        let (lo, hi) = self.lex_range(&prefix);
        // `lex_range` always returns a `lo..hi` window inside `by_lex`, and
        // every `by_lex` value is a valid index into `entries` (see the
        // field doc); `get`/`filter_map` keep those invariants rather than
        // indexing panics.
        self.by_lex
            .get(lo..hi)
            .unwrap_or(&[])
            .iter()
            .filter_map(move |&i| self.entries.get(i as usize))
    }

    /// Top-`n` completions of `prefix`, ranked by frequency descending.
    pub fn complete(&self, prefix: &str, n: usize) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self.iter_prefix(prefix).collect();
        v.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.word.cmp(&b.word)));
        v.truncate(n);
        v
    }

    // --- internals ------------------------------------------------------

    /// Index into `entries` for an exact (already-lowercased) word.
    fn lex_index_of(&self, word: &str) -> Option<usize> {
        // Every value in `by_lex` is a valid index into `entries` (see the
        // field doc), so `get` here never actually misses.
        let res = self.by_lex.binary_search_by(|&i| {
            self.entries
                .get(i as usize)
                .map_or(std::cmp::Ordering::Equal, |e| e.word.as_str().cmp(word))
        });
        res.ok()
            .and_then(|pos| self.by_lex.get(pos))
            .map(|&i| i as usize)
    }

    /// `[lo, hi)` range in `by_lex` of words with the given prefix.
    fn lex_range(&self, prefix: &str) -> (usize, usize) {
        if prefix.is_empty() {
            return (0, self.by_lex.len());
        }
        // Lower bound: first word >= prefix. Every `by_lex` value indexes
        // `entries` in range (see the field doc); `get` keeps that an
        // invariant instead of an indexing panic in the comparator.
        let lo = self.by_lex.partition_point(|&i| {
            self.entries
                .get(i as usize)
                .is_some_and(|e| e.word.as_str() < prefix)
        });
        // Upper bound: first word that does not start with prefix. We compare
        // against `prefix` truncated-then-incremented conceptually by checking
        // the `starts_with` boundary directly.
        let hi = self.by_lex.get(lo..).unwrap_or(&[]).partition_point(|&i| {
            self.entries
                .get(i as usize)
                .is_some_and(|e| e.word.as_str().starts_with(prefix))
        }) + lo;
        (lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_dictionary_loads_thousands_of_words() {
        let d = Dictionary::english();
        assert!(d.len() > 10_000, "want tens of thousands, got {}", d.len());
        assert!(!d.is_empty());
    }

    #[test]
    fn common_words_present_with_sane_relative_frequency() {
        let d = Dictionary::english();
        assert!(d.contains("the"));
        assert!(d.contains("quick"));
        assert!(d.contains("hello"));
        assert!(d.contains("keyboard"));
        // "the" is the most common English word — strictly more frequent than
        // a mid-rank word like "quick".
        assert!(d.weight("the").unwrap() > d.weight("quick").unwrap());
        // Case-insensitive lookup.
        assert_eq!(d.weight("THE"), d.weight("the"));
        // Absent garbage.
        assert!(!d.contains("zzxqj"));
        assert_eq!(d.frequency("zzxqj"), 0.0);
    }

    #[test]
    fn frequency_is_normalised_and_monotone() {
        let d = Dictionary::english();
        let f_the = d.frequency("the");
        let f_quick = d.frequency("quick");
        assert!((0.0..=1.0).contains(&f_the));
        assert!(f_the >= f_quick);
        assert!(f_the > 0.0);
    }

    #[test]
    fn prefix_iteration_returns_only_matching_words() {
        let d = Dictionary::english();
        let qu: Vec<_> = d.iter_prefix("qu").map(|e| e.word.clone()).collect();
        assert!(qu.iter().all(|w| w.starts_with("qu")));
        assert!(qu.contains(&"quick".to_string()));
        assert!(qu.contains(&"question".to_string()));
        // A prefix nothing starts with yields an empty range.
        assert_eq!(d.iter_prefix("qzx").count(), 0);
    }

    #[test]
    fn complete_ranks_by_frequency() {
        let d = Dictionary::english();
        let top = d.complete("th", 5);
        assert!(!top.is_empty());
        // "the" should be the top completion of "th".
        assert_eq!(top[0].word, "the");
        // Results are frequency-descending.
        for w in top.windows(2) {
            assert!(w[0].weight >= w[1].weight);
        }
    }

    #[test]
    fn from_str_parses_and_skips_malformed() {
        let d = Dictionary::from_str("apple\t100\nbanana\t50\nbroken-line\ncherry\t75\n");
        assert_eq!(d.len(), 3);
        assert!(d.contains("apple"));
        assert!(d.contains("cherry"));
        assert!(!d.contains("broken-line"));
        // Frequency order: apple (100) > cherry (75) > banana (50).
        assert_eq!(d.words()[0].word, "apple");
        assert_eq!(d.words()[2].word, "banana");
    }

    #[test]
    fn empty_prefix_iterates_all() {
        let d = Dictionary::from_str("a\t3\nb\t2\nc\t1\n");
        assert_eq!(d.iter_prefix("").count(), 3);
    }
}
