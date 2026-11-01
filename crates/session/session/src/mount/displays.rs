//! The panel's brightness, carried between quick settings and the session's
//! displays.
//!
//! `session:displays` publishes every display the session shows, how bright
//! each one is and which one the person works on, and the brightness verbs
//! that change them. The session lands that reading in the panel ([`land`])
//! and hands the panel's brightness effects to the displays service's actions
//! ([`follow`]), so the sliders, the keys, the settings app and a script all
//! read and change one thing: the resource.
//!
//! Nothing here decides what a change means: a step, "every display
//! together", which display the person works on and what a display with no
//! backlight does are the service's rules, and the reading it publishes
//! afterwards is what the panel shows.

use std::cell::RefCell;

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_quick_settings::device::SessionRequest;
use tilo_quick_settings::{QsEffect, focused_display_first};

use crate::{Shared, displays};

/// What the panel was last fed of the displays, so the session's reading is
/// read every pass but the panel is written only when what it shows moved.
#[derive(Default)]
pub(crate) struct Displays {
    fed: RefCell<Option<displays::Reading>>,
}

/// Land the session's displays in the panel, if this session has a device to
/// show them on. Returns whether the panel shows that reading now.
pub(crate) fn land(shared: &Shared) -> bool {
    let Some(device) = &shared.device else {
        return false;
    };
    let Some(reading) = shared
        .services
        .borrow()
        .read(displays::RESOURCE)
        .and_then(|reading| serde_json::from_value::<displays::Reading>(reading).ok())
    else {
        return false;
    };
    if shared.displays.fed.borrow().as_ref() == Some(&reading) {
        return true;
    }
    let mut levels: Vec<tilo_quick_settings::DisplayBrightness> = reading
        .displays
        .iter()
        .filter_map(|display| {
            Some(tilo_quick_settings::DisplayBrightness {
                connector: display.connector.clone(),
                name: display.name.clone(),
                level: display.brightness?,
            })
        })
        .collect();
    focused_display_first(&mut levels, reading.focused.as_deref());
    let brightness = reading.brightness();
    let focused = reading.focused.clone();
    *shared.displays.fed.borrow_mut() = Some(reading);
    device.update(|state| {
        state.brightness_available = brightness.is_some();
        if let Some(brightness) = brightness {
            state.brightness = brightness;
        }
        state.focused_display = focused;
        state.displays = levels;
    });
    true
}

/// Carry out a brightness effect quick settings asked of the desktop through
/// the session's displays. Returns whether this was one; `false` leaves it to
/// the other things the session follows.
pub(crate) fn follow(shared: &Shared, request: &SessionRequest) -> bool {
    let SessionRequest::Effect(effect) = request else {
        return false;
    };
    let Some((action, arguments)) = brightness_action(effect) else {
        return false;
    };
    let address = ActionAddress::new(displays::RESOURCE, action);
    match shared.doors.invoke(&address, &arguments) {
        Ok(_) => true,
        Err(error) => {
            // The reading is unchanged, so the panel's sliders show where the
            // displays are; a display with no backlight is answered by the
            // keys' readout, not by a notification.
            tracing::debug!(%error, %action, "the brightness did not change");
            true
        }
    }
}

/// The action of the displays resource that carries out `effect`, with its
/// arguments; `None` for an effect that does not change the brightness.
fn brightness_action(effect: &QsEffect) -> Option<(&'static str, Map<String, Value>)> {
    let mut arguments = Map::new();
    let action = match effect {
        QsEffect::SetBrightness { level } => {
            arguments.insert("level".into(), Value::from(*level));
            "set-brightness"
        }
        QsEffect::SetDisplayBrightness { display, level } => {
            arguments.insert("display".into(), Value::from(display.clone()));
            arguments.insert("level".into(), Value::from(*level));
            "set-display-brightness"
        }
        QsEffect::BrightnessUp => "brightness-up",
        QsEffect::BrightnessDown => "brightness-down",
        _ => return None,
    };
    Some((action, arguments))
}
