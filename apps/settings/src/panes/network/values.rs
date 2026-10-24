//! What the network settings found by search are set to, as the page last
//! read them, and the radios' switches: nothing until the area has read
//! the device.

use ui::LocalState;
use ui::tilo_resource::ActionRef;

use super::{NetworkEffect, NetworkModel, area};
use crate::effects::effect;
use crate::host::PaneCx;
use crate::i18n::t;
use crate::setting::{Follows, Read, on_off};
use crate::settings_app::SettingsApp;
use crate::widgets::Item;

fn read<V>(app: &SettingsApp, read: impl FnOnce(&NetworkModel) -> Option<V>) -> Option<V> {
    let model = area(app)?.read()?;
    let model = model.borrow();
    read(&model)
}

/// A switch the page shows on a card of its own, as a row: labelled
/// `label`, setting the radio through `set` as the card's switch does.
fn switch(
    cx: &PaneCx,
    label: &str,
    id: &str,
    on: bool,
    set: ActionRef<NetworkEffect, bool>,
) -> Item {
    cx.row(t(label))
        .id(id)
        .toggle_effect(&LocalState::new(on), effect(set))
}

/// Wi-Fi, a switch while the radio's state was read and nothing holds it
/// off.
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

pub(crate) fn wifi_switch(cx: &PaneCx, on: bool) -> Item {
    switch(cx, "net-wifi", "network.wifi", on, NetworkEffect::SET_WIFI)
}

/// The wired connection, a switch where there is a wired device.
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

pub(crate) fn wired_switch(cx: &PaneCx, on: bool) -> Item {
    switch(
        cx,
        "net-wired",
        "network.wired",
        on,
        NetworkEffect::SET_WIRED,
    )
}

/// Airplane mode, a switch once the radios were read.
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

pub(crate) fn airplane_switch(cx: &PaneCx, on: bool) -> Item {
    switch(
        cx,
        "net-airplane",
        "network.airplane",
        on,
        NetworkEffect::SET_AIRPLANE_MODE,
    )
}

/// The hotspot, a switch where there is Wi-Fi to share over.
pub(crate) fn hotspot(app: &SettingsApp, _: &Follows) -> Read {
    read(app, |model| {
        (!model.wifi_absent()).then_some(model.radios.hotspot_active)
    })
    .map_or(Read::Value(None), Read::Switch)
}

pub(crate) fn hotspot_switch(cx: &PaneCx, on: bool) -> Item {
    switch(
        cx,
        "net-hotspot",
        "network.hotspot",
        on,
        NetworkEffect::SET_HOTSPOT,
    )
}
