//! Battery health and charge limits.
//!
//! Reading the kernel's power-supply class — level, health, cycles, which
//! charge-limit control the hardware exposes — is [`tilo_power_mgmt::sysfs`]'s
//! job, shared with the status indicator, the installer and this crate; the
//! types below are re-exported from there so callers keep one name for them.
//!
//! What lives here is the one part that is not a read: applying a cap. Vendor
//! charge-limit files are not written by the desktop process — their names and
//! semantics differ between drivers, and granting a GUI arbitrary sysfs writes
//! would be a privilege boundary disguised as a setting — so the write goes
//! through the narrowly-scoped `tilo-battery-cap@` service.

pub use tilo_power_mgmt::sysfs::{
    BatteryInfo, CapacityUnit, ChargeCapBackend, ChargeCapCapability, PowerDevice, PowerSupplyKind,
    charge_capability, power_devices,
};

pub use tilo_power_mgmt::{BatteryHistory, BatteryHistoryPoint, load_battery_history};

/// Apply a normalized charge cap through the narrowly-scoped system service.
///
/// # Errors
///
/// Returns an error message if `percent` is outside `50..=100`, or the `tilo-battery-cap@`
/// service is unavailable, not authorised, or fails.
pub fn set_charge_limit(percent: u32) -> Result<(), String> {
    if !(50..=100).contains(&percent) {
        return Err("battery charge cap must be between 50 and 100 percent".into());
    }
    let unit = format!("tilo-battery-cap@{percent}.service");
    crate::process::run("systemctl", &["start", "--wait", &unit])
}

/// Apply and then re-read the hardware contract.  A successful systemd start
/// is not enough: firmware may accept the write while clamping or ignoring it.
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
