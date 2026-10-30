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

/// The `device:keyboard-backlight` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.system.keyboard-backlight")]
pub enum KeyboardLightAction {
    /// Light the keyboard a step brighter.
    #[effect(
        own_device,
        label = "Brighter keyboard",
        icon = "sun",
        keywords = "keyboard light, backlight, tastaturbeleuchtung"
    )]
    Brighter,
    /// Light the keyboard a step dimmer.
    #[effect(
        own_device,
        label = "Dimmer keyboard",
        icon = "sun-dim",
        keywords = "keyboard light, backlight"
    )]
    Dimmer,
    /// Turn the keyboard light off when lit, and full on when dark.
    #[effect(
        own_device,
        label = "Keyboard light on or off",
        icon = "keyboard",
        keywords = "keyboard light, backlight"
    )]
    Toggle,
    /// Set the keyboard light's level.
    #[effect(
        own_device,
        idempotent,
        label = "Keyboard light",
        icon = "keyboard",
        keywords = "keyboard light, backlight"
    )]
    SetLevel {
        #[param("The level from 0 (off) to 1 (brightest)")]
        level: f64,
    },
}
