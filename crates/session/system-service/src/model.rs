//! What the system resources read.

use serde::{Deserialize, Serialize};

/// A power mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl From<os_actions::PowerProfile> for PowerMode {
    fn from(profile: os_actions::PowerProfile) -> Self {
        match profile {
            os_actions::PowerProfile::Saver => Self::Saver,
            os_actions::PowerProfile::Balanced => Self::Balanced,
            os_actions::PowerProfile::Performance => Self::Performance,
        }
    }
}

impl From<PowerMode> for os_actions::PowerProfile {
    fn from(mode: PowerMode) -> Self {
        match mode {
            PowerMode::Saver => Self::Saver,
            PowerMode::Balanced => Self::Balanced,
            PowerMode::Performance => Self::Performance,
        }
    }
}

/// The `power` resource.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Time {
    /// The zone the clock shows, as tzdata names it.
    pub zone: Option<String>,
    /// The zone follows where the device is.
    pub automatic: bool,
    /// The clock is set from the network; `None` without a time daemon.
    pub network_time: Option<bool>,
    pub synchronized: bool,
    pub pending: Vec<String>,
    pub problem: Option<String>,
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

impl KeyboardLight {
    /// The light as the kernel reads it.
    #[must_use]
    pub fn of(light: os_actions::KeyboardBacklight) -> Self {
        Self {
            level: Some(light.level),
            max: light.max,
            ..Self::default()
        }
    }

    /// The light's own reading, when there is a light.
    #[must_use]
    pub fn light(&self) -> Option<os_actions::KeyboardBacklight> {
        self.level.map(|level| os_actions::KeyboardBacklight {
            level,
            max: self.max,
        })
    }
}
