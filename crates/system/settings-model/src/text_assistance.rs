//! The shared spelling preferences: the per-user `text_assistance` section every app's text
//! fields follow.

pub const SETTINGS_SECTION: &str = "text_assistance";

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::Model,
)]
#[model(defaults)]
pub struct TextAssistancePrefs {
    /// Underline words the spell checker does not know, in every text field.
    pub check_spelling: bool,
    /// Replace common misspellings with the word the checker suggests as they are typed.
    pub auto_correct: bool,
    /// The language the checker reads, as a BCP 47 tag; empty uses the person's interface language.
    #[serde(default)]
    pub spelling_language: String,
}

impl Default for TextAssistancePrefs {
    fn default() -> Self {
        Self {
            check_spelling: true,
            auto_correct: true,
            spelling_language: String::new(),
        }
    }
}

impl crate::SettingsSchema for TextAssistancePrefs {
    const SCOPE: crate::Scope = crate::Scope::Shared(SETTINGS_SECTION);
}

/// The section as a record the person edits by path.
#[tilo_resource::resource(kind = "settings.text-assistance")]
impl TextAssistancePrefs {}
