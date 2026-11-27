//! Read-only Windows observation through the signed-driver and CIM views.

use std::time::Duration;

use super::SupportReport;

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);

const SCRIPT: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
$cs = Get-CimInstance Win32_ComputerSystem
$bios = Get-CimInstance Win32_BIOS
$board = Get-CimInstance Win32_BaseBoard
$video = @(Get-CimInstance Win32_VideoController)
$net = @(Get-CimInstance Win32_NetworkAdapter -Filter 'PhysicalAdapter=True')
$battery = @(Get-CimInstance Win32_Battery)
$devices = @(Get-CimInstance Win32_PnPEntity | Where-Object {
  $_.PNPClass -in @('Camera','Bluetooth','AudioEndpoint','Biometric','Keyboard','Mouse') -or
  $_.Name -match 'Camera|Webcam|Fingerprint|Biometric|Bluetooth|Audio|Microphone|Speaker|Headphone|Headset|Keyboard|Touchpad|Mouse'
} | Select-Object Name,PNPClass,Status,PNPDeviceID)
$disks = @(Get-CimInstance Win32_DiskDrive | Select-Object Model,Size,InterfaceType,Status)
$monitors = @(Get-CimInstance -Namespace 'root/wmi' -ClassName WmiMonitorID | Select-Object InstanceName,Active)
$tpm = @(Get-CimInstance -Namespace 'root/cimv2/Security/MicrosoftTpm' -ClassName Win32_Tpm | Select-Object SpecVersion,IsEnabled_InitialValue)
$drivers = @(Get-CimInstance Win32_PnPSignedDriver | Select-Object DeviceID,DriverProviderName,DriverVersion)
$firmwareType = $null
try {
  $pe = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control' -Name PEFirmwareType).PEFirmwareType
  $firmwareType = if ($pe -eq 2) { 'Uefi' } elseif ($pe -eq 1) { 'Bios' } else { $null }
} catch {}
if (-not $firmwareType) {
  try { $firmwareType = (Get-ComputerInfo -Property BiosFirmwareType).BiosFirmwareType } catch {}
}
$secureBoot = $null
try { $secureBoot = Confirm-SecureBootUEFI } catch {}
[PSCustomObject]@{
  manufacturer = $cs.Manufacturer
  model = $cs.Model
  family = $cs.SystemFamily
  sku = $cs.SystemSKUNumber
  biosVendor = $bios.Manufacturer
  biosVersion = $bios.SMBIOSBIOSVersion
  biosDate = $bios.ReleaseDate
  boardVendor = $board.Manufacturer
  boardProduct = $board.Product
  boardVersion = $board.Version
  osVersion = [System.Environment]::OSVersion.VersionString
  firmwareType = $firmwareType
  secureBoot = $secureBoot
  graphics = @($video | ForEach-Object { [PSCustomObject]@{ name = $_.Name; vendor = $_.AdapterCompatibility; driverVersion = $_.DriverVersion; deviceId = $_.PNPDeviceID } })
  network = @($net | ForEach-Object { [PSCustomObject]@{ name = $_.Name; type = $_.AdapterType; enabled = $_.NetEnabled; id = $_.PNPDeviceID } })
  battery = @($battery | ForEach-Object { [PSCustomObject]@{ name = $_.Name; capacity = $_.EstimatedChargeRemaining; status = $_.BatteryStatus } })
  devices = @($devices | ForEach-Object { [PSCustomObject]@{ name = $_.Name; class = $_.PNPClass; status = $_.Status; id = $_.PNPDeviceID } })
  monitors = @($monitors | ForEach-Object { [PSCustomObject]@{ name = $_.InstanceName; active = $_.Active } })
  disks = @($disks | ForEach-Object { [PSCustomObject]@{ name = $_.Model; size = $_.Size; type = $_.InterfaceType; status = $_.Status } })
  tpm = @($tpm | ForEach-Object { [PSCustomObject]@{ spec = $_.SpecVersion; enabled = $_.IsEnabled_InitialValue } })
  drivers = @($drivers | ForEach-Object { [PSCustomObject]@{ deviceId = $_.DeviceID; provider = $_.DriverProviderName; version = $_.DriverVersion } })
} | ConvertTo-Json -Depth 6 -Compress"#;

pub(super) fn probe() -> SupportReport {
    let output = bounded_command::run(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", SCRIPT],
        None,
        OUTPUT_LIMIT,
        TIMEOUT,
    );
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            match super::windows_json::parse(&text) {
                Ok(mut raw) => {
                    raw.architecture = Some(std::env::consts::ARCH.to_string());
                    raw.into_report()
                }
                Err(_) => super::RawReport::default().into_report(),
            }
        }
        _ => super::RawReport::default().into_report(),
    }
}
