//! What the system resources read.

use serde::{Deserialize, Serialize};

/// A power mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PowerMode {
    Saver,
    Balanced,
    Performance,
}

impl PowerMode {
    /// Every mode, by the names its action takes.
    pub const NAMES: [&'static str; 3] = ["saver", "balanced", "performance"];

    /// The mode named `name` ([`Self::NAMES`]).
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "saver" => Some(Self::Saver),
            "balanced" => Some(Self::Balanced),
            "performance" => Some(Self::Performance),
            _ => None,
        }
    }
}

/// The `power` resource.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Power {
    /// The modes this machine offers; empty without a power daemon.
    pub modes: Vec<PowerMode>,
    pub mode: Option<PowerMode>,
    /// Another person may sign in beside this session (a display manager
    /// that switches seats is running).
    pub user_switching: bool,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

/// The `time-zone` resource.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default, rename_all = "kebab-case")]
pub struct Time {
    /// The zone the clock shows, as tzdata names it.
    pub zone: Option<String>,
    /// The zone follows where the device is.
    pub automatic: bool,
    /// The clock is set from the network; `None` without a time daemon.
    pub network_time: Option<bool>,
    pub synchronized: bool,
    /// Every zone the system names, with where its reference place stands: what a person picks
    /// from and what a map places.
    pub zones: Vec<ZonePlace>,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

/// One IANA zone and the place its clock is kept for: where a map puts it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default, rename_all = "kebab-case")]
pub struct ZonePlace {
    /// The zone, as tzdata names it (`Europe/Amsterdam`).
    pub name: String,
    /// Degrees north of the equator.
    pub latitude: f64,
    /// Degrees east of Greenwich.
    pub longitude: f64,
}

/// The `device:keyboard-backlight` resource.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct KeyboardLight {
    /// The light's level, from 0 to `max`; `None` without a keyboard light.
    pub level: Option<u32>,
    /// Its brightest level.
    pub max: u32,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}
