//! The input devices attached to this machine, published: the keyboards it types with, its
//! pointers and touchpads, and each drawing tablet with the pad beside it. A view names them by
//! the identity their settings are kept under, so a tablet's mapping, buttons and pad are read
//! beside the device they belong to.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use serde_json::{Map, Value};
use settings_store::hardware::{InputDeviceKind, InputDeviceSnapshot, InputHardwareSnapshot};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

pub(crate) const RESOURCE: &str = "session:input-devices";

/// The input devices as the session publishes them: the kind a reader with no words of its own
/// knows them by.
#[derive(tilo_resource::Row)]
#[row(kind = "session.input-devices", label = "Input devices")]
pub(crate) struct InputDevicesRoot;

pub(crate) use tilo_device_surfaces::inputs::{InputDevice, InputDevices, InputKind};

/// The input devices as the session serves them, read from what the compositor writes.
pub(crate) struct Served {
    /// What it last read, and the file's stamp then, so an unchanged file is not read again.
    published: InputDevices,
    stamp: Option<(SystemTime, u64)>,
}

impl Served {
    pub(crate) fn new() -> Self {
        Self {
            published: InputDevices::default(),
            stamp: None,
        }
    }

    /// What the compositor says now, and the file's stamp it was read at.
    fn now(&self) -> (InputDevices, Option<(SystemTime, u64)>) {
        let Some(path) = path() else {
            return (InputDevices::default(), None);
        };
        let stamp = fs::metadata(&path)
            .ok()
            .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
        let Some(snapshot) = InputHardwareSnapshot::load_at(&path) else {
            return (InputDevices::default(), stamp);
        };
        (published(&snapshot), stamp)
    }
}

/// The path the compositor writes its input devices to.
fn path() -> Option<PathBuf> {
    settings_store::hardware::input_snapshot_path()
}

/// One device as a view reads it.
fn device(device: &InputDeviceSnapshot) -> InputDevice {
    InputDevice {
        key: device.key.clone(),
        name: device.name.clone(),
        kind: kind(device.kind),
        pad_buttons: i64::from(device.pad_buttons),
        pad_rings: device
            .pad_rings
            .iter()
            .map(|modes| i64::from(*modes))
            .collect(),
        pad_strips: device
            .pad_strips
            .iter()
            .map(|modes| i64::from(*modes))
            .collect(),
        pad_mode_buttons: device
            .pad_mode_buttons
            .iter()
            .map(|button| i64::from(*button))
            .collect(),
        physical_device: device.physical_device.clone(),
    }
}

/// The kind a view reads.
fn kind(kind: InputDeviceKind) -> InputKind {
    match kind {
        InputDeviceKind::Keyboard => InputKind::Keyboard,
        InputDeviceKind::Mouse => InputKind::Mouse,
        InputDeviceKind::Touchpad => InputKind::Touchpad,
        InputDeviceKind::Touchscreen => InputKind::Touchscreen,
        InputDeviceKind::Tablet => InputKind::Tablet,
        InputDeviceKind::TabletPad => InputKind::TabletPad,
        InputDeviceKind::Other => InputKind::Other,
    }
}

/// The whole reading of `snapshot`.
fn published(snapshot: &InputHardwareSnapshot) -> InputDevices {
    InputDevices {
        devices: snapshot.devices.iter().map(device).collect(),
        active_keyboard: snapshot.active_keyboard.clone(),
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<NoEffect>(
            tilo_resource::LiveRow::typed::<InputDevicesRoot>(
                RESOURCE,
                crate::i18n::t("session-input-devices"),
            )
            .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.published))
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
        let (now, stamp) = self.now();
        // The stamp moving is what a device being plugged in or out writes: an unchanged file is
        // not read again, and a changed one is what a pane redraws for.
        if stamp == self.stamp && stamp.is_some() {
            return false;
        }
        self.stamp = stamp;
        if now == self.published {
            return false;
        }
        self.published = now;
        true
    }
}
