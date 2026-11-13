//! The set of known words.

use std::collections::{BTreeMap, HashSet};

//! The embedded English word list, from the system `hunspell` `en_US` list (the
//! SCOWL-derived one Fedora ships): generated, never edited by hand.
const BUILTIN_WORDS: &str = include_str!("../data/words.txt");

/// Product names included in the built-in dictionary, and known beside a
/// system dictionary too ([`is_extra_word`]).
///
/// Membership is case-insensitive (see [`Dictionary::contains`]), so one
/// spelling covers `Tilo`, `TILO` and `tilo`.
const PRODUCT_WORDS: &[&str] = &["tilo", "claude", "tilo"];

/// Common contractions and proper names included in the embedded fallback,
/// and known beside a system dictionary too ([`is_extra_word`]).
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

pub(crate) fn is_extra_word(word: &str) -> bool {
    let lower = word.to_lowercase();
    PRODUCT_WORDS.contains(&lower.as_str()) || CONVERSATIONAL_WORDS.contains(&lower.as_str())
}

/// A set of known words for spell checking, matched case-insensitively: words are stored
/// lowercased and a lookup lowercases its argument.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    words: HashSet<String>,
    /// `by_len[n]` holds every word of `char`-length `n` (lowercased), so the
    /// suggester can restrict its scan to lengths within +-max of the query.
    by_len: Vec<Vec<String>>,
    /// Runtime-added user words, keyed case-insensitively while keeping the spelling the
    /// person accepted, apart from `words` so the built-in list stays compile-time data.
    user_words: BTreeMap<String, String>,
}

impl Dictionary {
    pub fn builtin() -> Self {
        let mut builder = BuiltinBuilder::default();
        builder.step(&mut || true);
        builder.finish()
    }

    /// Load the persisted user words into this dictionary's runtime set; a missing or
    /// unreadable file is a silent no-op.
    pub fn load_user_dict(&mut self) {
        if let Some(path) = user_dict_path() {
            self.load_user_dict_from(&path);
        }
    }

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

    /// Build a dictionary from any iterator of word-like strings: each entry trimmed and
    /// lowercased, empty entries and duplicates dropped.
    pub fn from_words<I, S>(words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut building = Building::default();
        for word in words {
            building.add(word.as_ref());
        }
        building.finish()
    }

    pub fn contains(&self, word: &str) -> bool {
        let apostrophe_normalized;
        let word = if word.contains('\u{2019}') {
            apostrophe_normalized = word.replace('\u{2019}', "'");
            apostrophe_normalized.as_str()
        } else {
            word
        };
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

    /// Persist the runtime user dictionary to `$XDG_CONFIG_HOME/tilo/userdict.txt`; no-ops
    /// when no config path is resolvable and surfaces I/O errors.
    pub fn save_user_dict(&self) -> std::io::Result<()> {
        match user_dict_path() {
            Some(path) => self.save_user_dict_to(&path),
            None => Ok(()),
        }
    }

    pub fn save_user_dict_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let words: Vec<&str> = self.user_words.values().map(String::as_str).collect();
        let body = words.join("\n");
        std::fs::write(path, body)
    }

    pub fn user_words(&self) -> impl Iterator<Item = &str> {
        self.user_words.keys().map(String::as_str)
    }

    /// Personal spellings as explicitly accepted, in stable lookup order: readers such as
    /// prediction and speech biasing use these rather than changing recognised names.
    pub fn user_spellings(&self) -> impl Iterator<Item = &str> {
        self.user_words.values().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

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

/// Words taken in so far, for [`Dictionary::from_words`] and the built-in
/// list's [`BuiltinBuilder`].
#[derive(Default)]
struct Building {
    words: HashSet<String>,
    by_len: Vec<Vec<String>>,
}

impl Building {
    fn add(&mut self, word: &str) {
        let normalized = word.trim().to_lowercase();
        if normalized.is_empty() || self.words.contains(&normalized) {
            return;
        }
        let len = normalized.chars().count();
        if self.by_len.len() <= len {
            self.by_len.resize(len.saturating_add(1), Vec::new());
        }
        // The resize above guarantees `len` is now in bounds.
        if let Some(bucket) = self.by_len.get_mut(len) {
            bucket.push(normalized.clone());
        }
        self.words.insert(normalized);
    }

    fn finish(self) -> Dictionary {
        Dictionary {
            words: self.words,
            by_len: self.by_len,
            user_words: BTreeMap::new(),
        }
    }
}

// The built-in dictionary is built a slice at a time: a browser page has no thread to
// build its 89,000 words on, and building them at once holds the page for frames.
pub(crate) struct BuiltinBuilder {
    words: Box<dyn Iterator<Item = &'static str> + Send>,
    building: Building,
}

impl Default for BuiltinBuilder {
    /// The only code that reads the embedded list ([`Dictionary::builtin`]
    /// builds through it too), never inlined: a web build's split moves the
    /// list into the one module whose code reaches it, and keeps it in the
    /// start module when two do.
    #[inline(never)]
    fn default() -> Self {
        Self {
            words: Box::new(
                BUILTIN_WORDS
                    .lines()
                    .chain(PRODUCT_WORDS.iter().copied())
                    .chain(CONVERSATIONAL_WORDS.iter().copied()),
            ),
            building: Building::default(),
        }
    }
}

impl BuiltinBuilder {
    const SLICE: usize = 256;

    pub(crate) fn step(&mut self, more: &mut dyn FnMut() -> bool) -> bool {
        loop {
            for _ in 0..Self::SLICE {
                let Some(word) = self.words.next() else {
                    return true;
                };
                self.building.add(word);
            }
            if !more() {
                return false;
            }
        }
    }

    #[must_use]
    pub(crate) fn finish(self) -> Dictionary {
        self.building.finish()
    }
}

/// Resolve the path of the persisted user dictionary: `$XDG_CONFIG_HOME/tilo/userdict.txt`,
/// else `$HOME/.config/tilo/userdict.txt`, else `None`.
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
        {
            let d = Dictionary::from_words(["Hello", "WORLD"]);
            assert!(d.contains("hello"));
            assert!(d.contains("HELLO"));
            assert!(d.contains("World"));
            assert!(!d.contains("goodbye"));
        }
        {
            let d = Dictionary::from_words(["  spaced  ", "", "   ", "ok"]);
            assert!(d.contains("spaced"));
            assert!(d.contains("ok"));
            assert_eq!(d.len(), 2);
        }
        {
            let d = Dictionary::from_words(["a", "to", "cat", "four", "fives"]);
            let got: HashSet<&str> = d.words_in_len_range(3, 4).collect();
            assert!(got.contains("cat"));
            assert!(got.contains("four"));
            assert!(!got.contains("to"));
            assert!(!got.contains("fives"));
        }
        {
            let d = Dictionary::builtin();
            assert!(d.len() > 10_000);
            assert!(d.contains("the"));
            assert!(d.contains("quick"));
            assert!(!d.contains("teh"));
        }
        {
            let mut d = Dictionary::from_words(["alpha", "beta"]);
            assert!(!d.contains("tilo"), "precondition: unknown word");
            assert!(d.add_word("Tilo"), "first add records the word");
            assert!(d.contains("tilo"), "added word is now known");
            assert!(d.contains("TILO"), "membership stays case-insensitive");
            assert!(
                !d.add_word("tilo"),
                "re-adding an existing user word is a no-op"
            );
            assert!(
                !d.add_word("alpha"),
                "adding a built-in word records nothing"
            );
        }
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
