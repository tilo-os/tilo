//! Battery state + the **threshold monitor** — the pure logic that turns a
//! stream of battery readings into low-battery warnings and a critical
//! auto-suspend, with hysteresis so a battery hovering at the threshold does
//! not spam the user.
//!
//! The readings themselves come from `UPower`'s composite *display device*
//! (see [`super::upower`]); this module never touches the bus. Feeding it
//! Feeding fixture snapshots through the same monitor logic keeps the tests
//! aligned with the service.

use crate::policy::{PowerAction, PowerPolicy};

/// The battery's charge/discharge state, mirroring the `UPower`
/// `org.freedesktop.UPower.Device.State` enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryState {
    Unknown,
    Charging,
    Discharging,
    Empty,
    Full,
    PendingCharge,
    PendingDischarge,
}

impl BatteryState {
    /// Map a raw `UPower` state code (`1..=6`) to a [`BatteryState`].
    /// Anything out of range is [`BatteryState::Unknown`].
    pub fn from_upower(code: u32) -> Self {
        match code {
            1 => BatteryState::Charging,
            2 => BatteryState::Discharging,
            3 => BatteryState::Empty,
            4 => BatteryState::Full,
            5 => BatteryState::PendingCharge,
            6 => BatteryState::PendingDischarge,
            _ => BatteryState::Unknown,
        }
    }

    /// Whether the battery is actively draining. Only a discharging battery
    /// can trip the low / critical thresholds — a charging or full one is
    /// gaining runtime, so any prior warning is stale.
    pub fn is_discharging(self) -> bool {
        matches!(
            self,
            BatteryState::Discharging | BatteryState::PendingDischarge
        )
    }
}

/// A single reading of the machine's composite battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatterySnapshot {
    /// Whether a battery is present at all (a desktop has none).
    pub present: bool,
    /// Charge percentage, `0..=100`.
    pub percent: u8,
    /// Charge/discharge state.
    pub state: BatteryState,
}

impl BatterySnapshot {
    /// Whether the machine runs on its battery: one is there and it is
    /// draining, not charging, full or held by the charger.
    pub fn runs_on_battery(&self) -> bool {
        self.present
            && matches!(
                self.state,
                BatteryState::Discharging | BatteryState::Empty | BatteryState::PendingDischarge
            )
    }

    /// A snapshot for a machine with no battery (always "fine").
    pub fn absent() -> Self {
        Self {
            present: false,
            percent: 100,
            state: BatteryState::Unknown,
        }
    }
}

/// A decision emitted by [`BatteryMonitor::evaluate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryEvent {
    /// Battery crossed the low threshold while discharging — warn the user.
    Low(u8),
    /// Battery crossed the critical threshold — take the critical action
    /// (usually suspend before the machine dies uncleanly).
    Critical(u8),
    /// Battery recovered (plugged in / charging) after a prior warning —
    /// clear the warning UI.
    Recovered,
}

/// Tracks whether the low / critical warnings have already fired so each
/// fires **once** per discharge episode. Plugging in (any non-discharging
/// state) re-arms both. Being a plain latch — no timers — it is driven
/// entirely by the readings handed to [`Self::evaluate`].
#[derive(Debug, Default)]
pub struct BatteryMonitor {
    warned_low: bool,
    warned_critical: bool,
}

impl BatteryMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a low/critical warning is currently latched (for tests and
    /// for the UI to reflect a persistent "battery low" state).
    pub fn is_warning(&self) -> bool {
        self.warned_low || self.warned_critical
    }

    /// Feed one reading and get the events it triggers.
    ///
    /// Contract:
    /// * A battery that is absent, or not discharging, re-arms the latches
    ///   and emits [`BatteryEvent::Recovered`] exactly once if a warning was
    ///   outstanding.
    /// * While discharging, crossing `<= critical` emits
    ///   [`BatteryEvent::Critical`] once (and latches low too, so we do not
    ///   also emit a redundant Low on the same reading).
    /// * Crossing `<= low` (but above critical) emits [`BatteryEvent::Low`]
    ///   once.
    /// * Staying below a threshold that already fired emits nothing.
    pub fn evaluate(&mut self, snap: &BatterySnapshot, policy: &PowerPolicy) -> Vec<BatteryEvent> {
        let mut out = Vec::new();

        // Not draining (or no battery): the charge is safe. Re-arm so the
        // next discharge episode can warn again, and surface a one-shot
        // Recovered so the UI can drop any "low battery" chrome.
        if !snap.present || !snap.state.is_discharging() {
            if self.warned_low || self.warned_critical {
                out.push(BatteryEvent::Recovered);
            }
            self.warned_low = false;
            self.warned_critical = false;
            return out;
        }

        // Discharging: check the more severe threshold first.
        if snap.percent <= policy.critical_battery_percent {
            if !self.warned_critical {
                self.warned_critical = true;
                // Latch low too: at/below critical we are also at/below low,
                // and a single Critical is the right one-shot signal.
                self.warned_low = true;
                out.push(BatteryEvent::Critical(snap.percent));
            }
        } else if snap.percent <= policy.low_battery_percent && !self.warned_low {
            self.warned_low = true;
            out.push(BatteryEvent::Low(snap.percent));
        }

        out
    }
}

/// Convenience: does a [`BatteryEvent::Critical`] mean the machine should
/// suspend, given the policy? Keeps the compositor's mapping in one place.
pub fn critical_leads_to_suspend(policy: &PowerPolicy) -> bool {
    policy.critical_action == PowerAction::Suspend
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PowerPolicy {
        PowerPolicy {
            low_battery_percent: 15,
            critical_battery_percent: 5,
            ..Default::default()
        }
    }

    fn snap(percent: u8, state: BatteryState) -> BatterySnapshot {
        BatterySnapshot {
            present: true,
            percent,
            state,
        }
    }

    #[test]
    fn battery_warnings_fire_once_per_threshold_and_rearm_on_power() {
        {
            assert_eq!(BatteryState::from_upower(1), BatteryState::Charging);
            assert_eq!(BatteryState::from_upower(2), BatteryState::Discharging);
            assert_eq!(BatteryState::from_upower(4), BatteryState::Full);
            assert_eq!(BatteryState::from_upower(99), BatteryState::Unknown);
            assert!(BatteryState::Discharging.is_discharging());
            assert!(!BatteryState::Charging.is_discharging());
            assert!(!BatteryState::Full.is_discharging());
        }
        {
            let mut m = BatteryMonitor::new();
            assert!(
                m.evaluate(&snap(80, BatteryState::Discharging), &policy())
                    .is_empty()
            );
            assert!(!m.is_warning());
        }
        {
            let mut m = BatteryMonitor::new();
            let p = policy();
            assert_eq!(
                m.evaluate(&snap(15, BatteryState::Discharging), &p),
                vec![BatteryEvent::Low(15)]
            );
            // Draining further but still above critical: no repeat warning.
            assert!(
                m.evaluate(&snap(12, BatteryState::Discharging), &p)
                    .is_empty()
            );
            assert!(
                m.evaluate(&snap(8, BatteryState::Discharging), &p)
                    .is_empty()
            );
            assert!(m.is_warning());
        }
        {
            let mut m = BatteryMonitor::new();
            let p = policy();
            // Straight to critical without a prior Low: emit Critical only.
            assert_eq!(
                m.evaluate(&snap(4, BatteryState::Discharging), &p),
                vec![BatteryEvent::Critical(4)]
            );
            // Further drain: silent (already latched).
            assert!(
                m.evaluate(&snap(2, BatteryState::Discharging), &p)
                    .is_empty()
            );
        }
        {
            let mut m = BatteryMonitor::new();
            let p = policy();
            assert_eq!(
                m.evaluate(&snap(14, BatteryState::Discharging), &p),
                vec![BatteryEvent::Low(14)]
            );
            assert_eq!(
                m.evaluate(&snap(5, BatteryState::Discharging), &p),
                vec![BatteryEvent::Critical(5)]
            );
            assert!(
                m.evaluate(&snap(3, BatteryState::Discharging), &p)
                    .is_empty()
            );
        }
        {
            let mut m = BatteryMonitor::new();
            let p = policy();
            let _ = m.evaluate(&snap(10, BatteryState::Discharging), &p); // Low
            assert!(m.is_warning());
            // Plug in: one Recovered, latches cleared.
            assert_eq!(
                m.evaluate(&snap(10, BatteryState::Charging), &p),
                vec![BatteryEvent::Recovered]
            );
            assert!(!m.is_warning());
            // A second charging reading is silent (already recovered).
            assert!(m.evaluate(&snap(11, BatteryState::Charging), &p).is_empty());
            // Unplug and drain again: warns afresh.
            assert_eq!(
                m.evaluate(&snap(12, BatteryState::Discharging), &p),
                vec![BatteryEvent::Low(12)]
            );
        }
        {
            let mut m = BatteryMonitor::new();
            let p = policy();
            assert!(m.evaluate(&BatterySnapshot::absent(), &p).is_empty());
            assert!(!m.is_warning());
        }
        {
            let mut p = policy();
            p.critical_action = PowerAction::Suspend;
            assert!(critical_leads_to_suspend(&p));
            p.critical_action = PowerAction::Ignore;
            assert!(!critical_leads_to_suspend(&p));
        }
    }
}
