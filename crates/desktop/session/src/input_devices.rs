//! The input devices attached to this machine, published: the keyboards it types with, its
//! pointers and touchpads, and each drawing tablet with the pad beside it. A view names them by
//! the identity their settings are kept under, so a tablet's mapping, buttons and pad are read
//! beside the device they belong to.

use std::path::Path;

use serde_json::{Map, Value};
use settings_store::hardware::{InputDeviceKind, InputDeviceSnapshot, InputHardwareSnapshot};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_files::native::Watched;
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

pub(crate) const RESOURCE: &str = "session:input-devices";

/// The input devices as the session publishes them: the kind a reader with no words of its own
/// knows them by.
#[derive(tilo_resource::Row)]
#[row(kind = "session.input-devices", label = "Input devices")]
pub(crate) struct InputDevicesRoot;

pub(crate) use tilo_device_surfaces::inputs::{InputDevice, InputDevices, InputKind};

/// The input devices as the session serves them, read from what the compositor writes: on a
/// worker, so no pass of the loop stats or opens the file. Until the first read lands the
/// devices are empty, and a view that names one hears of it when it does.
pub(crate) struct Served {
    /// What it last served.
    published: InputDevices,
    /// The file as a worker reads it. `None` when the runtime directory cannot be watched: then
    /// the devices were read once at startup, before the loop, and no pass touches the disk.
    devices: Option<Watched<InputDevices>>,
}

impl Served {
    pub(crate) fn new() -> Self {
        let snapshot = settings_store::hardware::input_snapshot_path();
        let watched = snapshot
            .as_deref()
            .and_then(Path::parent)
            .and_then(|directory| {
                let read = snapshot.clone();
                match Watched::start_unread(
                    "tilo-input-devices",
                    vec![directory.to_path_buf()],
                    InputDevices::default(),
                    move || read_now(read.as_deref()),
                ) {
                    Ok(watched) => Some(watched),
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            "the input devices do not follow the runtime directory"
                        );
                        None
                    }
                }
            });
        let published = if watched.is_some() {
            InputDevices::default()
        } else {
            read_now(snapshot.as_deref())
        };
        Self {
            published,
            devices: watched,
        }
    }
}

/// The devices `path` names, as the compositor last wrote them.
fn read_now(path: Option<&Path>) -> InputDevices {
    path.and_then(InputHardwareSnapshot::load_at)
        .as_ref()
        .map_or_else(InputDevices::default, published)
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
        let Some(devices) = &self.devices else {
            return false;
        };
        let now = devices.get();
        // The file's worker reads it again when the compositor replaces it; a value it kept that
        // is the same as the served one changes nothing for a view.
        if *now == self.published {
            return false;
        }
        self.published = (*now).clone();
        true
    }
}
