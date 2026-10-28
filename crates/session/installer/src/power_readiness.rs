//! Installation power-safety policy.
//!
//! Starting a partitioning and encrypted-filesystem operation with an almost
//! empty, actively discharging battery is avoidable risk.  This module keeps
//! the decision narrow: desktops, VMs, unreadable firmware, and
//! a charging laptop all proceed.  Only a definitive low-and-discharging
//! reading asks the person to connect power before destructive work begins.

use tilo_power_mgmt::{BatterySnapshot, BatteryState};

/// Enough headroom for a normal offline install without making a healthy
/// laptop wait for an arbitrary full charge.
pub const MIN_INSTALL_BATTERY_PERCENT: u8 = 25;

/// Return the low charge level that must be remedied before installation.
///
/// `on_battery` is kept separate from the snapshot because the kernel's AC
/// online bit changes sooner than some batteries update their status.  Both
/// signals must say the machine is draining, which avoids blocking USB-C
/// laptops whose firmware exposes no conventional `Mains` supply node.
pub fn insufficient_battery_percent(snapshot: BatterySnapshot, on_battery: bool) -> Option<u8> {
    let draining = matches!(
        snapshot.state,
        BatteryState::Discharging | BatteryState::PendingDischarge | BatteryState::Empty
    );
    (snapshot.present && on_battery && draining && snapshot.percent < MIN_INSTALL_BATTERY_PERCENT)
        .then_some(snapshot.percent)
}

/// Read the live machine through Tilo's shared, bus-free sysfs model.
pub fn current_insufficient_battery_percent() -> Option<u8> {
    insufficient_battery_percent(
        tilo_power_mgmt::sysfs_battery_snapshot(),
        tilo_power_mgmt::sysfs_on_battery(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(percent: u8, state: BatteryState) -> BatterySnapshot {
        BatterySnapshot {
            present: true,
            percent,
            state,
        }
    }

    #[test]
    fn power_readiness_blocks_only_a_low_discharging_battery() {
        {
            assert_eq!(
                insufficient_battery_percent(
                    battery(MIN_INSTALL_BATTERY_PERCENT - 1, BatteryState::Discharging),
                    true,
                ),
                Some(MIN_INSTALL_BATTERY_PERCENT - 1)
            );
            assert_eq!(
                insufficient_battery_percent(battery(0, BatteryState::Empty), true),
                Some(0)
            );
        }
        {
            assert_eq!(
                insufficient_battery_percent(
                    battery(MIN_INSTALL_BATTERY_PERCENT, BatteryState::Discharging),
                    true,
                ),
                None
            );
            assert_eq!(
                insufficient_battery_percent(battery(3, BatteryState::Charging), true),
                None
            );
            assert_eq!(
                insufficient_battery_percent(battery(3, BatteryState::Discharging), false),
                None
            );
        }
        {
            assert_eq!(
                insufficient_battery_percent(BatterySnapshot::absent(), true),
                None
            );
            assert_eq!(
                insufficient_battery_percent(battery(3, BatteryState::Unknown), true),
                None
            );
        }
    }
}
