//! Who a device is and the revision a fact carries, from the one crate that holds them
//! ([`tilo_fabric_core`]), beside where its screens are — the display facts the actor carries.

use serde::{Deserialize, Serialize};

pub use tilo_fabric_core::{
    DeviceId, FabricStamp, InvalidDeviceId, MAX_FABRIC_ACTION_BYTES, MAX_FABRIC_CHECKPOINT_BYTES,
    looks_like_local_path, valid_identifier,
};

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
