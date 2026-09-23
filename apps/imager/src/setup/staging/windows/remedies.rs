//! Remedies the Review page offers for Windows blockers (design spec §5).
//! Each one that needs administrator rights raises its own UAC prompt and
//! returns only after the elevated program exited; none of them touches
//! disks, partitions or boot variables.
//!
//! - [`restart_to_firmware_settings`] — `secure-boot-on`: suspends BitLocker
//!   for exactly one restart when it protects the Windows volume (a firmware
//!   settings change would otherwise demand the recovery key), then
//!   `shutdown /r /fw /t 0`.
//! - [`disable_fast_startup`] — `hibernation`: `powercfg /h off`, which turns
//!   off hibernation and with it Fast Startup, then re-probes.
//! - [`restart_now`] — "Restart to install Tilo" after staging.

use super::scripts::{self, DriveLetter};

/// Exit codes of the elevated firmware-settings program.
pub const EXIT_BITLOCKER_UNKNOWN: i32 = 20;
pub const EXIT_BITLOCKER_STATUS: i32 = 21;
pub const EXIT_BITLOCKER_SUSPEND: i32 = 22;
pub const EXIT_BITLOCKER_STILL_ON: i32 = 23;

/// The elevated half of [`restart_to_firmware_settings`]. The authoritative
/// BitLocker status is read through `Win32_EncryptableVolume` (the provider
/// behind `Suspend-BitLocker`, present on every edition that can encrypt).
/// When the provider is missing the program proceeds only if the unprivileged
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

/// The elevated half of [`disable_fast_startup`].
pub fn disable_fast_startup_program() -> String {
    "$ErrorActionPreference = 'Stop'\n& \"$env:SystemRoot\\System32\\powercfg.exe\" /h off\nexit $LASTEXITCODE\n"
        .to_string()
}

/// Human-readable meaning of a remedy's exit status.
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
