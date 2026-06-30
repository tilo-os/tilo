//! The set of known words.

use std::collections::HashSet;

/// The embedded English word list.
///
/// Source: the system `hunspell` en_US spelling dictionary
/// (`/usr/share/hunspell/en_US.dic`, the SCOWL-derived list shipped by Fedora),
/// used as an oracle to validate a lowercased/normalised pass over the much
/// larger but noisier `/usr/share/dict/words` (`linux.words`). Every candidate
/// that `hunspell -d en_US` accepted — which expands the dictionary's affix
/// rules, so it includes inflected forms like `jumps`, `jumping`, `foxes` that
/// the bare `.dic` stems omit — was kept. The result was lowercased, restricted
/// to ASCII letters with optional internal apostrophes, length 1..=15, and sorted
/// unique.
///
/// Size: ~75,000 words, ~700 KB. See `spellcheck/data/words.txt`.
const BUILTIN_WORDS: &str = include_str!("../data/words.txt");

/// A set of known words for spell checking.
///
/// Membership is **case-insensitive**: words are stored lowercased and
/// [`Dictionary::contains`] lowercases its argument before looking it up. The
/// dictionary also keeps a length-bucketed index used by the suggester to avoid
/// scanning words whose length alone puts them out of edit-distance range.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    words: HashSet<String>,
    /// `by_len[n]` holds every word of `char`-length `n` (lowercased), so the
    /// suggester can restrict its scan to lengths within +-max of the query.
    by_len: Vec<Vec<String>>,
}

impl Dictionary {
    /// Build the dictionary embedded in the binary at compile time.
    pub fn builtin() -> Self {
        Self::from_words(BUILTIN_WORDS.lines())
    }

    /// Build a dictionary from any iterator of word-like strings.
    ///
    /// Each entry is trimmed and lowercased; empty entries are ignored.
    /// Duplicate words collapse. This is the constructor used both by
    /// [`Dictionary::builtin`] and by callers supplying a custom list (e.g. a
    /// per-document allow-list).
    pub fn from_words<I, S>(words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = HashSet::new();
        let mut by_len: Vec<Vec<String>> = Vec::new();
        for w in words {
            let normalized = w.as_ref().trim().to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            if set.insert(normalized.clone()) {
                let len = normalized.chars().count();
                if by_len.len() <= len {
                    by_len.resize(len + 1, Vec::new());
                }
                by_len[len].push(normalized);
            }
        }
        Dictionary { words: set, by_len }
    }

    /// Returns `true` if `word` is in the dictionary, compared case-insensitively.
    pub fn contains(&self, word: &str) -> bool {
        // Fast path: a word that is already lowercase needs no allocation.
        if word.chars().all(|c| !c.is_uppercase()) {
            self.words.contains(word)
        } else {
            self.words.contains(&word.to_lowercase())
        }
    }

    /// Number of distinct words in the dictionary.
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Returns `true` if the dictionary has no words.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Iterate over all words whose `char`-length is within `[min_len, max_len]`
    /// (inclusive). Used by the suggester to bound its scan.
    pub(crate) fn words_in_len_range(
        &self,
        min_len: usize,
        max_len: usize,
    ) -> impl Iterator<Item = &str> {
        let hi = max_len.min(self.by_len.len().saturating_sub(1));
        (min_len..=hi)
            .filter(move |&n| n < self.by_len.len())
            .flat_map(move |n| self.by_len[n].iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_words_is_case_insensitive() {
        let d = Dictionary::from_words(["Hello", "WORLD"]);
        assert!(d.contains("hello"));
        assert!(d.contains("HELLO"));
        assert!(d.contains("World"));
        assert!(!d.contains("goodbye"));
    }

    #[test]
    fn trims_and_skips_empty() {
        let d = Dictionary::from_words(["  spaced  ", "", "   ", "ok"]);
        assert!(d.contains("spaced"));
        assert!(d.contains("ok"));
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn len_range_iter_respects_bounds() {
        let d = Dictionary::from_words(["a", "to", "cat", "four", "fives"]);
        let got: HashSet<&str> = d.words_in_len_range(3, 4).collect();
        assert!(got.contains("cat"));
        assert!(got.contains("four"));
        assert!(!got.contains("to"));
        assert!(!got.contains("fives"));
    }

    #[test]
    fn builtin_loads() {
        let d = Dictionary::builtin();
        assert!(d.len() > 10_000);
        assert!(d.contains("the"));
        assert!(d.contains("quick"));
        assert!(!d.contains("teh"));
    }
}
