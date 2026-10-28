//! What the display settings found by search are set to, as the page
//! reads them: the display comforts as stored, and the selected display
//! (the primary one unless another was chosen) as last read. Nothing of
//! the displays until the area has read them.

use super::{
    DisplayModel, DisplayOutput, OutputConfig, area, refresh_label, resolution_label, scale_label,
    temperature_label, transform_label,
};
use crate::i18n::t;
use crate::prefs::DisplayPrefs;
use crate::setting::{Follows, Read, on_off};
use crate::settings_app::SettingsApp;

fn prefs(app: &SettingsApp, follows: &Follows) -> DisplayPrefs {
    follows.get::<DisplayPrefs>(app.host())
}

/// `read` of the selected display and how it is set, once the displays
/// were read.
fn selected<V>(
    app: &SettingsApp,
    read: impl FnOnce(&DisplayModel, &DisplayOutput, &OutputConfig) -> Option<V>,
) -> Option<V> {
    let model = area(app)?.read()?;
    let model = model.borrow();
    let output = model.selected_output()?;
    let config = model
        .staged()
        .outputs
        .get(&output.connector)
        .cloned()
        .unwrap_or_else(|| output.unconfigured());
    read(&model, output, &config)
}

pub(crate) fn night_light_warmth(app: &SettingsApp, follows: &Follows) -> String {
    temperature_label(prefs(app, follows).night_light_temperature)
}

/// Automatic brightness, a switch once the displays were read and one has
/// a light sensor.
pub(crate) fn auto_brightness(app: &SettingsApp, follows: &Follows) -> Read {
    let on = prefs(app, follows).auto_brightness;
    match area(app).and_then(super::DisplayArea::read) {
        Some(model) if !model.borrow().ambient_sensor_available => {
            Read::Value(Some(t("display-no-ambient")))
        }
        Some(_) => Read::Switch(on),
        None => Read::Value(Some(on_off(on))),
    }
}

pub(crate) fn brightness(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |model, output, _| {
        (output.builtin && model.brightness_error.is_none())
            .then(|| ui::format::percent(f64::from(model.brightness)))
    })
}

pub(crate) fn scale(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |_, _, config| {
        Some(scale_label(config.scale).to_owned())
    })
}

pub(crate) fn resolution(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |_, output, config| {
        config.mode.or(output.current_mode).map(resolution_label)
    })
}

pub(crate) fn refresh_rate(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |_, output, config| {
        config.mode.or(output.current_mode).map(refresh_label)
    })
}

pub(crate) fn rotation(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |_, _, config| Some(transform_label(config.transform)))
}

pub(crate) fn hdr(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |_, output, config| {
        Some(if output.hdr {
            on_off(config.hdr)
        } else {
            t(super::hdr_unavailable_key(
                output.sink_hdr,
                output.hdr_unavailable_reason,
            ))
        })
    })
}

pub(crate) fn vrr(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |_, output, config| {
        Some(if output.vrr {
            on_off(config.vrr)
        } else {
            t(super::adaptive_sync_unavailable_key(
                output.sink_vrr,
                output.vrr_unavailable_reason,
            ))
        })
    })
}

pub(crate) fn mirroring(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |model, _, config| {
        Some(match &config.mirror_of {
            Some(target) => model
                .connected_outputs()
                .into_iter()
                .find(|candidate| candidate.connector == *target)
                .map_or_else(|| target.clone(), |target| target.label.clone()),
            None => on_off(false),
        })
    })
}

pub(crate) fn colour_profile(app: &SettingsApp, _: &Follows) -> Option<String> {
    selected(app, |_, output, config| {
        Some(
            config
                .color_profile
                .as_ref()
                .and_then(|id| output.color_profile_labels.get(id).cloned())
                .unwrap_or_else(|| t("display-colour-profile-empty")),
        )
    })
}

pub(crate) fn arrangement(app: &SettingsApp, _: &Follows) -> Option<String> {
    let model = area(app)?.read()?;
    let count = model.borrow().connected_outputs().len();
    (count > 0).then(|| {
        crate::i18n::t_args(
            "display-arrangement-count",
            &[("count", i64::try_from(count).unwrap_or(i64::MAX).into())],
        )
    })
}
