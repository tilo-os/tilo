//! What the power settings found by search are set to, as the page reads
//! them: the stored timers and actions, and what the hardware last
//! reported.

use super::{
    PowerPrefs, action_shown, area, blank_label, lock_label, profile_label, suspend_label,
};
use crate::setting::Follows;
use crate::settings_app::SettingsApp;

fn prefs(app: &SettingsApp, follows: &Follows) -> PowerPrefs {
    follows.get::<PowerPrefs>(app.host())
}

pub(crate) fn profile(app: &SettingsApp, _: &Follows) -> Option<String> {
    let state = area(app)?.state.get()?;
    (!state.profile_unread && !state.available_profiles.is_empty())
        .then(|| profile_label(state.profile))
}

pub(crate) fn charge_limit(app: &SettingsApp, _: &Follows) -> Option<String> {
    let state = area(app)?.state.get()?;
    Some(match state.charge_cap {
        Some(cap) => ui::format::percent(f64::from(cap.current) / 100.0),
        None => crate::i18n::t("power-charge-cap-unsupported"),
    })
}

pub(crate) fn suspend_after(app: &SettingsApp, follows: &Follows) -> String {
    let prefs = prefs(app, follows);
    let timers = prefs.timers();
    if !prefs.auto_suspend {
        crate::setting::on_off(false)
    } else if timers.profiles_differ() {
        crate::i18n::t("power-suspend-in-profile-value")
    } else {
        suspend_label(timers.plugged_in())
    }
}

pub(crate) fn blank(app: &SettingsApp, follows: &Follows) -> String {
    blank_label(prefs(app, follows).blank_minutes)
}

pub(crate) fn lock_after(app: &SettingsApp, follows: &Follows) -> String {
    lock_label(prefs(app, follows).lock_minutes)
}

pub(crate) fn lid(app: &SettingsApp, follows: &Follows) -> String {
    action_shown(prefs(app, follows).lid_close_action)
}

pub(crate) fn power_key(app: &SettingsApp, follows: &Follows) -> String {
    action_shown(prefs(app, follows).power_key_action)
}

pub(crate) fn critical(app: &SettingsApp, follows: &Follows) -> String {
    action_shown(prefs(app, follows).critical_action)
}
