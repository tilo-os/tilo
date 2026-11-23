//! The `reboot` and `poweroff` steps. Both return before the machine goes down, so the caller
//! hears the answer: systemd is asked with `--no-block`.

use std::process::Command;

use tilo_device_protocol::image::{Poweroff, PoweroffResult, Reboot, RebootResult};

use crate::daemon::Daemon;
use crate::error::DaemonError;

fn systemctl(verb: &str) -> Result<(), DaemonError> {
    let status = Command::new("systemctl")
        .args(["--no-block", verb])
        .status()
        .map_err(DaemonError::io(format!("run systemctl {verb}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(DaemonError::Apply {
            step: format!("systemctl {verb}"),
            detail: status.to_string(),
        })
    }
}

/// Restart the device, with the ESP left writable for the deployment bootc applies at shutdown.
///
/// # Errors
/// systemd refused the restart.
pub fn reboot(_: &Reboot) -> Result<RebootResult, DaemonError> {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(DaemonError::io("read the boot id"))?
        .trim()
        .to_owned();
    let writable = Command::new("sh")
        .args(["-c", crate::image_upgrade::ESP_WRITABLE_UNTIL_BOOT])
        .status()
        .map_err(DaemonError::io("make /boot writable"))?;
    if !writable.success() {
        return Err(DaemonError::BootLoader {
            detail: "/boot could not be made writable for the deployment bootc applies at shutdown"
                .into(),
        });
    }
    systemctl("reboot")?;
    Ok(RebootResult { boot_id })
}

/// Power the device off once no swap, gc or upgrade holds the VM lock. The lock is held until the
/// process ends, so none starts after the check.
///
/// # Errors
/// [`DaemonError::Busy`] while VM-wide work runs, or systemd refused.
pub fn poweroff(daemon: &Daemon, _: &Poweroff) -> Result<PoweroffResult, DaemonError> {
    let held = daemon.locks.vm_exclusive()?;
    systemctl("poweroff")?;
    std::mem::forget(held);
    Ok(PoweroffResult {})
}
