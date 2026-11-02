//! The words a package shows, in the reader's language (docs/designs/app-language/i18n.md).
//!
//! A package's catalogues travel in its module; the runtime hands them to its [`Words`] as the
//! package loads, and resolves a declaration's label, description and noun through it where it
//! builds what a view reads. The host that draws decides the language: its `Words` resolves in
//! the person's locale. Without one the source words stand.

/// Where a runtime's words come from.
pub trait Words {
    /// Takes in a package's catalogues, `(locale, Fluent text)`.
    fn add(&self, catalogues: &[(String, String)]);
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
}

/// The source words: what a package's declarations say, untranslated.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourceWords;

impl Words for SourceWords {
    fn add(&self, _catalogues: &[(String, String)]) {}

    fn word(&self, _key: &str, _args: &[(String, String)], source: &str) -> String {
        source.to_owned()
    }
}
