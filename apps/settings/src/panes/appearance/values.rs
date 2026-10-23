//! What the appearance settings found by search are set to, as the page
//! reads them from the person's appearance preferences.

use super::{AppearancePrefs, ThemeTiles, schedule_label, ui_scale_label};
use crate::setting::Follows;
use crate::settings_app::SettingsApp;

fn prefs(app: &SettingsApp, follows: &Follows) -> AppearancePrefs {
    follows.get::<AppearancePrefs>(app.host())
}

pub(crate) fn style(app: &SettingsApp, follows: &Follows) -> String {
    ThemeTiles::label_of(&prefs(app, follows).theme_choice())
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
