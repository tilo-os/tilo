//! The input devices attached to this machine, published: its keyboards, pointers, touchscreens
//! and drawing tablets, with a pad's buttons, rings and strips.
//!
//! This is the one reader of the compositor's input snapshot ([`InputHardwareSnapshot`], written
//! beside the display snapshot in the private runtime directory). Settings draws the tablets'
//! buttons, dials and pens over it, and an agent reads the same reading instead of the file.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

use crate::i18n::t;

/// The resource the input devices are published as.
pub const RESOURCE: &str = "session:input";

/// A device the person's hand meets, as a view of another language reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct InputDevice {
    /// The identity its settings are kept under: vendor, product and the kernel's name.
    pub key: String,
    /// What the device calls itself.
    pub name: String,
    /// What it is: `touchpad`, `mouse`, `keyboard`, `touchscreen`, `tablet`, `tablet_pad` or
    /// `other`.
    pub kind: String,
    /// How many buttons a drawing tablet's pad has.
    pub pad_buttons: u32,
    /// Each of a pad's touch rings, by how many modes its mode group has.
    pub pad_rings: Vec<u32>,
    /// Each of a pad's strips, the same way.
    pub pad_strips: Vec<u32>,
    /// The buttons that change a ring's or strip's mode.
    pub pad_mode_buttons: Vec<u32>,
    /// The device this one is part of, as a pen is part of its tablet; empty when it is alone.
    pub physical_device: String,
}

/// The input hardware as one reads it now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Reading {
    /// The devices attached now.
    pub devices: Vec<InputDevice>,
    /// The last keyboard typed on, while one is.
    pub active_keyboard: Option<String>,
}

/// The input hardware as a view of another language reads it (`device.input`).
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Reading, NoEffect>("input", "Input devices")
}

/// The input hardware as the session publishes it.
#[derive(Default)]
pub struct Input {
    /// What the last pass read, so an unchanged snapshot is not published again.
    published: Option<Reading>,
    /// When the snapshot was last read: a change is noticed twice a second, not every frame.
    checked: Option<std::time::Instant>,
}

impl Input {
    /// Mount it as one of the session's services.
    pub fn mount(services: &Rc<RefCell<tilo_service::Services>>) {
        services.borrow_mut().add(Box::new(Self::default()));
    }

    /// The reading now, from the compositor's one snapshot.
    fn now(&self) -> Reading {
        let Some(snapshot) = settings_store::hardware::InputHardwareSnapshot::load() else {
            return Reading::default();
        };
        Reading {
            devices: snapshot
                .devices
                .into_iter()
                .map(|device| InputDevice {
                    key: device.key,
                    name: device.name,
                    kind: kind_name(device.kind),
                    pad_buttons: device.pad_buttons,
                    pad_rings: device.pad_rings,
                    pad_strips: device.pad_strips,
                    pad_mode_buttons: device.pad_mode_buttons,
                    physical_device: device.physical_device,
                })
                .collect(),
            active_keyboard: snapshot.active_keyboard,
        }
    }
}

/// The device kind as the reading names it.
fn kind_name(kind: settings_store::hardware::InputDeviceKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "other".into())
}

impl Service for Input {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<NoEffect>(
            tilo_resource::LiveRow::typed::<InputRoot>(RESOURCE, t("session-input"))
                .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.now()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(ActionAddress::new(resource, action)))
    }

    fn poll(&mut self) -> bool {
        const EVERY: std::time::Duration = std::time::Duration::from_millis(500);
        let at = std::time::Instant::now();
        if self
            .checked
            .is_some_and(|checked| at.duration_since(checked) < EVERY)
        {
            return false;
        }
        self.checked = Some(at);
        let now = self.now();
        if self.published.as_ref() == Some(&now) {
            return false;
        }
        self.published = Some(now);
        true
    }
}

/// The input hardware as the session's own resource.
#[derive(tilo_resource::Row)]
#[row(kind = "session.input", label = "Input devices")]
pub struct InputRoot;
