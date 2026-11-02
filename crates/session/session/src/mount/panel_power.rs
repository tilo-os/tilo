//! The panel's battery and what keeps the machine awake, carried between
//! quick settings and the power service.
//!
//! The power service owns the machine's pack and its logind link, and
//! publishes them: `power:battery` is the charge as it reads now, and
//! `power:sleep` is the policy in force with what holds the machine awake, in
//! the holders' words. The session lands those readings in the panel
//! ([`land`]), so the bar's charge glyph, the battery section and the row that
//! says why the screen stays on all read one thing: what the service reads.

use std::cell::RefCell;

use tilo_power_mgmt::{BatterySnapshot, BatteryState, BatteryTime};
use tilo_session_power::{BATTERY, Battery, SLEEP, Sleep};

use crate::Shared;

/// What the panel was last fed of the machine's power, so the service's
/// readings are read every pass but the panel is written only when what it
/// shows moved.
#[derive(Default)]
pub(crate) struct PanelPower {
    fed: RefCell<Option<Reading>>,
}

/// What the panel shows of the machine's power: the pack, how long it lasts
/// or needs, and what keeps the machine awake.
#[derive(Clone, Debug, Default, PartialEq)]
struct Reading {
    battery: Option<BatterySnapshot>,
    time: Option<BatteryTime>,
    holds: Vec<String>,
}

impl Reading {
    /// The reading of what the service publishes, each part only when its
    /// resource is there: a session with no power service of its own leaves
    /// both as they were.
    fn of(battery: Option<&Battery>, sleep: Option<&Sleep>) -> Self {
        Self {
            battery: battery.map(|battery| BatterySnapshot {
                present: battery.present,
                percent: battery.percentage,
                state: battery.state,
            }),
            time: battery.and_then(time_of),
            holds: sleep.map(|sleep| sleep.holds.clone()).unwrap_or_default(),
        }
    }
}

/// How long the battery lasts, or how long it needs: the service publishes the
/// minutes, and whether they count down to empty or up to full is what the
/// pack is doing.
fn time_of(battery: &Battery) -> Option<BatteryTime> {
    let minutes = battery.minutes_left?;
    match battery.state {
        BatteryState::Charging | BatteryState::Full | BatteryState::PendingCharge => {
            Some(BatteryTime::ToFull(minutes))
        }
        BatteryState::Empty
        | BatteryState::Discharging
        | BatteryState::PendingDischarge
        | BatteryState::Unknown => Some(BatteryTime::ToEmpty(minutes)),
    }
}

/// Land the power service's readings in the panel, if this session has a
/// device to show them on. Returns whether the panel shows them now.
pub(crate) fn land(shared: &Shared) -> bool {
    let Some(device) = &shared.device else {
        return false;
    };
    let readings = {
        let services = shared.services.borrow();
        let battery = services
            .read(BATTERY)
            .and_then(|reading| serde_json::from_value::<Battery>(reading).ok());
        let sleep = services
            .read(SLEEP)
            .and_then(|reading| serde_json::from_value::<Sleep>(reading).ok());
        (battery, sleep)
    };
    let (battery, sleep) = readings;
    if battery.is_none() && sleep.is_none() {
        return false;
    }
    let reading = Reading::of(battery.as_ref(), sleep.as_ref());
    if shared.panel_power.fed.borrow().as_ref() == Some(&reading) {
        return true;
    }
    *shared.panel_power.fed.borrow_mut() = Some(reading.clone());
    device.update(|state| {
        if let Some(battery) = reading.battery {
            state.status.battery = Some(battery);
            state.status.battery_time = reading.time;
        }
        state.awake_holders = reading.holds;
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(state: BatteryState, minutes_left: Option<u32>) -> Battery {
        Battery {
            present: true,
            percentage: 62,
            state,
            on_battery: state == BatteryState::Discharging,
            minutes_left,
            charge_limit: None,
            notice: None,
        }
    }

    #[test]
    fn the_charge_is_the_packs_own_reading() {
        let reading = Reading::of(Some(&battery(BatteryState::Discharging, Some(214))), None);
        assert_eq!(
            reading.battery,
            Some(BatterySnapshot {
                present: true,
                percent: 62,
                state: BatteryState::Discharging,
            })
        );
        assert_eq!(reading.time, Some(BatteryTime::ToEmpty(214)));
        assert!(reading.holds.is_empty());
    }

    #[test]
    fn time_to_full_counts_up_while_charging() {
        assert_eq!(
            Reading::of(Some(&battery(BatteryState::Charging, Some(35))), None).time,
            Some(BatteryTime::ToFull(35))
        );
        assert_eq!(
            Reading::of(Some(&battery(BatteryState::Discharging, None)), None).time,
            None,
            "a pack that reports no rate has no time"
        );
    }

    #[test]
    fn what_holds_the_machine_awake_is_named_as_its_holder_said() {
        let sleep = Sleep {
            blank_minutes: Some(10),
            lock_minutes: Some(5),
            suspend_minutes: None,
            lock_on_sleep: true,
            lid_close: tilo_power_mgmt::PowerAction::Suspend,
            power_key: tilo_power_mgmt::PowerAction::Lock,
            held_awake: true,
            holds: vec!["a long download".into()],
            profile: None,
            profiles: Vec::new(),
        };
        let reading = Reading::of(None, Some(&sleep));
        assert_eq!(reading.holds, vec!["a long download".to_owned()]);
        assert_eq!(reading.battery, None, "no battery reading was published");
    }
}
