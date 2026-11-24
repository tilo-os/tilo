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
                "charge_limit": { "type": ["object", "null"], "properties": {
                    "current": { "type": "integer" },
                    "choices": { "type": "array", "items": { "type": "integer" } },
                }, "required": ["current", "choices"] },
            },
            "required": ["present", "percentage", "state", "on_battery"],
        })
    }

    fn field_type(types: &mut tilo_resource::TypeSet) -> tilo_resource::FieldType {
        use tilo_resource::JsonSchema;
        tilo_resource::surface::resource_of(
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
                (
                    "charge_limit",
                    <Option<ChargeLimit> as JsonSchema>::field_type,
                ),
            ],
        )
    }
}

/// The pack's charge limit as a view of another language reads it: where the
/// pack stops charging, and the values the hardware takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargeLimit {
    /// The per cent the pack stops charging at.
    pub current: u32,
    /// The per cents this machine takes.
    pub choices: Vec<u32>,
}

impl From<&ChargeCapCapability> for ChargeLimit {
    fn from(capability: &ChargeCapCapability) -> Self {
        Self {
            current: capability.current,
            choices: capability.choices.clone(),
        }
    }
}

impl tilo_resource::JsonSchema for ChargeLimit {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "current": { "type": "integer" },
                "choices": { "type": "array", "items": { "type": "integer" } },
            },
            "required": ["current", "choices"],
        })
    }

    fn field_type(types: &mut tilo_resource::TypeSet) -> tilo_resource::FieldType {
        use tilo_resource::JsonSchema;
        tilo_resource::surface::resource_of(
            types,
            "tilo_power_service::ChargeLimit",
            "ChargeLimit",
            "Where the pack stops charging, and the values this machine takes.",
            &[
                ("current", <u32 as JsonSchema>::field_type),
                ("choices", <Vec<u32> as JsonSchema>::field_type),
            ],
        )
    }
}

/// One reading of the battery's history: its charge at a moment, and whether
/// the machine was on its charger then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPoint {
    /// The minute it was sampled at, in Unix minutes.
    pub unix_minute: u64,
    /// The charge then, in per cent.
    pub capacity: u32,
    /// Whether the machine was on its charger then.
    pub on_ac: bool,
}

/// The battery's last month of readings, for a chart over the day or the week.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub points: Vec<HistoryPoint>,
}

impl From<tilo_power_mgmt::BatteryHistory> for History {
    fn from(history: tilo_power_mgmt::BatteryHistory) -> Self {
        Self {
            points: history
                .points
                .into_iter()
                .map(|point| HistoryPoint {
                    unix_minute: point.unix_minute,
                    capacity: point.capacity,
                    on_ac: point.on_ac,
                })
                .collect(),
        }
    }
}

impl tilo_resource::JsonSchema for History {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "points": { "type": "array", "items": { "type": "object", "properties": {
                    "unix_minute": { "type": "integer", "minimum": 0 },
                    "capacity": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "on_ac": { "type": "boolean" },
                }, "required": ["unix_minute", "capacity", "on_ac"] } },
            },
            "required": ["points"],
        })
    }

    fn field_type(types: &mut tilo_resource::TypeSet) -> tilo_resource::FieldType {
        use tilo_resource::JsonSchema;
        tilo_resource::surface::resource_of(
            types,
            "tilo_power_service::History",
            "BatteryHistory",
            "The battery's readings over the last month.",
            &[("points", <Vec<HistoryPoint> as JsonSchema>::field_type)],
        )
    }
}

impl tilo_resource::JsonSchema for HistoryPoint {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "unix_minute": { "type": "integer", "minimum": 0 },
                "capacity": { "type": "integer", "minimum": 0, "maximum": 100 },
                "on_ac": { "type": "boolean" },
            },
            "required": ["unix_minute", "capacity", "on_ac"],
        })
    }

    fn field_type(types: &mut tilo_resource::TypeSet) -> tilo_resource::FieldType {
        use tilo_resource::JsonSchema;
        tilo_resource::surface::resource_of(
            types,
            "tilo_power_service::HistoryPoint",
            "HistoryPoint",
            "The battery's charge at one moment.",
            &[
                ("unix_minute", <u64 as JsonSchema>::field_type),
                ("capacity", <u32 as JsonSchema>::field_type),
                ("on_ac", <bool as JsonSchema>::field_type),
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
}
