//! What the sound settings found by search are set to, as the page reads
//! them: the stored preferences, and the devices and levels as last read
//! (nothing of those until the area has read them).

use super::{AudioModel, area};
use crate::i18n::{t, t_args};
use crate::prefs::AudioPrefs;
use crate::setting::{Follows, Read, on_off};
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

/// Muting, a switch once the output level was read.
pub(crate) fn mute(app: &SettingsApp, _: &Follows) -> Read {
    read(app, |model| {
        model.volume_error.is_none().then_some(model.muted)
    })
    .map_or(Read::Value(None), Read::Switch)
}

/// A stored yes-or-no, a switch where the area read that this device
/// `offers` it.
fn switch_where(app: &SettingsApp, offers: impl FnOnce(&AudioModel) -> bool, on: bool) -> Read {
    if read(app, |model| Some(offers(model))).unwrap_or(false) {
        Read::Switch(on)
    } else {
        Read::Value(Some(on_off(on)))
    }
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

pub(crate) fn mono(app: &SettingsApp, follows: &Follows) -> Read {
    switch_where(
        app,
        AudioModel::offers_mono,
        prefs(app, follows).mono_output,
    )
}

pub(crate) fn echo(app: &SettingsApp, follows: &Follows) -> Read {
    switch_where(
        app,
        AudioModel::offers_echo_cancellation,
        prefs(app, follows).echo_cancellation,
    )
}

pub(crate) fn system_sounds(app: &SettingsApp, follows: &Follows) -> Read {
    switch_where(
        app,
        |model| model.event_sounds_available,
        prefs(app, follows).system_sounds.enabled,
    )
}
