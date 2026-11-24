//! The person's own words as a resource: the spelling check keeps them, the
//! word processor, the on-screen keyboard and dictation read them, and every
//! one of the person's devices keeps the same ones.
//!
//! The words are the `personal_dictionary` section of the settings document,
//! one value so a change reaches the person's other devices whole. A view
//! reads them here and changes them through [`PersonalDictionary::add`] and
//! [`PersonalDictionary::remove`], which check each word the same way the
//! spelling check does; the revision the words carry is what the apps served
//! the dictionary follow, so an editor elsewhere sees the same change.

use serde::{Deserialize, Serialize};
use settings_store::personal_dictionary::{Edit, Snapshot, Word};
use ui::tilo_resource::{self, Refusal};

/// The person's words: the explicit additions their spelling check keeps, and
/// the revision an edit names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[model(defaults)]
#[serde(default)]
pub struct PersonalDictionary {
    /// The words and their revision, as the settings store keeps them: a word
    /// spelling and the language tag it is used in, or none for every
    /// language.
    #[model(readonly)]
    pub words: Snapshot,
}

impl settings_model::SettingsSchema for PersonalDictionary {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::personal_dictionary::SECTION);
    settings_model::settings_codec!();
}

impl PersonalDictionary {
    /// The words after `edit`, kept only when they are a valid dictionary the
    /// device fabric still carries.
    fn edit(&mut self, edit: Edit) -> Result<(), Refusal> {
        let next = self
            .words
            .preview(edit)
            .map_err(|error| Refusal::new(error.to_string()))?;
        let bytes =
            serde_json::to_vec(&next.words).map_err(|error| Refusal::new(error.to_string()))?;
        if bytes.len() > settings_model::MAX_PERSONAL_VALUE_BYTES {
            return Err(Refusal::new("the personal dictionary is full"));
        }
        self.words = next;
        Ok(())
    }
}

fn word(spelling: &str, language: Option<&str>) -> Result<Word, Refusal> {
    Word::new(spelling, language).map_err(|error| Refusal::new(error.to_string()))
}

#[tilo_resource::resource(kind = "settings.personal-dictionary")]
impl PersonalDictionary {
    /// Add one word: it is then spelled as written, in every language or in
    /// the one its language names, without being marked.
    #[action(id = "add", keywords = "dictionary, spelling, word, vocabulary")]
    pub fn add(
        &mut self,
        #[param("The word, spelled as it should be kept")] spelling: String,
        #[param("A language tag, or none for every language")] language: Option<String>,
    ) -> Result<(), Refusal> {
        self.edit(Edit::Add(word(&spelling, language.as_deref())?))
    }

    /// Remove one word. A word is named as it is written; its letters' case
    /// does not matter.
    #[action(id = "remove", keywords = "dictionary, spelling, word")]
    pub fn remove(
        &mut self,
        #[param("The word, as the list shows it")] spelling: String,
        #[param("Its language tag, or none")] language: Option<String>,
    ) -> Result<(), Refusal> {
        self.edit(Edit::Remove(word(&spelling, language.as_deref())?))
    }

    /// Remove every word the spelling check kept.
    #[action(id = "clear", keywords = "dictionary, spelling, forget")]
    pub fn clear(&mut self) -> Result<(), Refusal> {
        self.edit(Edit::Reset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    /// A word added through the section is in the words, a word in another
    /// language is kept apart from it, and removing one leaves the rest.
    #[test]
    fn a_word_is_added_in_its_language_and_removed_as_written() {
        let mut dictionary = Replica::<PersonalDictionary>::new();
        dictionary
            .apply(PersonalDictionary::ADD.with(("Tilo".to_owned(), None)))
            .unwrap();
        dictionary
            .apply(PersonalDictionary::ADD.with(("Élodie".to_owned(), Some("fr".to_owned()))))
            .unwrap();
        let words = &dictionary.state().unwrap().words;
        assert_eq!(words.words.len(), 2);
        assert_eq!(words.words[0].spelling, "Tilo");
        assert_eq!(words.words[0].language, None);
        assert_eq!(words.words[1].spelling, "Élodie");
        assert_eq!(words.words[1].language.as_deref(), Some("fr"));
        assert_eq!(words.revision, 2);
        let before = dictionary.state().unwrap();
        dictionary
            .apply(PersonalDictionary::REMOVE.with(("tilo".to_owned(), None)))
            .unwrap();
        let after = dictionary.state().unwrap();
        assert_eq!(after.words.words.len(), 1);
        assert_eq!(after.words.words[0].spelling, "Élodie");
        assert_eq!(after.words.revision, before.words.revision + 1);
    }

    /// A word that cannot be one is refused, naming why, and nothing changes.
    #[test]
    fn a_word_that_is_no_word_is_refused() {
        let mut dictionary = Replica::<PersonalDictionary>::new();
        let refused = dictionary
            .apply(PersonalDictionary::ADD.with(("two words".to_owned(), None)))
            .unwrap_err();
        assert!(format!("{refused:?}").contains("one word"), "{refused:?}");
        assert!(dictionary.state().unwrap().words.words.is_empty());
        let refused = dictionary
            .apply(PersonalDictionary::ADD.with(("Tilo".to_owned(), Some("../en".to_owned()))))
            .unwrap_err();
        assert!(
            format!("{refused:?}").contains("language tag"),
            "{refused:?}"
        );
        assert!(dictionary.state().unwrap().words.words.is_empty());
    }

    /// The section is stored as the settings document keeps it: the words and
    /// their revision under one leaf, which the spelling check reads back.
    #[test]
    fn the_section_is_the_document_it_keeps() {
        let mut dictionary = PersonalDictionary::default();
        dictionary
            .add("Tilo".to_owned(), None)
            .expect("a word to add");
        let stored = serde_json::to_value(&dictionary).expect("the section writes");
        assert_eq!(
            stored,
            serde_json::json!({
                "words": {
                    "revision": 1,
                    "words": [{"spelling": "Tilo", "language": null}],
                }
            })
        );
        let read = settings_store::personal_dictionary::snapshot_of(
            &serde_json::json!({"personal_dictionary": stored})
                .as_object()
                .cloned()
                .unwrap(),
        );
        assert_eq!(read, dictionary.words);
    }
}
