//! What the person has learned of the shell's own controls, published: which control hints they
//! know, how often each showed, and whether hints show at all. The shell's hints write it as the
//! person learns ([`ui::control_hints::ControlHints`]), the profile card's switch and Settings
//! turn hints on and off and show them again, and every device of the person reads the one record.

use serde::{Deserialize, Serialize};
use settings_store::control_hints::Learned;

/// The person's control hints, as their settings hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct ControlHintsPrefs {
    /// What the person has learned, and whether hints show.
    #[serde(default)]
    pub learned: Learned,
}

impl settings_model::SettingsSchema for ControlHintsPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::control_hints::SECTION);
}

/// The hint `hint` names, when it is one of the shell's.
fn hint(hint: &str) -> Result<&'static str, ui::tilo_resource::Refusal> {
    ui::control_hints::shell::HINTS
        .iter()
        .map(|spec| spec.id)
        .find(|id| *id == hint)
        .ok_or_else(|| {
            let known: Vec<&str> = ui::control_hints::shell::HINTS
                .iter()
                .map(|spec| spec.id)
                .collect();
            ui::tilo_resource::Refusal::new(format!(
                "`{hint}` is not a control hint: {}",
                known.join(", ")
            ))
        })
}

#[ui::tilo_resource::resource(kind = "settings.control-hints")]
impl ControlHintsPrefs {
    /// Show the shell's control hints, or stop showing them: the person's "Show control hints".
    #[action(id = "show-control-hints")]
    pub fn show_control_hints(
        &mut self,
        #[param("Whether control hints show")] enabled: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.learned.enabled = enabled;
        Ok(())
    }

    /// The person showed they know the control `hint` teaches: it never shows again.
    #[action(id = "learn-control-hint")]
    pub fn learn_control_hint(
        &mut self,
        #[param("The hint, by name")] hint: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.learned.learn(self::hint(&hint)?);
        Ok(())
    }

    /// The hint `hint` began to show: it counts as shown.
    #[action(id = "note-control-hint-shown")]
    pub fn note_control_hint_shown(
        &mut self,
        #[param("The hint, by name")] hint: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.learned.note_shown(self::hint(&hint)?);
        Ok(())
    }

    /// The hint `hint` began but never reached the screen: its last showing does not count.
    #[action(id = "withdraw-control-hint-shown")]
    pub fn withdraw_control_hint_shown(
        &mut self,
        #[param("The hint, by name")] hint: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.learned.withdraw_shown(self::hint(&hint)?);
        Ok(())
    }

    /// Show every control hint again, as to someone new: nothing counts as learned or shown.
    #[action(id = "show-control-hints-again")]
    pub fn show_control_hints_again(&mut self) -> Result<(), ui::tilo_resource::Refusal> {
        self.learned.known.clear();
        self.learned.shown.clear();
        Ok(())
    }
}
