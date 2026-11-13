//! The names a desk holds, as a person reads them: the name they gave, or the word the desk calls
//! what nobody has named.
//!
//! A desk stores no name for what the person left unnamed — a workspace they never named, the strip
//! a workspace comes with — so the record holds the same nothing on every device, and each person
//! reads it in their own language.

use crate::i18n::{t, t_args};

/// The name a person reads for a workspace called `name` : the name they gave it, or the word for
/// a workspace nobody has named.
pub fn workspace(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        t("desk-unnamed-workspace")
    } else {
        name.to_owned()
    }
}

/// The name a person reads for the strip in place `at` that is called `name` : the name it was
/// given, or where it stands, the first being strip 1. The strip switcher names a strip the same
/// way.
pub fn strip_named(name: &str, at: usize) -> String {
    let name = name.trim();
    if name.is_empty() {
        t_args(
            "desk-unnamed-strip",
            &[("number", at.saturating_add(1).to_string().into())],
        )
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_or_a_strip_nobody_named_reads_as_the_desks_own_word() {
        crate::i18n::ensure();
        assert_eq!(workspace("Research"), "Research");
        assert_eq!(workspace("  "), "Workspace");
        assert_eq!(strip_named("Side", 1), "Side");
        assert_eq!(
            strip_named("", 0),
            "Strip 1",
            "a strip nobody named stands in its place"
        );
    }

    /// A German desk reads its own words for the names nobody gave, and the names it was given as
    /// they are: one record, read in the language of the person reading it.
    #[test]
    fn a_german_desk_names_what_nobody_named_in_german() {
        crate::i18n::ensure();
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(workspace(""), "Arbeitsbereich");
        assert_eq!(strip_named("", 0), "Leiste 1");
        assert_eq!(workspace("Forschung"), "Forschung");
        ui::i18n::I18nManager::set_locale("en-US");
    }
}
