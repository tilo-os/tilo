//! The input devices attached to this machine, published: the keyboards it types with, its
//! pointers and touchpads, and each drawing tablet with the pad beside it. A view names them by
//! the identity their settings are kept under, so a tablet's mapping, buttons and pad are read
//! beside the device they belong to.

use std::path::Path;
use std::sync::Arc;

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

/// The input devices the compositor writes, kept current by a worker: what serves them and what
/// names the keyboard last typed on read the same value, and neither a pass nor a chord opens the
/// file.
#[derive(Clone)]
pub(crate) struct Inputs(Arc<Held>);

struct Held {
    /// The file as a worker reads it, once a watch could be armed.
    watched: Option<Watched<InputDevices>>,
    /// What was read once at startup when it could not be watched.
    kept: Arc<InputDevices>,
}

impl Inputs {
    /// Watch the runtime directory the compositor writes its input devices in, and read them on
    /// a worker; without a watch, read them once here, before the loop.
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
        let kept = if watched.is_some() {
            Arc::new(InputDevices::default())
        } else {
            Arc::new(read_now(snapshot.as_deref()))
        };
        Self(Arc::new(Held { watched, kept }))
    }

    /// The devices as last read: the worker's latest, or the startup read.
    pub(crate) fn get(&self) -> Arc<InputDevices> {
        self.0
            .watched
            .as_ref()
            .map_or_else(|| Arc::clone(&self.0.kept), Watched::get)
    }

    /// The list the keyboard last typed on types in, when that keyboard is one of those present:
    /// its own, else the shared one.
    pub(crate) fn scope(&self) -> String {
        let devices = self.get();
        settings_store::keyboard_sources::scope_of(
            devices.active_keyboard.as_deref(),
            devices
                .devices
                .iter()
                .filter(|device| device.kind == InputKind::Keyboard)
                .map(|device| device.key.as_str()),
        )
    }
}

/// The input devices as the session serves them: empty until the worker's first read lands, and a
/// view that names one hears of it when it does.
pub(crate) struct Served {
    /// What it last served.
    published: InputDevices,
    /// What the compositor published, as the worker read it.
    inputs: Inputs,
}

impl Served {
    pub(crate) fn new(inputs: Inputs) -> Self {
        Self {
            published: InputDevices::default(),
            inputs,
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
        let now = self.inputs.get();
        // The file's worker reads it again when the compositor replaces it; a value it kept that
        // is the same as the served one changes nothing for a view.
        if *now == self.published {
            return false;
        }
        self.published = (*now).clone();
        true
    }
}
