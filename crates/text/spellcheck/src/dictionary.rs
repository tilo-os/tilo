//! The set of known words.

use std::collections::{BTreeMap, HashSet};

/// The embedded English word list.
///
/// Source: the system `hunspell` `en_US` spelling dictionary
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

/// Product names included in the built-in dictionary.
///
/// Membership is case-insensitive (see [`Dictionary::contains`]), so one
/// spelling covers `Tilo`, `TILO` and `tilo`.
const PRODUCT_WORDS: &[&str] = &["tilo", "claude", "tilo"];

/// Common contractions and proper names included in the embedded fallback.
const CONVERSATIONAL_WORDS: &[&str] = &[
    "aren't",
    "can't",
    "couldn't",
    "didn't",
    "doesn't",
    "don't",
    "hadn't",
    "hasn't",
    "haven't",
    "he'd",
    "he'll",
    "he's",
    "how's",
    "i'd",
    "i'll",
    "i'm",
    "i've",
    "isn't",
    "it'd",
    "it'll",
    "it's",
    "let's",
    "mustn't",
    "shan't",
    "she'd",
    "she'll",
    "she's",
    "shouldn't",
    "that's",
    "there's",
    "they'd",
    "they'll",
    "they're",
    "they've",
    "wasn't",
    "we'd",
    "we'll",
    "we're",
    "we've",
    "weren't",
    "what's",
    "where's",
    "who's",
    "won't",
    "wouldn't",
    "you'd",
    "you'll",
    "you're",
    "you've",
    "alex",
    "alexander",
    "alice",
    "amelia",
    "andrew",
    "anna",
    "anthony",
    "ava",
    "benjamin",
    "charles",
    "charlotte",
    "chloe",
    "daniel",
    "david",
    "elijah",
    "elizabeth",
    "ella",
    "emily",
    "emma",
    "ethan",
    "evelyn",
    "grace",
    "harper",
    "henry",
    "isabella",
    "jack",
    "james",
    "jane",
    "joseph",
    "liam",
    "lily",
    "lucas",
    "lucy",
    "maria",
    "mason",
    "mia",
    "michael",
    "noah",
    "oliver",
    "olivia",
    "ada",
    "samuel",
    "sophia",
    "victoria",
    "william",
    "ctrl",
];

/// A set of known words for spell checking.
///
/// Membership is **case-insensitive**: words are stored lowercased and
/// [`Dictionary::contains`] lowercases its argument before looking it up. The
/// dictionary also keeps a length-bucketed index used by the suggester to avoid
/// scanning words whose length alone puts them out of edit-distance range.
///
/// On top of the built-in word list, a [`Dictionary`] carries a small,
/// runtime-mutable **user dictionary** ([`Dictionary::add_word`]) — words the
/// user has explicitly accepted (e.g. via "Add to dictionary" in the word
/// processor). User words participate in [`contains`](Dictionary::contains)
/// exactly like built-in ones, so an added word is no longer flagged.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    words: HashSet<String>,
    /// `by_len[n]` holds every word of `char`-length `n` (lowercased), so the
    /// suggester can restrict its scan to lengths within +-max of the query.
    by_len: Vec<Vec<String>>,
    /// Runtime-added user words, keyed case-insensitively while retaining the
    /// spelling explicitly accepted by the person. Kept separate from `words` so the
    /// built-in list stays a clean compile-time set and the user words can be
    /// persisted independently. Consulted by [`contains`](Self::contains).
    user_words: BTreeMap<String, String>,
}

impl Dictionary {
    /// Build the dictionary embedded in the binary at compile time.
    pub fn builtin() -> Self {
        Self::from_words(
            BUILTIN_WORDS
                .lines()
                .chain(PRODUCT_WORDS.iter().copied())
                .chain(CONVERSATIONAL_WORDS.iter().copied()),
        )
    }

    /// Load any persisted user words from the user-config file into this
    /// dictionary's runtime user set. Best-effort: a missing / unreadable file
    /// is a silent no-op. Lets a caller that already holds a built-in dictionary
    /// (e.g. cloned from a process-wide shared copy to avoid re-parsing the 75k
    /// word list) layer in the persisted user words without re-parsing.
    pub fn load_user_dict(&mut self) {
        if let Some(path) = user_dict_path() {
            self.load_user_dict_from(&path);
        }
    }

    /// [`Self::load_user_dict`] from an explicit word-list file.
    pub fn load_user_dict_from(&mut self, path: &std::path::Path) {
        if let Ok(contents) = std::fs::read_to_string(path) {
            for line in contents.lines() {
                let w = line.trim();
                if !w.is_empty() && !self.words.contains(&w.to_lowercase()) {
                    self.user_words
                        .entry(w.to_lowercase())
                        .or_insert_with(|| w.to_owned());
                }
            }
        }
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
                    by_len.resize(len.saturating_add(1), Vec::new());
                }
                // The resize above guarantees `len` is now in bounds.
                if let Some(bucket) = by_len.get_mut(len) {
                    bucket.push(normalized);
                }
            }
        }
        Dictionary {
            words: set,
            by_len,
            user_words: BTreeMap::new(),
        }
    }

    /// Returns `true` if `word` is in the dictionary (built-in list **or** the
    /// runtime user dictionary), compared case-insensitively.
    pub fn contains(&self, word: &str) -> bool {
        let apostrophe_normalized;
        let word = if word.contains('\u{2019}') {
            apostrophe_normalized = word.replace('\u{2019}', "'");
            apostrophe_normalized.as_str()
        } else {
            word
        };
        // Fast path: a word that is already lowercase needs no allocation.
        if word.chars().all(|c| !c.is_uppercase()) {
            self.words.contains(word) || self.user_words.contains_key(word)
        } else {
            let lower = word.to_lowercase();
            self.words.contains(&lower) || self.user_words.contains_key(&lower)
        }
    }

    /// Add `word` to the runtime user dictionary so it is no longer flagged as a
    /// misspelling. Stored lowercased (membership is case-insensitive). Empty /
    /// whitespace-only input is ignored. Returns `true` if the word was newly
    /// added (i.e. not already known via the built-in list or a prior add).
    ///
    /// This does **not** persist on its own — call [`save_user_dict`](Self::save_user_dict)
    /// to write the user dictionary to disk.
    pub fn add_word(&mut self, word: &str) -> bool {
        let normalized = word.trim().to_lowercase();
        if normalized.is_empty() {
            return false;
        }
        if self.words.contains(&normalized) {
            // Already a built-in word — nothing to record.
            return false;
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = self.user_words.entry(normalized)
        {
            entry.insert(word.trim().to_owned());
            true
        } else {
            false
        }
    }

    /// Persist the runtime user dictionary to the user-config file
    /// (`$XDG_CONFIG_HOME/tilo/userdict.txt`). Best-effort: returns `Ok(())` and
    /// no-ops when no config path is resolvable; surfaces I/O errors otherwise so
    /// a caller *can* report them, but the typical caller ignores the result.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the config directory cannot
    /// be created (`create_dir_all`) or the word list cannot be written
    /// (`write`) — e.g. permission denied, or a read-only filesystem.
    pub fn save_user_dict(&self) -> std::io::Result<()> {
        match user_dict_path() {
            Some(path) => self.save_user_dict_to(&path),
            None => Ok(()),
        }
    }

    /// [`Self::save_user_dict`] to an explicit word-list file, creating its
    /// directory.
    ///
    /// # Errors
    ///
    /// As [`Self::save_user_dict`].
    pub fn save_user_dict_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Stable case-insensitive order; the same existing file carries the
        // person's accepted spelling without creating another dictionary.
        let words: Vec<&str> = self.user_words.values().map(String::as_str).collect();
        let body = words.join("\n");
        std::fs::write(path, body)
    }

    /// The runtime user-added words, lowercased and unsorted. Mostly useful for
    /// tests and persistence.
    pub fn user_words(&self) -> impl Iterator<Item = &str> {
        self.user_words.keys().map(String::as_str)
    }

    /// Personal spellings as explicitly accepted, in stable lookup order.
    /// Readers such as prediction and speech biasing must use these spellings
    /// rather than changing recognised proper names with a fuzzy rewrite.
    pub fn user_spellings(&self) -> impl Iterator<Item = &str> {
        self.user_words.values().map(String::as_str)
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
            .flat_map(move |n| self.by_len.get(n).into_iter().flatten().map(String::as_str))
    }
}

/// Resolve the path of the persisted user dictionary:
/// `$XDG_CONFIG_HOME/tilo/userdict.txt`, falling back to
/// `$HOME/.config/tilo/userdict.txt`. Returns `None` when neither environment
/// variable is set (e.g. a locked-down sandbox), in which case the user
/// dictionary lives only in memory.
pub fn user_dict_path() -> Option<std::path::PathBuf> {
    let base = tilo_paths::xdg::config_home()?;
    Some(base.join("tilo").join("userdict.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_spelling_survives_the_existing_dictionary_file_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("userdict.txt");
        let mut dictionary = Dictionary::default();
        assert!(dictionary.add_word("Tilo"));
        assert!(dictionary.add_word("Mélusine"));
        assert!(!dictionary.add_word("tilo"));
        dictionary.save_user_dict_to(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Mélusine\nTilo");
        let mut restored = Dictionary::default();
        restored.load_user_dict_from(&path);
        assert!(restored.contains("MÉLUSINE"));
        assert!(restored.contains("TILO"));
        assert_eq!(
            restored.user_words().collect::<Vec<_>>(),
            ["mélusine", "tilo"]
        );
        assert_eq!(
            restored.user_spellings().collect::<Vec<_>>(),
            ["Mélusine", "Tilo"]
        );
    }

    #[test]
    fn dictionaries_build_case_insensitively_and_accept_added_words() {
        // Phase `from_words_is_case_insensitive`.
        {
            let d = Dictionary::from_words(["Hello", "WORLD"]);
            assert!(d.contains("hello"));
            assert!(d.contains("HELLO"));
            assert!(d.contains("World"));
            assert!(!d.contains("goodbye"));
        }
        // Phase `trims_and_skips_empty`.
        {
            let d = Dictionary::from_words(["  spaced  ", "", "   ", "ok"]);
            assert!(d.contains("spaced"));
            assert!(d.contains("ok"));
            assert_eq!(d.len(), 2);
        }
        // Phase `len_range_iter_respects_bounds`.
        {
            let d = Dictionary::from_words(["a", "to", "cat", "four", "fives"]);
            let got: HashSet<&str> = d.words_in_len_range(3, 4).collect();
            assert!(got.contains("cat"));
            assert!(got.contains("four"));
            assert!(!got.contains("to"));
            assert!(!got.contains("fives"));
        }
        // Phase `builtin_loads`.
        {
            let d = Dictionary::builtin();
            assert!(d.len() > 10_000);
            assert!(d.contains("the"));
            assert!(d.contains("quick"));
            assert!(!d.contains("teh"));
        }
        // Phase `add_word_makes_a_word_known_case_insensitively`.
        {
            let mut d = Dictionary::from_words(["alpha", "beta"]);
            assert!(!d.contains("tilo"), "precondition: unknown word");
            assert!(d.add_word("Tilo"), "first add records the word");
            assert!(d.contains("tilo"), "added word is now known");
            assert!(d.contains("TILO"), "membership stays case-insensitive");
            // Re-adding is a no-op, and adding a built-in word records nothing.
            assert!(
                !d.add_word("tilo"),
                "re-adding an existing user word is a no-op"
            );
            assert!(
                !d.add_word("alpha"),
                "adding a built-in word records nothing"
            );
        }
        // Phase `add_word_ignores_empty_and_whitespace`.
        {
            let mut d = Dictionary::from_words(["alpha"]);
            assert!(!d.add_word(""));
            assert!(!d.add_word("   "));
            assert_eq!(d.user_words().count(), 0);
        }
    }
}

#[cfg(test)]
mod product_vocabulary_tests {
    use super::*;

    #[test]
    fn product_nouns_are_known_but_ordinary_misspellings_are_caught() {
        // Phase `the_products_own_nouns_are_not_misspellings`.
        // The assistant this OS ships is not a misspelling.
        //
        // A red underline beneath `Claude` in a Tilo document is the system
        // reporting itself as an error, and it turned up in a document where
        // somebody had simply addressed the agent by name.
        {
            let d = Dictionary::builtin();
            for word in ["Claude", "claude", "Tilo", "TILO", "tilo", "tilo"] {
                assert!(
                    d.contains(word),
                    "{word} is a name this OS puts on screen itself, so its own \
                     spellchecker must not underline it",
                );
            }
        }
        // Phase `an_ordinary_misspelling_is_still_caught`.
        // And the general list still applies — a dictionary that accepted
        // everything would pass the test above for the wrong reason.
        {
            let d = Dictionary::builtin();
            assert!(!d.contains("teh"), "the general word list must still apply");
            assert!(d.contains("the"));
        }
    }
}
