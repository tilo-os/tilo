//! Battery health and charge limits, read from the kernel's power-supply class: level,
//! health, cycles, and which charge-limit control the hardware has.

pub use tilo_power_mgmt::sysfs::{
    BatteryInfo, CapacityUnit, ChargeCapBackend, ChargeCapCapability, PowerDevice, PowerSupplyKind,
    charge_capability, power_devices,
};

pub use tilo_power_mgmt::{BatteryHistory, BatteryHistoryPoint, load_battery_history};

pub fn set_charge_limit(percent: u32) -> Result<(), String> {
    if !(50..=100).contains(&percent) {
        return Err("battery charge cap must be between 50 and 100 percent".into());
    }
    let unit = format!("tilo-battery-cap@{percent}.service");
    crate::process::run("systemctl", &["start", "--wait", &unit])
}

/// Applies and then re-reads the hardware contract: a successful systemd start is not enough, since
/// firmware may accept the write and clamp.
pub fn set_charge_limit_checked(percent: u32) -> Result<ChargeCapCapability, String> {
    set_charge_limit(percent)?;
    let actual =
        charge_capability().ok_or("charge-cap control disappeared after applying the change")?;
    if actual.current != percent {
        return Err(format!(
            "firmware kept the charge cap at {}% instead of accepting {percent}%",
            actual.current
        ));
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_charge_cap_outside_the_hardware_range_is_refused_before_any_write() {
        assert!(set_charge_limit(49).is_err());
        assert!(set_charge_limit(101).is_err());
    }
}
