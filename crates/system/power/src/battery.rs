//! Battery state and the threshold monitor: the pure logic that turns a stream of
//! readings into a low-battery warning and a critical action, with hysteresis so
//! a battery hovering at a threshold does not repeat itself. The readings come
//! from `UPower`'s display device; this module never touches the bus.

use crate::policy::{PowerAction, PowerPolicy};

/// The battery's charge and discharge state, mirroring `UPower`'s `Device.State`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "service", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "service", serde(rename_all = "kebab-case"))]
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
    /// Maps a raw `UPower` state code (`1..=6`) to a [`BatteryState`]; anything out
    /// of range is [`BatteryState::Unknown`].
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

    /// Whether the battery is actively draining. Only a discharging battery can
    /// trip the low and critical thresholds; a charging or full one is gaining
    /// runtime, so any prior warning is stale.
    pub fn is_discharging(self) -> bool {
        matches!(
            self,
            BatteryState::Discharging | BatteryState::PendingDischarge
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatterySnapshot {
    /// Whether a battery is present at all (a desktop has none).
    pub present: bool,
    /// Charge percentage, `0..=100`.
    pub percent: u8,
    pub state: BatteryState,
}

impl BatterySnapshot {
    /// Whether the machine runs on its battery: one is there and it is draining,
    /// not charging, full or held by the charger.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryEvent {
    Low(u8),
    /// The battery crossed the critical threshold: take the critical action,
    /// usually suspending before the machine dies uncleanly.
    Critical(u8),
    /// The battery recovered (plugged in, charging) after a prior warning: clear
    /// the warning.
    Recovered,
}

/// Tracks whether the low and critical warnings have already fired, so each
/// fires once per discharge episode; plugging in re-arms both.
#[derive(Debug, Default)]
pub struct BatteryMonitor {
    warned_low: bool,
    warned_critical: bool,
}

impl BatteryMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a warning is currently latched.
    pub fn is_warning(&self) -> bool {
        self.warned_low || self.warned_critical
    }

    /// Feeds one reading and returns the events it triggers.
    ///
    /// A battery that is absent or not discharging re-arms the latches and emits
    /// [`BatteryEvent::Recovered`] once if a warning was outstanding. While it
    /// discharges, `<= critical` emits [`BatteryEvent::Critical`] once and latches
    /// low too, and `<= low` above it emits [`BatteryEvent::Low`] once.
    pub fn evaluate(&mut self, snap: &BatterySnapshot, policy: &PowerPolicy) -> Vec<BatteryEvent> {
        let mut out = Vec::new();

        // Not draining (or no battery): the charge is safe. Re-arm so the next
        // discharge episode can warn again, and surface a one-shot Recovered so the UI
        // can drop any "battery low" chrome.
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
                // Latch low too: at or below critical we are also at or below low, and a single
                // Critical is the right one-shot signal.
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

/// Whether a [`BatteryEvent::Critical`] means the machine should suspend,
/// given the policy.
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
            let _ = m.evaluate(&snap(10, BatteryState::Discharging), &p);
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
