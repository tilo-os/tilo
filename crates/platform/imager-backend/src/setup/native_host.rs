//! Native host identity, folder boundaries and explicit setup remedies.
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    Linux,
    Windows,
    Other,
}

pub fn host() -> Host {
    if cfg!(target_os = "linux") {
        Host::Linux
    } else if cfg!(windows) {
        Host::Windows
    } else {
        Host::Other
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCommand {
    Restart,
    FirmwareSettings { suspend_bitlocker: bool },
    DisableFastStartup,
}

#[cfg(target_os = "linux")]
fn run_checked(program: &str, args: &[&str]) -> Result<(), String> {
    const OUTPUT_LIMIT: usize = 64 * 1024;
    // polkit may wait for a person to authorise the restart.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
    let output = bounded_command::run(program, args, None, OUTPUT_LIMIT, TIMEOUT)
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{program} exited with {}: {}",
            output.status,
            stderr.trim()
        ))
    }
}

/// Unverified on real hardware.
pub fn run(command: HostCommand) -> Result<(), String> {
    #[cfg(windows)]
    {
        // Each call raises its own UAC prompt; the firmware restart
        // suspends BitLocker for one restart when asked.
        use crate::staging::windows::remedies;
        match command {
            HostCommand::Restart => remedies::restart_now(),
            HostCommand::FirmwareSettings { suspend_bitlocker } => {
                remedies::restart_to_firmware_settings(suspend_bitlocker)
            }
            HostCommand::DisableFastStartup => remedies::disable_fast_startup(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        match command {
            HostCommand::Restart => run_checked("systemctl", &["reboot"]),
            HostCommand::FirmwareSettings { .. } => {
                run_checked("systemctl", &["reboot", "--firmware-setup"])
            }
            HostCommand::DisableFastStartup => {
                Err("This host does not support the requested setup remedy.".into())
            }
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = command;
        Err("This host does not support the requested setup remedy.".into())
    }
}
pub fn system_folder_reason(path: &Path) -> Option<&'static str> {
    if path.parent().is_none() {
        return Some("filesystem root");
    }

    #[cfg(unix)]
    {
        const SYSTEM_ROOTS: [&str; 13] = [
            "/bin", "/boot", "/dev", "/etc", "/lib", "/private", "/proc", "/root", "/run", "/sbin",
            "/System", "/sys", "/usr",
        ];
        let is_protected_var_path = (path == Path::new("/var") || path.starts_with("/var"))
            && !path.starts_with("/var/home");
        if SYSTEM_ROOTS
            .into_iter()
            .any(|root| path == Path::new(root) || path.starts_with(root))
            || is_protected_var_path
            || path == Path::new("/Library")
            || path.starts_with("/Library")
        {
            return Some("operating-system directory");
        }
    }

    #[cfg(windows)]
    {
        let first = path.components().find_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        });
        if first.as_deref().is_some_and(|name| {
            matches!(
                name,
                "windows" | "program files" | "program files (x86)" | "programdata"
            )
        }) {
            return Some("operating-system directory");
        }
    }

    None
}
