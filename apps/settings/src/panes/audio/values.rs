//! What the sound settings found by search are set to, as the page reads
//! them: the stored preferences, and the devices and levels as last read
//! (nothing of those until the area has read them).

use super::{AudioModel, area};
use crate::i18n::{t, t_args};
use crate::prefs::AudioPrefs;
use crate::setting::{Follows, on_off};
use crate::settings_app::SettingsApp;

fn prefs(app: &SettingsApp, follows: &Follows) -> AudioPrefs {
    follows.get::<AudioPrefs>(app.host())
}

fn read<V>(app: &SettingsApp, read: impl FnOnce(&AudioModel) -> Option<V>) -> Option<V> {
    let model = area(app)?.model.get()?;
    let model = model.borrow();
    read(&model)
}

pub(crate) fn volume(app: &SettingsApp, _: &Follows) -> Option<String> {
    read(app, |model| {
        model
            .volume_error
            .is_none()
            .then(|| ui::format::percent(f64::from(model.volume)))
    })
}

pub(crate) fn mute(app: &SettingsApp, _: &Follows) -> Option<String> {
    read(app, |model| {
        model.volume_error.is_none().then(|| on_off(model.muted))
    })
}

pub(crate) fn output_device(app: &SettingsApp, _: &Follows) -> Option<String> {
    read(app, |model| model.devices.default_sink().map(str::to_owned))
}

pub(crate) fn input_device(app: &SettingsApp, _: &Follows) -> Option<String> {
    read(app, |model| {
        model.devices.default_source().map(str::to_owned)
    })
}

pub(crate) fn input_level(app: &SettingsApp, _: &Follows) -> Option<String> {
    read(app, |model| {
        model
            .input_error
            .is_none()
            .then(|| ui::format::percent(f64::from(model.input_volume)))
    })
}

pub(crate) fn balance(app: &SettingsApp, follows: &Follows) -> String {
    let balance = f64::from(prefs(app, follows).balance.clamp(0.0, 1.0));
    let off_centre = (balance - 0.5) * 2.0;
    if off_centre.abs() < 0.01 {
        t("sound-balance-centred")
    } else {
        t_args(
            if off_centre < 0.0 {
                "sound-balance-left-by"
            } else {
                "sound-balance-right-by"
            },
            &[("amount", ui::format::percent(off_centre.abs()).into())],
        )
    }
}

pub(crate) fn mono(app: &SettingsApp, follows: &Follows) -> String {
    on_off(prefs(app, follows).mono_output)
}

pub(crate) fn echo(app: &SettingsApp, follows: &Follows) -> String {
    on_off(prefs(app, follows).echo_cancellation)
}

pub(crate) fn system_sounds(app: &SettingsApp, follows: &Follows) -> String {
    on_off(prefs(app, follows).system_sounds.enabled)
}
