//! The languages this desktop speaks, as Settings' Region page offers them: each with the tag a
//! setting names it by and its own words, so a person finds their language by reading it.
//!
//! The list is the desktop's own ([`ui::i18n::spoken_languages`]: the toolkit's languages and the
//! language packs installed beside them), never a second table that could drift from what a
//! window can actually show. Choosing one is the appearance section's `choose-language`, which
//! keeps nothing this desktop cannot speak.

use serde::{Deserialize, Serialize};

/// The resource the languages are published as.
pub const RESOURCE: &str = "settings/languages";
/// Its kind, as the reading's type is named and a view reads `settings.languages`.
pub const KIND: &str = "settings.languages";

/// One language the desktop speaks.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Language {
    /// The tag a setting holds it as (`de`).
    pub tag: String,
    /// Its name in its own words (`Deutsch`).
    pub name: String,
}

/// The languages this desktop speaks, in the order its catalogue names them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Languages {
    pub languages: Vec<Language>,
}

impl Languages {
    /// What this desktop speaks now.
    #[must_use]
    pub fn spoken() -> Self {
        Self {
            languages: ui::i18n::spoken_languages()
                .into_iter()
                .map(|(tag, name)| Language { tag, name })
                .collect(),
        }
    }

    /// Whether `tag` names one of the languages in this reading, by the tag a setting holds
    /// (`de` of `de-DE`).
    #[must_use]
    pub fn speaks(&self, tag: &str) -> bool {
        let language = ui::i18n::language_of(tag);
        self.languages.iter().any(|spoken| spoken.tag == language)
    }
}

/// The languages as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "settings.languages", label = "Languages")]
pub struct LanguagesReading;

/// The languages as a view of another language reads them (`settings.languages`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Languages, tilo_resource::NoEffect>(KIND, "Languages")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_speaks_english_and_german_and_names_them_itself() {
        let spoken = Languages::spoken();
        assert!(spoken.speaks("en-US"), "{spoken:?}");
        assert!(spoken.speaks("de-DE"), "{spoken:?}");
        assert!(!spoken.speaks("fr"), "{spoken:?}");
        let german = spoken
            .languages
            .iter()
            .find(|language| language.tag == "de")
            .expect("German");
        assert_eq!(german.name, "Deutsch");
    }
}
