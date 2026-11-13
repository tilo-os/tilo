//! Remedies the Review page offers for Windows blockers. Each one that needs administrator rights
//! raises its own UAC prompt and returns after the elevated program exited; none touches disks,
//! partitions or boot variables.

use super::scripts::{self, DriveLetter};

pub const EXIT_BITLOCKER_UNKNOWN: i32 = 20;
pub const EXIT_BITLOCKER_STATUS: i32 = 21;
pub const EXIT_BITLOCKER_SUSPEND: i32 = 22;
pub const EXIT_BITLOCKER_STILL_ON: i32 = 23;

/// The authoritative BitLocker status is read through `Win32_EncryptableVolume`, the provider behind
/// `Suspend-BitLocker`. When the provider is missing the program proceeds only if the unprivileged
/// probe also saw no protection (`protected_hint`).
pub fn firmware_settings_program(drive: DriveLetter, protected_hint: bool) -> String {
    let letter = drive.letter();
    let hint = if protected_hint { "$true" } else { "$false" };
    format!(
        r#"$ErrorActionPreference = 'Stop'
$protectedHint = {hint}
$ev = $null
try {{ $ev = Get-CimInstance -Namespace 'root/cimv2/Security/MicrosoftVolumeEncryption' -ClassName Win32_EncryptableVolume -Filter "DriveLetter='{letter}:'" }} catch {{ if ($protectedHint) {{ exit {EXIT_BITLOCKER_UNKNOWN} }} }}
if ($ev) {{
  $st = Invoke-CimMethod -InputObject $ev -MethodName GetProtectionStatus
  if ($st.ReturnValue -ne 0) {{ exit {EXIT_BITLOCKER_STATUS} }}
  if ($st.ProtectionStatus -eq 1) {{
    $r = Invoke-CimMethod -InputObject $ev -MethodName DisableKeyProtectors -Arguments @{{ DisableCount = [uint32]1 }}
    if ($r.ReturnValue -ne 0) {{ exit {EXIT_BITLOCKER_SUSPEND} }}
    $st = Invoke-CimMethod -InputObject $ev -MethodName GetProtectionStatus
    if ($st.ProtectionStatus -ne 0) {{ exit {EXIT_BITLOCKER_STILL_ON} }}
  }} elseif ($st.ProtectionStatus -ne 0) {{ exit {EXIT_BITLOCKER_STATUS} }}
}} elseif ($protectedHint) {{ exit {EXIT_BITLOCKER_UNKNOWN} }}
& "$env:SystemRoot\System32\shutdown.exe" /r /fw /t 0
exit $LASTEXITCODE
"#
    )
}

pub fn disable_fast_startup_program() -> String {
    "$ErrorActionPreference = 'Stop'\n& \"$env:SystemRoot\\System32\\powercfg.exe\" /h off\nexit $LASTEXITCODE\n"
        .to_string()
}

pub fn describe_exit(code: Option<i32>) -> Result<(), String> {
    match code {
        Some(0) => Ok(()),
        Some(scripts::EXIT_ELEVATION_DECLINED) => {
            Err("administrator permission was not granted".into())
        }
        Some(EXIT_BITLOCKER_UNKNOWN) => {
            Err("could not confirm whether BitLocker protects the Windows volume".into())
        }
        Some(EXIT_BITLOCKER_STATUS) => Err("could not read BitLocker protection status".into()),
        Some(EXIT_BITLOCKER_SUSPEND | EXIT_BITLOCKER_STILL_ON) => Err(
            "BitLocker protection could not be suspended for the restart; nothing was changed"
                .into(),
        ),
        Some(code) => Err(format!("the Windows command failed with status {code}")),
        None => Err("the Windows command was terminated".into()),
    }
}

#[cfg(windows)]
pub use imp::{disable_fast_startup, restart_now, restart_to_firmware_settings};

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use super::super::sys;
    use super::{describe_exit, disable_fast_startup_program, firmware_settings_program};
    use crate::staging::windows::scripts;

    const ELEVATION_TIMEOUT: Duration = Duration::from_secs(600);
    const OUTPUT_LIMIT: usize = 64 * 1024;

    fn run_elevated(inner: &str) -> Result<(), String> {
        let powershell = sys::system_executable(sys::POWERSHELL)?;
        let outer = scripts::elevated(&powershell.to_string_lossy(), inner);
        let out = bounded_command::run_os(
            &powershell,
            &scripts::powershell_args(&outer),
            None,
            OUTPUT_LIMIT,
            ELEVATION_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
        describe_exit(out.status.code())
    }

    /// Suspends BitLocker for one restart first when it is on; `bitlocker_protected` is
    /// [`HostFacts::bitlocker_protected`].
    pub fn restart_to_firmware_settings(bitlocker_protected: bool) -> Result<(), String> {
        let drive = sys::windows_drive().map_err(|e| e.to_string())?;
        run_elevated(&firmware_settings_program(drive, bitlocker_protected))
    }

    pub fn disable_fast_startup() -> Result<(), String> {
        run_elevated(&disable_fast_startup_program())?;
        let observed = sys::hibernation_observation();
        if super::super::facts::hibernation_active(observed.0, observed.1, observed.2) {
            return Err("Windows still reports hibernation or Fast Startup as enabled".into());
        }
        Ok(())
    }

    pub fn restart_now() -> Result<(), String> {
        let shutdown = sys::system_executable(sys::SHUTDOWN)?;
        let out = bounded_command::run_os(
            &shutdown,
            &["/r", "/t", "0"],
            None,
            OUTPUT_LIMIT,
            Duration::from_secs(30),
        )
        .map_err(|error| error.to_string())?;
        describe_exit(out.status.code())
    }
}
