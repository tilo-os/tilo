//! The pure power **policy** — the decision layer that maps events (a lid
//! closing, the power key, a critical battery) onto the action the
//! compositor must take. No I/O, no D-Bus, no clock: every decision is a
//! total function of the policy configuration and the event, so the whole
//! layer is exhaustively unit-testable without a bus or a battery.
//!
//! The [`super::service::PowerService`] pairs these decisions with the real
//! `systemd-logind` / `UPower` glue in [`super::logind`] / [`super::upower`];
//! the compositor applies the resulting [`PowerAction`]s.

use std::time::Duration;

/// What the compositor should do in response to a power event.
///
/// The same three outcomes cover the lid switch, the power key, and the
/// critical-battery threshold — each configurable independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    /// Suspend the machine (`logind.Manager.Suspend`).
    Suspend,
    /// Lock the session (the compositor's own lock overlay).
    Lock,
    /// Do nothing — the event is deliberately ignored.
    Ignore,
}

impl PowerAction {
    /// The stable on-disk / on-wire label. The Settings pane writes these
    /// exact strings into `settings.json`; the compositor parses them back.
    pub fn label(self) -> &'static str {
        match self {
            PowerAction::Suspend => "suspend",
            PowerAction::Lock => "lock",
            PowerAction::Ignore => "ignore",
        }
    }

    /// Parse a label written by the Settings pane. An unknown value yields
    /// `None` so the caller can fall back to its own default rather than
    /// silently picking a surprising action.
    pub fn from_label(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "suspend" => Some(PowerAction::Suspend),
            "lock" => Some(PowerAction::Lock),
            "ignore" | "nothing" | "none" => Some(PowerAction::Ignore),
            _ => None,
        }
    }
}

/// The resolved power-management policy the compositor runs against.
///
/// Values originate in the Settings pane (persisted to `settings.json`) and
/// are parsed by [`super::config`]. Every field has a sane default so a
/// missing or corrupt config never leaves the machine unmanaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerPolicy {
    /// Idle duration before the display blanks. `None` means "Never". This
    /// is intentionally independent from idle-suspend: a machine can save
    /// display power without suspending an active background task.
    pub idle_blank: Option<Duration>,
    /// Idle duration before the session suspends. `None` disables
    /// idle-suspend (the display can still blank/lock — that is the idle
    /// state machine's separate concern).
    pub idle_suspend: Option<Duration>,
    /// What to do when the lid closes.
    pub lid_close_action: PowerAction,
    /// What to do when the power key is pressed.
    pub power_key_action: PowerAction,
    /// Warn (a notification) at or below this battery percentage while
    /// discharging.
    pub low_battery_percent: u8,
    /// Take [`Self::critical_action`] at or below this battery percentage
    /// while discharging. Must be `<= low_battery_percent` to be reachable.
    pub critical_battery_percent: u8,
    /// What to do when the battery reaches the critical threshold.
    pub critical_action: PowerAction,
}

impl Default for PowerPolicy {
    fn default() -> Self {
        Self {
            // Match the Settings default. `config::default_policy` only
            // changes the more disruptive idle-suspend behaviour.
            idle_blank: Some(Duration::from_secs(600)),
            // Idle-suspend is opt-in: suspending is more disruptive than
            // blanking, so a fresh machine only suspends if the user (or the
            // Settings default) asks. The Settings default *does* enable it
            // (see `config`), this bare default is the "no config at all"
            // floor.
            idle_suspend: None,
            lid_close_action: PowerAction::Suspend,
            power_key_action: PowerAction::Suspend,
            low_battery_percent: 15,
            critical_battery_percent: 5,
            critical_action: PowerAction::Suspend,
        }
    }
}

impl PowerPolicy {
    /// Resolve the lid-close action. `None` when the lid *opened* (no
    /// action) or the configured action is [`PowerAction::Ignore`].
    pub fn on_lid(&self, closed: bool) -> Option<PowerAction> {
        if !closed {
            return None;
        }
        match self.lid_close_action {
            PowerAction::Ignore => None,
            a => Some(a),
        }
    }

    /// Resolve the power-key action. `None` when configured to ignore.
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
    fn action_labels_round_trip() {
        for a in [PowerAction::Suspend, PowerAction::Lock, PowerAction::Ignore] {
            assert_eq!(PowerAction::from_label(a.label()), Some(a));
        }
        // Case-insensitive and tolerant of synonyms for "ignore".
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

    #[test]
    fn lid_open_is_never_an_action() {
        let p = PowerPolicy::default();
        assert_eq!(p.on_lid(false), None, "opening the lid does nothing");
    }

    #[test]
    fn lid_close_follows_policy() {
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

    #[test]
    fn power_key_follows_policy() {
        let mut p = PowerPolicy {
            power_key_action: PowerAction::Lock,
            ..Default::default()
        };
        assert_eq!(p.on_power_key(), Some(PowerAction::Lock));
        p.power_key_action = PowerAction::Ignore;
        assert_eq!(p.on_power_key(), None);
    }
}
