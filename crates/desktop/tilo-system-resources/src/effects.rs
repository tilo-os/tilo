//! The system resources' actions. Every one acts on this device only.

use tilo_resource::Effect;

use crate::model::PowerMode;

/// The `power` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.system.power")]
pub enum PowerAction {
    /// Choose the power mode.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "gauge",
        keywords = "power saver, balanced, performance, battery saver, energiesparmodus"
    )]
    SetPowerMode {
        #[param("The power mode", choices = PowerMode::NAMES)]
        mode: String,
    },
    /// Lock the screen: unlocking asks for the password.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "lock",
        keywords = "lock screen, lock the screen, sperren, bildschirm sperren"
    )]
    Lock,
    /// Leave this session running and let someone else sign in.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "users",
        keywords = "switch user, change user, other user, benutzer wechseln"
    )]
    SwitchUser,
    /// Put the device to sleep.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "moon",
        keywords = "sleep, suspend, standby, ruhezustand"
    )]
    Suspend,
    /// Restart the device.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "rotate-ccw",
        keywords = "reboot, neustart"
    )]
    Restart,
    /// Turn the device off.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "power",
        keywords = "shut down, power off, ausschalten"
    )]
    PowerOff,
    /// Log out of this session.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "download",
        keywords = "update, upgrade, aktualisieren"
    )]
    UpdateNow,
    /// Restart the desktop onto the new interface to finish the system
    /// update, with the windows it has open reopened.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "restart desktop, finish update, desktop neu starten"
    )]
    RestartDesktop,
    /// Restart the device to finish the system update.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "rotate-ccw",
        keywords = "restart to update, reboot"
    )]
    RestartToUpdate,
    /// Go back to the system this machine ran before, and restart into it.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "history",
        keywords = "roll back, previous system, go back, zurücksetzen"
    )]
    Rollback,
    /// Go back to the system this machine ran before, and restart into it when the person is
    /// ready.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "history",
        keywords = "roll back later, previous system, restart later"
    )]
    RollbackLater,
    /// Choose when this machine updates by itself: the image's own schedule, daily or weekly.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "clock",
        keywords = "update schedule, daily, weekly, automatisch aktualisieren"
    )]
    SetSchedule {
        #[param("system, daily or weekly")]
        schedule: String,
    },
    /// Say whether an automatic update may download over a metered connection.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "bar-chart",
        keywords = "metered, metered connection, data limit"
    )]
    SetAllowMetered {
        #[param("Whether it may download over a metered connection")]
        allow: bool,
    },
}

/// The `time-zone` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.system.time")]
pub enum TimeAction {
    /// Set the time zone; it no longer follows where the device is.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "sun",
        keywords = "keyboard light, backlight, tastaturbeleuchtung"
    )]
    Brighter,
    /// Light the keyboard a step dimmer.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "sun-dim", keywords = "keyboard light, backlight")]
    Dimmer,
    /// Turn the keyboard light off when lit, and full on when dark.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "keyboard", keywords = "keyboard light, backlight")]
    Toggle,
    /// Set the keyboard light's level.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "keyboard",
        keywords = "keyboard light, backlight"
    )]
    SetLevel {
        #[param("The level from 0 (off) to 1 (brightest)")]
        level: f64,
    },
}
