//! The pure power policy: the decision layer that maps an event (a lid closing,
//! the power key, a critical battery) onto the action the compositor takes. No
//! I/O, no D-Bus, no clock, so the whole layer is testable without a bus or a
//! battery.

use std::time::Duration;

/// What the compositor should do in response to a power event. The same three
/// outcomes cover the lid switch, the power key and the critical-battery
/// threshold, each configurable independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "service", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "service", serde(rename_all = "lowercase"))]
pub enum PowerAction {
    /// Suspend the machine (`logind.Manager.Suspend`).
    Suspend,
    /// Lock the session (the compositor's own lock overlay).
    Lock,
    /// Do nothing — the event is deliberately ignored.
    Ignore,
}

impl PowerAction {
    /// The stable on-disk label. The Settings pane writes these exact strings into
    /// `settings.json`; the compositor parses them back.
    pub fn label(self) -> &'static str {
        match self {
            PowerAction::Suspend => "suspend",
            PowerAction::Lock => "lock",
            PowerAction::Ignore => "ignore",
        }
    }

    /// Parses a label written by the Settings pane; an unknown value yields `None`
    /// so the caller falls back to its own default.
    pub fn from_label(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "suspend" => Some(PowerAction::Suspend),
            "lock" => Some(PowerAction::Lock),
            "ignore" | "nothing" | "none" => Some(PowerAction::Ignore),
            _ => None,
        }
    }
}

/// The resolved power policy the compositor runs against: written by the
/// Settings pane, parsed by [`super::config`], with a sane default for every
/// field so a missing or corrupt config never leaves the machine unmanaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerPolicy {
    /// Idle duration before the display blanks; `None` is "Never". Independent of
    /// idle-suspend, so a machine can save display power without suspending a
    /// background task.
    pub idle_blank: Option<Duration>,
    /// Idle duration before the compositor locks the session; `None` is the
    /// persisted "Never" choice, which does not disable explicit or pre-sleep
    /// locking.
    pub idle_lock: Option<Duration>,
    /// Idle duration before the session suspends; `None` disables idle-suspend (the
    /// display can still blank and lock).
    pub idle_suspend: Option<Duration>,
    /// Whether entering sleep locks the session. `true`, the default, makes every
    /// suspend leave it locked so waking asks for the password; `false` lets an
    /// unlocked machine wake to the work its owner left on screen. A lock that was
    /// already there is kept either way.
    pub lock_on_sleep: bool,
    pub lid_close_action: PowerAction,
    /// What to do when the power key is pressed.
    pub power_key_action: PowerAction,
    /// Warn at or below this battery percentage while discharging.
    pub low_battery_percent: u8,
    /// Take [`Self::critical_action`] at or below this percentage while
    /// discharging; must be `<= low_battery_percent` to be reachable.
    pub critical_battery_percent: u8,
    /// What to do when the battery reaches the critical threshold.
    pub critical_action: PowerAction,
}

impl Default for PowerPolicy {
    fn default() -> Self {
        Self {
            // Match the Settings default. `config::default_policy` only changes the more
            // disruptive idle-suspend behaviour.
            idle_blank: Some(Duration::from_secs(600)),
            idle_lock: Some(Duration::from_secs(300)),
            // Idle-suspend is opt-in: suspending is more disruptive than blanking, so the
            // bare default only suspends if the person asks. The Settings default does
            // enable it (see `config`); this is the "no config at all" floor.
            idle_suspend: None,
            lock_on_sleep: true,
            lid_close_action: PowerAction::Suspend,
            power_key_action: PowerAction::Suspend,
            low_battery_percent: 15,
            critical_battery_percent: 5,
            critical_action: PowerAction::Suspend,
        }
    }
}

/// What this computer owes the rest of the person's devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineDuty {
    /// An always-on host, such as a board: nobody is at it to wake it again, so it
    /// never suspends on its own.
    Host,
    /// An ordinary computer, following its power settings.
    Endpoint,
}

/// Marker the board image ships to select the headless host session.
pub const HOST_IMAGE_MARKER: &str = "/usr/share/tilo/board-headless-session";

/// Classify this machine: the board image is the always-on host, since its image
/// carries [`HOST_IMAGE_MARKER`] and its session runs with `TILO_BOARD_SESSION=1`.
#[must_use]
pub fn this_machine_duty() -> MachineDuty {
    duty_from(
        std::env::var_os("TILO_BOARD_SESSION").is_some_and(|value| value == "1"),
        std::path::Path::new(HOST_IMAGE_MARKER).is_file(),
    )
}

fn duty_from(board_session: bool, host_image: bool) -> MachineDuty {
    if board_session || host_image {
        MachineDuty::Host
    } else {
        MachineDuty::Endpoint
    }
}

impl PowerPolicy {
    /// The policy this machine actually runs. A host drops every automatic suspend
    /// — idle, lid, power key and critical battery — whatever the settings ask for,
    /// while the critical-battery warning still shows. Blanking, locking and an
    /// explicit suspend request are untouched.
    #[must_use]
    pub fn for_duty(self, duty: MachineDuty) -> Self {
        if duty == MachineDuty::Endpoint {
            return self;
        }
        let stay_up = |action| match action {
            PowerAction::Suspend => PowerAction::Ignore,
            other => other,
        };
        Self {
            idle_suspend: None,
            lid_close_action: stay_up(self.lid_close_action),
            power_key_action: stay_up(self.power_key_action),
            critical_action: stay_up(self.critical_action),
            ..self
        }
    }

    /// Resolve the lid-close action; `None` when the lid opened or the configured
    /// action is [`PowerAction::Ignore`].
    pub fn on_lid(&self, closed: bool) -> Option<PowerAction> {
        if !closed {
            return None;
        }
        match self.lid_close_action {
            PowerAction::Ignore => None,
            a => Some(a),
        }
    }

    /// Resolve the power-key action; `None` when configured to ignore.
    pub fn on_power_key(&self) -> Option<PowerAction> {
        match self.power_key_action {
            PowerAction::Ignore => None,
            a => Some(a),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_actions_round_trip_and_lid_and_key_follow_policy() {
        {
            for a in [PowerAction::Suspend, PowerAction::Lock, PowerAction::Ignore] {
                assert_eq!(PowerAction::from_label(a.label()), Some(a));
            }
            assert_eq!(
                PowerAction::from_label("SUSPEND"),
                Some(PowerAction::Suspend)
            );
            assert_eq!(
                PowerAction::from_label("nothing"),
                Some(PowerAction::Ignore)
            );
            assert_eq!(PowerAction::from_label("none"), Some(PowerAction::Ignore));
            assert_eq!(PowerAction::from_label("garbage"), None);
        }
        {
            let p = PowerPolicy::default();
            assert_eq!(p.on_lid(false), None, "opening the lid does nothing");
        }
        {
            let mut p = PowerPolicy {
                lid_close_action: PowerAction::Suspend,
                ..Default::default()
            };
            assert_eq!(p.on_lid(true), Some(PowerAction::Suspend));
            p.lid_close_action = PowerAction::Lock;
            assert_eq!(p.on_lid(true), Some(PowerAction::Lock));
            p.lid_close_action = PowerAction::Ignore;
            assert_eq!(p.on_lid(true), None, "ignore means no action");
        }
        {
            let mut p = PowerPolicy {
                power_key_action: PowerAction::Lock,
                ..Default::default()
            };
            assert_eq!(p.on_power_key(), Some(PowerAction::Lock));
            p.power_key_action = PowerAction::Ignore;
            assert_eq!(p.on_power_key(), None);
        }
    }

    #[test]
    fn a_host_never_suspends_and_an_endpoint_follows_its_settings() {
        {
            assert_eq!(duty_from(true, false), MachineDuty::Host);
            assert_eq!(duty_from(false, true), MachineDuty::Host);
            assert_eq!(duty_from(false, false), MachineDuty::Endpoint);
        }
        let configured = PowerPolicy {
            idle_suspend: Some(Duration::from_secs(900)),
            lid_close_action: PowerAction::Suspend,
            power_key_action: PowerAction::Lock,
            critical_action: PowerAction::Suspend,
            ..PowerPolicy::default()
        };
        {
            assert_eq!(configured.for_duty(MachineDuty::Endpoint), configured);
        }
        {
            let host = configured.for_duty(MachineDuty::Host);
            assert_eq!(host.idle_suspend, None);
            assert_eq!(host.lid_close_action, PowerAction::Ignore);
            assert_eq!(host.critical_action, PowerAction::Ignore);
            assert_eq!(host.power_key_action, PowerAction::Lock);
            assert_eq!(host.idle_blank, configured.idle_blank);
            assert_eq!(host.idle_lock, configured.idle_lock);
            assert_eq!(
                host.critical_battery_percent,
                configured.critical_battery_percent
            );
        }
    }
}
