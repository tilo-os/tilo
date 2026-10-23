//! What the keyboard, pointing and spelling settings found by search are
//! set to, as their pages read them from the person's input and text
//! preferences.

use super::{
    CLICK_METHOD_CHOICES, InputPrefs, SCROLL_METHOD_CHOICES, SPELLING_LANGUAGES,
    TextAssistancePrefs, accel_shown, area, caps_lock_label, caps_lock_shown, click_method_id,
    compose_english, compose_shown, scroll_method_id,
};
use crate::i18n::t;
use crate::setting::{Follows, on_off};
use crate::settings_app::SettingsApp;

fn input(app: &SettingsApp, follows: &Follows) -> InputPrefs {
    follows.get::<InputPrefs>(app.host())
}

/// A slider's position, as a share of its track.
fn position(fraction: f64) -> String {
    ui::format::percent(fraction.clamp(0.0, 1.0))
}

pub(crate) fn layout(app: &SettingsApp, follows: &Follows) -> Option<String> {
    let active = input(app, follows).keyboard_layout;
    area(app)?.catalog.label_if_read(&active)
}

pub(crate) fn key_repeat(app: &SettingsApp, follows: &Follows) -> String {
    let input = input(app, follows);
    match input.key_repeat_rate_hz {
        Some(0) => t("input-key-repeat-off"),
        _ => position(input.key_repeat),
    }
}

pub(crate) fn compose(app: &SettingsApp, follows: &Follows) -> String {
    compose_shown(compose_english(input(app, follows).compose_key.as_deref()))
}

pub(crate) fn caps_lock(app: &SettingsApp, follows: &Follows) -> String {
    caps_lock_shown(caps_lock_label(input(app, follows).caps_lock.as_deref()))
}

/// Natural scrolling, as the touchpad has it: the mouse has its own.
pub(crate) fn natural_scrolling(app: &SettingsApp, follows: &Follows) -> String {
    on_off(input(app, follows).touchpad_natural_scroll)
}

pub(crate) fn touchpad_speed(app: &SettingsApp, follows: &Follows) -> String {
    let input = input(app, follows);
    position(input.touchpad_pointer_speed.unwrap_or(input.pointer_speed))
}

pub(crate) fn mouse_speed(app: &SettingsApp, follows: &Follows) -> String {
    let input = input(app, follows);
    position(input.mouse_pointer_speed.unwrap_or(input.pointer_speed))
}

pub(crate) fn scroll_speed(app: &SettingsApp, follows: &Follows) -> String {
    position(settings_store::hardware::scroll_speed_to_slider(
        input(app, follows).scroll_speed,
    ))
}

pub(crate) fn acceleration(app: &SettingsApp, follows: &Follows) -> String {
    accel_shown(&input(app, follows).accel_profile)
}

pub(crate) fn secondary_click(app: &SettingsApp, follows: &Follows) -> Option<String> {
    let id = click_method_id(input(app, follows).touchpad_click_method);
    CLICK_METHOD_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, key, _)| t(key))
}

pub(crate) fn scroll_method(app: &SettingsApp, follows: &Follows) -> Option<String> {
    let id = scroll_method_id(input(app, follows).touchpad_scroll_method);
    SCROLL_METHOD_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, key, _)| t(key))
}

pub(crate) fn spelling_language(app: &SettingsApp, follows: &Follows) -> String {
    let language = follows
        .get::<TextAssistancePrefs>(app.host())
        .spelling_language;
    if language.is_empty() {
        return t("input-spell-language-session");
    }
    SPELLING_LANGUAGES
        .iter()
        .find(|(id, _)| *id == language)
        .map_or_else(|| language.clone(), |(_, name)| (*name).to_owned())
}
