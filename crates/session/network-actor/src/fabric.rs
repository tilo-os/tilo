//! The words one person's devices say about themselves on their private lane
//! ([`crate::PersonalFabricPayload`]): who a device is, the revision a fact
//! carries, and where its screens are. They are wire types: the actor
//! carries them, and the shell's device fabric decides what they mean.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};

/// Aggregate state is metadata and small clipboard payloads, never media data.
pub const MAX_FABRIC_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FABRIC_ACTION_BYTES: usize = 64 * 1024;
const MAX_DEVICE_ID_BYTES: usize = 128;

/// A stable, human-identity-certified device identifier.
///
/// The network layer will supply this from an authenticated device key. It is
/// kept textual here so the pure model does not depend on a transport crate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

/// A device id that is empty, too long, carries characters outside
/// `[A-Za-z0-9.-_:]` or looks like a local path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("device id is invalid")]
pub struct InvalidDeviceId;

impl DeviceId {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidDeviceId> {
        let value = value.into();
        if !valid_identifier(&value, MAX_DEVICE_ID_BYTES) {
            return Err(InvalidDeviceId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for DeviceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DeviceId::new(self.0.clone())
            .map_err(S::Error::custom)?
            .0
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DeviceId::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A short machine identifier that can never name a local file: ASCII
/// letters, digits and `.-_:`, at most `max` bytes, and nothing shaped like
/// a path.
pub fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !looks_like_local_path(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

/// Whether `value` reads as a local path on any platform.
pub fn looks_like_local_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with("file:")
        || value.starts_with("\\\\")
        || value.as_bytes().get(1) == Some(&b':')
        || value.contains('/')
        || value.contains('\\')
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FabricStamp {
    pub counter: u64,
    pub device: DeviceId,
}

impl FabricStamp {
    pub fn is_newer_than(&self, other: &Self) -> bool {
        (self.counter, self.device.as_str()) > (other.counter, other.device.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl DisplayRect {
    /// Logical geometry shared with the Displays arrangement control.
    /// Malformed or overflowing screen bounds are never crossing targets.
    pub fn logical_placement(self) -> Option<settings_store::display_placement::Placement> {
        let width = i32::try_from(self.width).ok()?;
        let height = i32::try_from(self.height).ok()?;
        if width <= 0 || height <= 0 {
            return None;
        }
        self.x.checked_add(width)?;
        self.y.checked_add(height)?;
        Some(settings_store::display_placement::Placement::new(
            self.x, self.y, width, height,
        ))
    }

    pub fn contains_x(&self, x: i32) -> bool {
        x >= self.x && x < self.x.saturating_add_unsigned(self.width)
    }

    pub fn contains_y(&self, y: i32) -> bool {
        y >= self.y && y < self.y.saturating_add_unsigned(self.height)
    }

    pub fn intersects(&self, other: Self) -> bool {
        self.x < other.x.saturating_add_unsigned(other.width)
            && self.x.saturating_add_unsigned(self.width) > other.x
            && self.y < other.y.saturating_add_unsigned(other.height)
            && self.y.saturating_add_unsigned(self.height) > other.y
    }
}

/// A physical connector in its owner's logical seat coordinate space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayConnectorPlacement {
    pub connector: String,
    pub label: String,
    pub rect: DisplayRect,
    pub scale_milli: u16,
}

/// Rich hardware facts accompanying the connector extension. They describe
/// an output but never grant control or change its live geometry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayOutputObservation {
    pub connector: String,
    pub output: settings_store::person_displays::DeviceOutput,
}
