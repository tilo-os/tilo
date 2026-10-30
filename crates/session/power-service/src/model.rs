//! What the service publishes: the battery as it reads, the policy in
//! force, and the notice a surface has not shown yet.

use serde::Serialize;

/// A low or critical battery notice, in the words a banner shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub summary: String,
    pub body: String,
    /// `true` for the critical-battery notice.
    pub critical: bool,
}

/// The machine's battery as it reads now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Battery {
    /// Whether this machine has a battery at all (a desktop has none).
    pub present: bool,
    /// Charge percentage, `0..=100`.
    pub percentage: u8,
    /// `charging`, `discharging`, `empty`, `full`, `pending-charge`,
    /// `pending-discharge` or `unknown`.
    pub state: &'static str,
    /// Whether the machine runs on the battery right now.
    pub on_battery: bool,
    /// Minutes until the battery is empty, or until it is full; absent
    /// while the rate is not known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes_left: Option<u32>,
    /// The notice no surface has shown yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<Notice>,
}

/// The policy in force, and what keeps the machine awake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sleep {
    /// Idle minutes before the displays blank; absent for "Never".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blank_minutes: Option<u64>,
    /// Idle minutes before the session locks; absent for "Never".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_minutes: Option<u64>,
    /// Idle minutes before the machine sleeps; absent for "Never".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspend_minutes: Option<u64>,
    /// Whether a sleep locks the session.
    pub lock_on_sleep: bool,
    /// What closing the lid does: `suspend`, `lock` or `ignore`.
    pub lid_close: &'static str,
    /// What the power key does.
    pub power_key: &'static str,
    /// Whether anything holds the machine awake.
    pub held_awake: bool,
    /// Why, in the holders' words.
    pub holds: Vec<String>,
}
