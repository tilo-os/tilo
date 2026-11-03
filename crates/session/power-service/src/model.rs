//! What the service publishes: the battery as it reads and the policy in
//! force.

use serde::{Deserialize, Serialize};

pub use tilo_power_mgmt::sysfs::ChargeCapCapability;
use tilo_power_mgmt::{BatteryState, PowerAction};

/// A low or critical battery notice, in the words a banner shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub summary: String,
    pub body: String,
    /// `true` for the critical-battery notice.
    pub critical: bool,
}

/// The machine's battery as it reads now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Battery {
    /// Whether this machine has a battery at all (a desktop has none).
    pub present: bool,
    /// Charge percentage, `0..=100`.
    pub percentage: u8,
    /// Whether it charges, drains, or is held by the charger.
    pub state: BatteryState,
    /// Whether the machine runs on the battery right now.
    pub on_battery: bool,
    /// Minutes until the battery is empty, or until it is full; absent
    /// while the rate is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes_left: Option<u32>,
    /// The charge limit the pack's driver offers, its current setting and
    /// the values it takes; absent where the hardware offers none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_limit: Option<ChargeCapCapability>,

    /// The notice no surface has shown yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<Notice>,
}

/// The battery's reading as a view of another language reads it: the pack's charge, its
/// state and the time left. The charge limit is the settings page's, which
/// reads it through the service's own types.
impl tilo_resource::JsonSchema for Battery {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "present": { "type": "boolean" },
                "percentage": { "type": "integer", "minimum": 0, "maximum": 100 },
                "state": { "type": "string" },
                "on_battery": { "type": "boolean" },
                "minutes_left": { "type": ["integer", "null"], "minimum": 0 },
            },
            "required": ["present", "percentage", "state", "on_battery"],
        })
    }

    fn field_type(types: &mut tilo_resource::TypeSet) -> tilo_resource::FieldType {
        use tilo_resource::JsonSchema;
        tilo_resource::surface::record_of(
            types,
            "tilo_power_service::Battery",
            "Battery",
            "The machine's battery as it reads now.",
            &[
                ("present", <bool as JsonSchema>::field_type),
                ("percentage", <u8 as JsonSchema>::field_type),
                ("state", <String as JsonSchema>::field_type),
                ("on_battery", <bool as JsonSchema>::field_type),
                ("minutes_left", <Option<u32> as JsonSchema>::field_type),
            ],
        )
    }
}

/// The policy in force, and what keeps the machine awake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sleep {
    /// Idle minutes before the displays blank; absent for "Never".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blank_minutes: Option<u64>,
    /// Idle minutes before the session locks; absent for "Never".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_minutes: Option<u64>,
    /// Idle minutes before the machine sleeps; absent for "Never".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_minutes: Option<u64>,
    /// Whether a sleep locks the session.
    pub lock_on_sleep: bool,
    /// What closing the lid does.
    pub lid_close: PowerAction,
    /// What the power key does.
    pub power_key: PowerAction,
    /// Whether anything holds the machine awake.
    pub held_awake: bool,
    /// Why, in the holders' words.
    pub holds: Vec<String>,
    /// The power profile in force, by the name this machine gives it; absent
    /// where it offers none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The profiles this machine offers, in its own order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
}
