//! A typed setting: where it lives and how far it travels.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::DeviceClass;

/// The section holding every app's settings that follow the person, one
/// object per app id: an app's typed settings declare where each belongs,
/// and only those that follow the person are written here.
pub const APP_PERSONAL_SECTION: &str = "app_personal";

pub const APP_DEVICE_LOCAL_SECTION: &str = "app_device_local";

/// Where an app's typed settings live and how far they travel. Settings follow a person
/// or a device, never a workspace's members; they are never visible to members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Personal,
    /// Kept on this device and never sent anywhere: a hardware choice, a path, a window
    /// placement.
    DeviceLocal,
    Shared(&'static str),
}

impl Scope {
    /// The document section holding the settings of this scope.
    #[must_use]
    pub const fn section(self) -> &'static str {
        match self {
            Self::Personal => APP_PERSONAL_SECTION,
            Self::DeviceLocal => APP_DEVICE_LOCAL_SECTION,
            Self::Shared(name) => name,
        }
    }
}

/// A typed settings value with a declared [`Scope`].
///
/// Reads never fail: a missing document reads as [`Default`], and a key
/// that is missing or does not decode reads as its default while every
/// other key keeps its stored value. An app's types of one scope share
/// its section, so their field names must not collide.
///
/// ```ignore
/// #[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
/// struct Preferences { confirm_kill: bool }
/// impl SettingsSchema for Preferences {
///     const SCOPE: Scope = Scope::Personal;
/// }
/// let prefs = context.settings::<Preferences>().get();
/// ```
pub trait SettingsSchema: Serialize + DeserializeOwned + Default + 'static {
    const SCOPE: Scope;

    const APP: Option<&'static str> = None;

    /// What a person who changed nothing has on a `device`: [`Default`] everywhere,
    /// unless the setting costs what one class of device should not spend unasked (a copy
    /// kept on the device, work in the background).
    #[must_use]
    fn defaults_on(device: DeviceClass) -> Self {
        let _ = device;
        Self::default()
    }

    /// Whether the setting means anything on a `device`: one whose effect a class of
    /// device cannot have (a copy kept on a device that keeps none) is not offered there,
    /// neither shown nor published.
    #[must_use]
    fn available_on(device: DeviceClass) -> bool {
        let _ = device;
        true
    }

    /// The setting `value` holds, as JSON stores it. When `value` is not one.
    fn from_settings_value(value: Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    /// The setting as JSON stores it; see [`Self::from_settings_value`]. When it does not
    /// serialize.
    fn to_settings_value(&self) -> serde_json::Result<Value> {
        serde_json::to_value(self)
    }
}

/// [`SettingsSchema::from_settings_value`] and [`SettingsSchema::to_settings_value`]
/// compiled where the impl is, never in the crates that read the setting: for a large
/// settings type whose crate is in the shared runtime or an engine, every app or pane
/// reading it otherwise carries its own copy of serde's decoder.
#[macro_export]
macro_rules! settings_codec {
    () => {
        #[inline(never)]
        fn from_settings_value(
            value: $crate::__serde_json::Value,
        ) -> $crate::__serde_json::Result<Self> {
            $crate::__serde_json::from_value(value)
        }

        #[inline(never)]
        fn to_settings_value(&self) -> $crate::__serde_json::Result<$crate::__serde_json::Value> {
            $crate::__serde_json::to_value(self)
        }
    };
}
