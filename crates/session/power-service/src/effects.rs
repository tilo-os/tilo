//! The `power:sleep` and `power:battery` resources' actions. Every one acts
//! on this device only.

use tilo_resource::Effect;

/// The `power:battery` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.power.battery")]
pub enum BatteryAction {
    /// Stop charging at `percent`, where the pack's driver offers a limit.
    #[effect(
        own_device,
        idempotent,
        label = "Set the charge limit",
        icon = "battery",
        keywords = "charge limit, charge cap, charge threshold, battery health, ladelimit"
    )]
    SetChargeLimit {
        #[param("The charge to stop at, in percent")]
        percent: u32,
    },
}

/// The `power:sleep` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.power.sleep")]
pub enum SleepAction {
    /// Stop the machine from sleeping on its own, until released.
    #[effect(
        own_device,
        label = "Keep awake",
        icon = "sun",
        keywords = "keep awake, inhibit suspend, stay awake, wachhalten"
    )]
    KeepAwake {
        #[param("Why the machine must stay awake")]
        reason: String,
    },
    /// Let the machine sleep on its own again.
    #[effect(
        own_device,
        idempotent,
        label = "Let it sleep",
        icon = "moon",
        keywords = "release, allow suspend, sleep again"
    )]
    Release {
        #[param("The ticket `keep-awake` returned")]
        ticket: String,
    },
    /// What pressing the power key does, as the policy resolves it.
    #[effect(
        own_device,
        icon = "power",
        keywords = "power key, power button, sleep, lock"
    )]
    PowerKey,
    /// What opening or closing the lid does, as the policy resolves it.
    #[effect(own_device, icon = "laptop", keywords = "lid, close the lid, laptop")]
    Lid {
        #[param("Whether the lid is closed")]
        closed: bool,
    },
}
