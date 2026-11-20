//! Values the appearance settings show, read from the person's preferences.

use super::{AppearancePrefs, mode_label, schedule_label, ui_scale_label};
use crate::setting::Follows;
use crate::settings_app::SettingsApp;

fn prefs(app: &SettingsApp, follows: &Follows) -> AppearancePrefs {
    follows.get::<AppearancePrefs>(app.storage())
}

/// Light, dark or automatic: what people search for as "dark mode".
pub(crate) fn style(app: &SettingsApp, follows: &Follows) -> String {
    mode_label(&prefs(app, follows).mode)
}

pub(crate) fn schedule(app: &SettingsApp, follows: &Follows) -> String {
    schedule_label(&prefs(app, follows).schedule)
}

pub(crate) fn ui_scale(app: &SettingsApp, follows: &Follows) -> String {
    ui_scale_label(prefs(app, follows).ui_scale).to_owned()
}

pub(crate) fn wallpaper(app: &SettingsApp, follows: &Follows) -> Option<String> {
    match tilo_wallpapers::choice(prefs(app, follows).wallpaper.as_deref())? {
        tilo_wallpapers::Choice::Bundled(wallpaper) => Some(crate::i18n::t(&format!(
            "settings-wallpaper-{}",
            wallpaper.id
        ))),
        tilo_wallpapers::Choice::Custom => Some(crate::i18n::t("settings-appearance-your-picture")),
    }
}
