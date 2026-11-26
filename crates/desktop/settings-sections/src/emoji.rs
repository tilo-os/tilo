//! How often a person used each emoji, in the picker's frequent row: their own setting, so every
//! device of theirs opens the picker with the same ones.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// The most the picker counts: its own list's length.
pub const KEPT: usize = tilo_ui_base::emoji_catalogue::EmojiUses::KEPT;

/// How often the person used each emoji.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct EmojiUses {
    /// Each emoji and how many times, as JSON: most recently used first.
    #[model(readonly)]
    pub used: String,
}

impl settings_model::SettingsSchema for EmojiUses {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::emoji::SECTION);
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.emoji")]
impl EmojiUses {
    /// Keep these counts of the emoji the person used, as JSON: most recently used first.
    #[action(id = "set-used")]
    pub fn set_used(
        &mut self,
        #[param("Each emoji and how many times, as JSON: most recently used first")] used: String,
    ) -> Result<(), tilo_resource::Refusal> {
        let counted: Vec<serde_json::Value> = serde_json::from_str(&used)
            .map_err(|_| tilo_resource::Refusal::new("the counts are a JSON list"))?;
        if counted.len() > KEPT {
            return Err(tilo_resource::Refusal::new(format!(
                "the picker counts at most {KEPT} emoji"
            )));
        }
        self.used = used;
        Ok(())
    }
}
