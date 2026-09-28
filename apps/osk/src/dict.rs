//! Word-frequency dictionaries.
//!
//! Compact, **offline**, embedded word lists with relative frequencies, one
//! per [`Language`], used by the swipe decoder ([`crate::swipe`]), the
//! next-word predictor ([`crate::predict`]) and autocorrection.
//!
//! The data is bundled at build time from `apps/osk/data/words-<lang>.txt`
//! (`word<TAB>weight`, sorted by frequency descending). See that directory's
//! `README.md` for provenance and licences (SCOWL or Grammalecte vocabulary
//! ranked by wordfreq frequencies).
//!
//! ## What it exposes
//! * [`Dictionary::contains`] / [`Dictionary::weight`] — membership + freq.
//! * [`Dictionary::iter_prefix`] — every word starting with a prefix (used by
//!   prefix-completion in the predictor and as a candidate filter in swipe).
//! * [`Dictionary::words`] — the full backing slice for the decoder to scan.
//!
//! The words are stored sorted by frequency (most common first) **and** an
//! index sorted by their accent-folded spelling is built once, so prefix
//! queries are a binary-search range rather than a full scan. Prefixes match
//! accent-insensitively: the keyboards carry no accented keys, so typing
//! `tres` must still reach `très`.

use std::cmp::Ordering;
use std::collections::HashMap;

/// A language the keyboard ships offline word data for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Language {
    #[default]
    English,
    French,
}

impl Language {
    /// The language typed on the letter layout `name`: QWERTY types English
    /// and AZERTY French. `None` for layouts with no word data (symbols,
    /// dialpads).
    pub fn for_layout(name: &str) -> Option<Self> {
        match name {
            "qwerty" => Some(Self::English),
            "azerty" => Some(Self::French),
            _ => None,
        }
    }

    /// The embedded `word<TAB>weight` table.
    fn words_table(self) -> &'static str {
        match self {
            Self::English => include_str!("../data/words-en.txt"),
            Self::French => include_str!("../data/words-fr.txt"),
        }
    }
}

/// `c` without its accent (`é` → `e`, `ç` → `c`). Letters with no single-letter
/// base (`æ`, `œ`) and everything else are returned unchanged.
pub fn fold_char(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' => 'a',
        'ç' => 'c',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' => 'i',
        'ô' | 'ö' => 'o',
        'ù' | 'û' | 'ü' => 'u',
        'ÿ' => 'y',
        other => other,
    }
}

/// Whether `word` starts with `prefix`, ignoring accents on either side.
pub fn starts_with_folded(word: &str, prefix: &str) -> bool {
    let mut word = word.chars().map(fold_char);
    prefix
        .chars()
        .map(fold_char)
        .all(|p| word.next() == Some(p))
}

/// Order by accent-folded spelling, then by exact spelling, so words that
/// differ only in accents (`ou`, `où`) sit next to each other.
fn cmp_folded(a: &str, b: &str) -> Ordering {
    a.chars()
        .map(fold_char)
        .cmp(b.chars().map(fold_char))
        .then_with(|| a.cmp(b))
}

/// One dictionary entry: the word plus its compact log-scaled frequency
/// weight (higher = more common).
#[derive(Clone, Debug)]
pub struct Entry {
    pub word: String,
    pub weight: u32,
}

/// A word-frequency dictionary loaded from embedded or custom data.
///
/// Construction parses the table once. Lookups are `O(log n)` via an
/// accent-folded lexicographic index; prefix iteration yields a contiguous
/// range of that index.
#[derive(Clone, Debug)]
pub struct Dictionary {
    /// Entries in frequency order (most common first). Stable; indices into
    /// this vector are used by `by_lex`.
    entries: Vec<Entry>,
    /// Indices into `entries`, sorted by [`cmp_folded`]. Enables
    /// binary-search prefix queries without duplicating the strings.
    by_lex: Vec<u32>,
    /// Max weight, used to normalise frequencies into 0..1.
    max_weight: u32,
}

impl Dictionary {
    /// Load the embedded dictionary of `language`.
    pub fn for_language(language: Language) -> Self {
        Self::from_str(language.words_table())
    }

    /// Load the embedded English dictionary.
    pub fn english() -> Self {
        Self::for_language(Language::English)
    }

    /// Load the embedded French dictionary.
    pub fn french() -> Self {
        Self::for_language(Language::French)
    }

    /// Parse a `word<TAB>weight` table (one entry per line). Malformed lines
    /// are skipped. Exposed for tests and for hosts that want a custom list.
    /// (Infallible, so it intentionally does not implement `std::str::FromStr`.)
    #[allow(
        clippy::should_implement_trait,
        reason = "infallible parse; a FromStr impl would force a pointless error type"
    )]
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
            cmp_folded(wa, wb)
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

    /// How often each non-ASCII letter occurs in running text of this
    /// language: its occurrences summed over every word, weighted by the
    /// word's estimated count. Ranks the long-press accents.
    pub fn accent_usage(&self) -> HashMap<char, f64> {
        let mut usage: HashMap<char, f64> = HashMap::new();
        for entry in &self.entries {
            let count = weight_to_count(entry.weight);
            for c in entry.word.chars().filter(|c| !c.is_ascii()) {
                *usage.entry(c).or_insert(0.0) += count;
            }
        }
        usage
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

    /// Iterate every entry whose word starts with `prefix` (case- and
    /// accent-insensitive), in folded lexicographic order. Backed by a binary-search range over the lex
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

    /// Mass of each letter that can follow `prefix` (case- and
    /// accent-insensitive): for every word extending `prefix`, its estimated
    /// corpus count is credited to the (accent-folded) character right after
    /// the prefix. The count is recovered from
    /// the log-scaled weight so frequent words dominate as they do in text.
    /// Words equal to `prefix` end there and contribute nothing.
    pub fn next_char_weights(&self, prefix: &str) -> Vec<(char, f64)> {
        let prefix = prefix.to_lowercase();
        let skip = prefix.chars().count();
        let mut out: Vec<(char, f64)> = Vec::new();
        for entry in self.iter_prefix(&prefix) {
            let Some(next) = entry.word.chars().nth(skip).map(fold_char) else {
                continue;
            };
            let count = weight_to_count(entry.weight);
            match out.iter_mut().find(|(c, _)| *c == next) {
                Some((_, w)) => *w += count,
                None => out.push((next, count)),
            }
        }
        out
    }

    // --- internals ------------------------------------------------------

    /// Index into `entries` for an exact (already-lowercased) word.
    fn lex_index_of(&self, word: &str) -> Option<usize> {
        // Every value in `by_lex` is a valid index into `entries` (see the
        // field doc), so `get` here never actually misses.
        let res = self.by_lex.binary_search_by(|&i| {
            self.entries
                .get(i as usize)
                .map_or(Ordering::Equal, |e| cmp_folded(&e.word, word))
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
        // Lower bound: first word whose folded spelling sorts at or after the
        // folded prefix. Every `by_lex` value indexes `entries` in range (see
        // the field doc); `get` keeps that an invariant instead of an
        // indexing panic in the comparator.
        let lo = self.by_lex.partition_point(|&i| {
            self.entries.get(i as usize).is_some_and(|e| {
                e.word
                    .chars()
                    .map(fold_char)
                    .lt(prefix.chars().map(fold_char))
            })
        });
        // Upper bound: first word past `lo` that does not start with the
        // prefix; folded order keeps every match contiguous.
        let hi = self.by_lex.get(lo..).unwrap_or(&[]).partition_point(|&i| {
            self.entries
                .get(i as usize)
                .is_some_and(|e| starts_with_folded(&e.word, prefix))
        }) + lo;
        (lo, hi)
    }
}

/// Estimated corpus count behind a compact weight (`round(log10(count) *
/// 1000)`, see `data/README.md`).
pub fn weight_to_count(weight: u32) -> f64 {
    10f64.powf(f64::from(weight) / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_dictionary_loads_with_sane_normalised_frequencies() {
        eprintln!("phase embedded_dictionary_loads_thousands_of_words");
        {
            let d = Dictionary::english();
            assert!(d.len() > 10_000, "want tens of thousands, got {}", d.len());
            assert!(!d.is_empty());
        }

        eprintln!("phase common_words_present_with_sane_relative_frequency");
        {
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

        eprintln!("phase frequency_is_normalised_and_monotone");
        {
            let d = Dictionary::english();
            let f_the = d.frequency("the");
            let f_quick = d.frequency("quick");
            assert!((0.0..=1.0).contains(&f_the));
            assert!(f_the >= f_quick);
            assert!(f_the > 0.0);
        }

        eprintln!("phase from_str_parses_and_skips_malformed");
        {
            let d = Dictionary::from_str("apple\t100\nbanana\t50\nbroken-line\ncherry\t75\n");
            assert_eq!(d.len(), 3);
            assert!(d.contains("apple"));
            assert!(d.contains("cherry"));
            assert!(!d.contains("broken-line"));
            // Frequency order: apple (100) > cherry (75) > banana (50).
            assert_eq!(d.words()[0].word, "apple");
            assert_eq!(d.words()[2].word, "banana");
        }
    }

    #[test]
    fn prefix_iteration_and_completion_return_ranked_matches() {
        eprintln!("phase prefix_iteration_returns_only_matching_words");
        {
            let d = Dictionary::english();
            let qu: Vec<_> = d.iter_prefix("qu").map(|e| e.word.clone()).collect();
            assert!(qu.iter().all(|w| w.starts_with("qu")));
            assert!(qu.contains(&"quick".to_string()));
            assert!(qu.contains(&"question".to_string()));
            // A prefix nothing starts with yields an empty range.
            assert_eq!(d.iter_prefix("qzx").count(), 0);
        }

        eprintln!("phase complete_ranks_by_frequency");
        {
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

        eprintln!("phase prefixes_match_accent_insensitively");
        {
            let d = Dictionary::from_str(
                "tres\t10\ntrès\t900\ntrésor\t300\ntrain\t500\nou\t800\noù\t700\n",
            );
            let tre: Vec<&str> = d.iter_prefix("tre").map(|e| e.word.as_str()).collect();
            assert_eq!(tre, ["tres", "très", "trésor"]);
            assert_eq!(d.complete("tres", 1)[0].word, "très");
            // Exact lookups still tell accented words apart.
            assert_eq!(d.weight("ou"), Some(800));
            assert_eq!(d.weight("où"), Some(700));
            assert_eq!(d.weight("trés"), None);
            let next: HashMap<char, f64> = d.next_char_weights("tr").into_iter().collect();
            assert!(next[&'e'] > next[&'a'], "è and é count for e: {next:?}");
        }

        eprintln!("phase embedded_french_dictionary_loads");
        {
            let d = Dictionary::french();
            assert!(d.len() > 10_000, "want tens of thousands, got {}", d.len());
            for word in ["de", "bonjour", "très", "été", "ça", "merci"] {
                assert!(d.contains(word), "{word} missing");
            }
            assert!(d.weight("de").unwrap() > d.weight("bonjour").unwrap());
            // Stray letters and elided articles are not words.
            assert!(!d.contains("l") && !d.contains("d"));
            assert!(d.contains("à"));
        }

        eprintln!("phase empty_prefix_iterates_all");
        {
            let d = Dictionary::from_str("a\t3\nb\t2\nc\t1\n");
            assert_eq!(d.iter_prefix("").count(), 3);
        }

        eprintln!("phase next_char_weights_credit_the_letter_after_the_prefix");
        {
            let d = Dictionary::from_str("the\t3000\nthree\t2000\nthat\t2500\nth\t1000\n");
            let w: std::collections::HashMap<char, f64> =
                d.next_char_weights("th").into_iter().collect();
            assert_eq!(w.len(), 3, "'th' itself ends there: {w:?}");
            assert!(w[&'e'] > w[&'a'] && w[&'a'] > w[&'r']);
            assert!((w[&'e'] - weight_to_count(3000)).abs() < 1e-6);
        }
    }
}
