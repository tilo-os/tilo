//! The words a package shows, in the reader's language (docs/designs/app-language/i18n.md).
//!
//! A package's catalogues travel in its module; the runtime hands them to its [`Words`] as the
//! package loads, and resolves a declaration's label, description and noun through it where it
//! builds what a view reads. The host that draws decides the language: its `Words` resolves in
//! the person's locale. Without one the source words stand.
//!
//! A window shows one package's views as its own and runs the packages they use. The own
//! package's catalogues choose the window's language, once; a used package's words ride along
//! in it and never choose it ([`Words::add`], [`Words::add_used`]).

/// Where a runtime's words come from.
pub trait Words {
    /// Takes in the catalogues of the package the window shows, `(locale, Fluent text)`: the
    /// languages it ships are the ones the window's language is chosen from, with the person's
    /// preference (i18n.md §4). A host chooses on the first, and keeps it.
    fn add(&self, catalogues: &[(String, String)]);
    /// Takes in the catalogues of a package the window uses: its views run inline in the
    /// window's, and its words are read in the language the window speaks, never choosing it.
    /// A word of it the window's language lacks falls back to its source language, not to
    /// another of its translations.
    fn add_used(&self, catalogues: &[(String, String)]);
    /// The message `key` in the reader's language, given `args` (a declaration's `d-<id>`,
    /// `d-<id>.noun` with a `count`, or one of the platform's phrases); `source` when no
    /// catalogue holds it.
    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String;
    /// What the words resolve at now: it moves when the reader's language, or a catalogue,
    /// does. What a package published in one language is published again when it moves
    /// ([`crate::Words::word`] reads the language as it is, not as it was).
    fn revision(&self) -> u64 {
        0
    }
    /// The language the reader types in (`en`, `de`), as their words resolve it: what a date
    /// or a time typed in an entry is said in (`crate::natural::Language`). Empty when the
    /// host names none, which reads as English.
    fn language(&self) -> String {
        String::new()
    }
}

/// Which package of a window a catalogue belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordsRole {
    /// The package the window shows: the languages it ships are the ones the window's
    /// language is chosen from, once.
    Window,
    /// A package the window uses: its words ride along in the window's language.
    Used,
}

impl WordsRole {
    /// Hand `catalogues` to `words` as this role's: the window's own choose its language, a
    /// used package's ride along.
    pub fn hand(self, words: &dyn Words, catalogues: &[(String, String)]) {
        match self {
            Self::Window => words.add(catalogues),
            Self::Used => words.add_used(catalogues),
        }
    }
}

/// The source words: what a package's declarations say, untranslated.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourceWords;

impl Words for SourceWords {
    fn add(&self, _catalogues: &[(String, String)]) {}

    fn add_used(&self, _catalogues: &[(String, String)]) {}

    fn word(&self, _key: &str, _args: &[(String, String)], source: &str) -> String {
        source.to_owned()
    }
}
