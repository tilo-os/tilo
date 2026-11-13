//! A screen of one of the person's devices and a placement for it, as the shell-ui interface spells
//! them for an app asking the session to move a screen.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDisplay {
    pub device: String,
    pub device_label: String,
    pub connector: String,
    #[serde(default)]
    pub output_id: Option<String>,
    pub label: String,
    pub online: bool,
    pub local: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u16,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceDisplayMove {
    pub screen: DeviceDisplay,
    pub x: i32,
    pub y: i32,
}
