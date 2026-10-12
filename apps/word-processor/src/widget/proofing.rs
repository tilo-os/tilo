//! Spell-check span preparation, independent of widget cache and painting,
//! and the person's own dictionary.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

/// The words this person added to the dictionary. They are never seen by
/// collaborators: a document shared with someone else does not change what
/// is spelled right for them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonalDictionary {
    pub dictionary_words: Vec<String>,
}

/// How this person likes the editor laid out: which panels are open. It
/// follows them to each of their devices; it is never part of a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterLayout {
    #[serde(default = "shown")]
    pub show_ruler: bool,
    #[serde(default)]
    pub show_styles_panel: bool,
    #[serde(default)]
    pub show_outline: bool,
}

fn shown() -> bool {
    true
}

impl Default for WriterLayout {
    fn default() -> Self {
        Self {
            show_ruler: true,
            show_styles_panel: false,
            show_outline: false,
        }
    }
}

impl SettingsSchema for WriterLayout {
    const SCOPE: Scope = Scope::Personal;
}

impl PersonalDictionary {
    /// The dictionary `built_in` with these words added.
    pub(super) fn over(&self, built_in: &spellcheck::Dictionary) -> spellcheck::Dictionary {
        let mut dictionary = built_in.clone();
        for word in &self.dictionary_words {
            dictionary.add_word(word);
        }
        dictionary
    }

    /// Add `word`; `false` when it is already there.
    pub(super) fn add(&mut self, word: &str) -> bool {
        if self.dictionary_words.iter().any(|known| known == word) {
            return false;
        }
        self.dictionary_words.push(word.to_owned());
        true
    }
}

pub(super) fn misspelled_spans(
    dictionary: &spellcheck::Dictionary,
    language: &str,
    text: &str,
) -> Vec<(usize, usize)> {
    let mut spans = spellcheck::spans_for_language(language, text);
    spans.retain(|&(start, end)| {
        text.get(start..end)
            .is_some_and(|word| !dictionary.contains(word))
    });
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_words_join_the_built_in_dictionary_once() {
        let built_in = spellcheck::Dictionary::from_words(["hello"]);
        let mut personal = PersonalDictionary::default();
        assert!(personal.add("qwordz"));
        assert!(!personal.add("qwordz"), "a word is added once");
        let dictionary = personal.over(&built_in);
        assert!(dictionary.contains("qwordz"));
        assert!(dictionary.contains("hello"));
        assert!(
            !built_in.contains("qwordz"),
            "the built-in list is untouched"
        );
    }

    #[test]
    fn user_dictionary_words_are_removed_without_losing_byte_offsets() {
        let dictionary = spellcheck::Dictionary::from_words(["qwordz"]);
        assert_eq!(
            misspelled_spans(&dictionary, "en-US", "qwordz wrold"),
            vec![(7, 12)]
        );
    }
}
