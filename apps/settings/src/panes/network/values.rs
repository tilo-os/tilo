//! What the network settings found by search are set to, as the page last read
//! them, and the radios' switches: nothing until the area has read the device.

use provider_settings_ui::bound::SettingRows as _;
use ui::LocalState;
use ui::framework::ViewCx;
use ui::tilo_resource::ActionRef;

use super::{NetworkEffect, NetworkModel, area};
use crate::effects::effect;
use crate::i18n::t;
use crate::resource::Settings;
use crate::setting::{Follows, Read, on_off};
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::Item;

fn read<V>(app: &SettingsApp, read: impl FnOnce(&NetworkModel) -> Option<V>) -> Option<V> {
    let model = area(app)?.read()?;
    let model = model.borrow();
    read(&model)
}

fn switch(
    cx: &ViewCx<Settings>,
    label: &str,
    id: &str,
    on: bool,
    set: ActionRef<NetworkEffect, bool>,
) -> Item {
    cx.setting(t(label))
        .id(id)
        .toggle_effect(&LocalState::new(on), &cx.actions(), effect(set))
}

pub(crate) fn wifi(app: &SettingsApp, _: &Follows) -> Read {
    read(app, |model| {
        Some(if model.wifi_absent() {
            Read::Value(Some(t("net-no-wifi-adapter")))
        } else if model.read_errors.wifi_enabled.is_some() {
            Read::Value(None)
        } else if model.radios.wifi_hardware_blocked {
            Read::Value(Some(on_off(model.wifi_enabled)))
        } else {
            Read::Switch(model.wifi_enabled)
        })
    })
    .unwrap_or(Read::Value(None))
}

pub(crate) fn wifi_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
    switch(cx, "net-wifi", "network.wifi", on, NetworkEffect::SET_WIFI)
}

pub(crate) fn wired(app: &SettingsApp, _: &Follows) -> Read {
    read(app, |model| {
        model
            .wired
            .device
            .is_some()
            .then_some(model.wired.connected)
    })
    .map_or(Read::Value(None), Read::Switch)
}

pub(crate) fn wired_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
    switch(
        cx,
        "net-wired",
        "network.wired",
        on,
        NetworkEffect::SET_WIRED,
    )
}

pub(crate) fn airplane(app: &SettingsApp, _: &Follows) -> Read {
    read(app, |model| {
        model
            .read_errors
            .radios
            .is_none()
            .then_some(model.radios.airplane_mode)
    })
    .map_or(Read::Value(None), Read::Switch)
}

pub(crate) fn airplane_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
    switch(
        cx,
        "net-airplane",
        "network.airplane",
        on,
        NetworkEffect::SET_AIRPLANE_MODE,
    )
}

pub(crate) fn hotspot(app: &SettingsApp, _: &Follows) -> Read {
    read(app, |model| {
        (!model.wifi_absent()).then_some(model.radios.hotspot_active)
    })
    .map_or(Read::Value(None), Read::Switch)
}

pub(crate) fn hotspot_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
    switch(
        cx,
        "net-hotspot",
        "network.hotspot",
        on,
        NetworkEffect::SET_HOTSPOT,
    )
}
