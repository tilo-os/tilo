//! The system resources' actions. Every one acts on this device only.

use tilo_resource::Effect;

use crate::model::PowerMode;

/// The `power` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.system.power")]
pub enum PowerAction {
    /// Choose the power mode.
    #[effect(
        own_device,
        idempotent,
        label = "Power mode",
        icon = "gauge",
        keywords = "power saver, balanced, performance, battery saver, energiesparmodus"
    )]
    SetPowerMode {
        #[param("The power mode", choices = PowerMode::NAMES)]
        mode: String,
    },
    /// Lock the screen: unlocking asks for the password.
    #[effect(
        own_device,
        idempotent,
        icon = "lock",
        keywords = "lock screen, lock the screen, sperren, bildschirm sperren"
    )]
    Lock,
    /// Leave this session running and let someone else sign in.
    #[effect(
        own_device,
        icon = "users",
        keywords = "switch user, change user, other user, benutzer wechseln"
    )]
    SwitchUser,
    /// Put the device to sleep.
    #[effect(
        own_device,
        icon = "moon",
        keywords = "sleep, suspend, standby, ruhezustand"
    )]
    Suspend,
    /// Restart the device.
    #[effect(
        own_device,
        destructive,
        icon = "rotate-ccw",
        keywords = "reboot, neustart"
    )]
    Restart,
    /// Turn the device off.
    #[effect(
        own_device,
        destructive,
        icon = "power",
        keywords = "shut down, power off, ausschalten"
    )]
    PowerOff,
    /// Log out of this session.
    #[effect(
        own_device,
        destructive,
        icon = "log-out",
        keywords = "sign out, abmelden"
    )]
    LogOut,
}

/// The `system-update` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.system.update")]
pub enum UpdateAction {
    /// Fetch and apply the newest system now.
    #[effect(
        own_device,
        icon = "download",
        keywords = "update, upgrade, aktualisieren"
    )]
    UpdateNow,
    /// Restart the device to finish the system update.
    #[effect(
        own_device,
        destructive,
        icon = "rotate-ccw",
        keywords = "restart to update, reboot"
    )]
    RestartToUpdate,
}

/// The `time-zone` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.system.time")]
pub enum TimeAction {
    /// Set the time zone; it no longer follows where the device is.
    #[effect(
        own_device,
        idempotent,
        icon = "globe",
        keywords = "time zone, zeitzone"
    )]
    SetZone {
        #[param("The zone, as tzdata names it (such as `Europe/Amsterdam`)")]
        zone: String,
    },
    /// Let the time zone follow where the device is, or stop.
    #[effect(
        own_device,
        idempotent,
        icon = "map-pin",
        keywords = "automatic time zone, travel"
    )]
    SetAutomaticZone {
        #[param("Whether the zone follows the device")]
        on: bool,
    },
    /// Set the clock from the network, or stop.
    #[effect(own_device, idempotent, icon = "clock", keywords = "ntp, network time")]
    SetNetworkTime {
        #[param("Whether the clock is set from the network")]
        on: bool,
    },
}
