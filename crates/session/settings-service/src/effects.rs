//! What a setting people look for by name offers: each is offered bound to
//! the setting ([`crate::SETTING_KIND`]), never on its own.

use tilo_resource::Effect;

/// A setting's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.settings.setting")]
pub enum SettingEffect {
    /// Turn the setting on when it is off, off when it is on.
    #[effect(row, icon = "toggle-right")]
    Switch,
    /// Set the setting to one of the few values it holds.
    #[effect(row, idempotent)]
    Choose {
        #[param("The value, as the setting stores it in JSON")]
        value: String,
    },
    /// Set the setting to a value.
    #[effect(row, idempotent)]
    Set {
        #[param("The value")]
        value: String,
    },
}
