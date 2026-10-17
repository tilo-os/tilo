//! Pure assembly of Windows host observations into [`HostFacts`]. The Win32
//! side only gathers [`Observed`]; every decision lives here so fixtures can
//! prove it on any development host.

use super::super::plan::{Basis, FirmwareFacts};
use super::super::platform::{Disk, SystemVolume};
use super::super::probe::{Blocker, HostFacts};
use super::storage::{self, BitlockerState, PartitionStyle, Snapshot};

/// `GetSystemPowerStatus` fields the battery rule reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerStatus {
    /// 0 offline, 1 online, 255 unknown.
    pub ac_line: u8,
    /// `BATTERY_FLAG_*`; 128 means no system battery.
    pub battery_flag: u8,
    /// 0–100, 255 unknown.
    pub percent: u8,
}

/// Minimum charge for staging on battery power (design spec §5).
pub const MIN_BATTERY_PERCENT: u8 = 50;

/// True when staging must wait for mains power. Unknown readings on a
/// machine that has a battery fail closed.
pub fn on_battery(status: Option<PowerStatus>) -> bool {
    let Some(status) = status else {
        return true;
    };
    if status.ac_line == 1 || status.battery_flag == 128 {
        return false;
    }
    status.percent == 255 || status.percent < MIN_BATTERY_PERCENT
}

/// `UEFISecureBootEnabled` under `HKLM\SYSTEM\CurrentControlSet\Control\
/// SecureBoot\State`. A missing value on UEFI fails closed as enabled; the
/// remedy (restart to firmware settings) is always available for it.
pub fn secure_boot_enabled(registry_value: Option<u32>) -> bool {
    registry_value != Some(0)
}

/// Windows would resume a hibernated kernel (or a Fast Startup image) over a
/// partition map that changed underneath it. Missing registry values take the
/// Windows defaults (both enabled).
pub fn hibernation_active(
    hiberboot_enabled: Option<u32>,
    hibernate_enabled: Option<u32>,
    hiberfile_present: bool,
) -> bool {
    hiberfile_present || (hiberboot_enabled != Some(0) && hibernate_enabled != Some(0))
}

/// Everything the unprivileged Windows probe observes, before policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// `GetFirmwareType` == `FirmwareTypeUefi`; `None` when the call failed.
    pub uefi: Option<bool>,
    pub secure_boot_registry: Option<u32>,
    /// `HiberbootEnabled` under `...\Control\Session Manager\Power`.
    pub hiberboot_enabled: Option<u32>,
    /// `HibernateEnabled` under `...\Control\Power`.
    pub hibernate_enabled: Option<u32>,
    pub hiberfile_present: bool,
    pub power: Option<PowerStatus>,
    /// The Storage-cmdlet snapshot, or why it could not be taken.
    pub snapshot: Result<Snapshot, String>,
    /// The native-layout disk, or why it could not be read.
    pub layout_disk: Result<Disk, String>,
    /// `GetDiskFreeSpaceExW` of the system volume: (total, free) bytes.
    pub volume_space: Option<(u64, u64)>,
}

/// Firmware facts as the unprivileged probe sees them. Windows only lets a
/// process holding `SeSystemEnvironmentPrivilege` (an elevated administrator)
/// read EFI variables, so the boot-variable fields are always left empty here
/// — in the UI *and* in the elevated helper's re-probe, so `basis` equality
/// holds. The helper reads the live `BootCurrent`/`BootOrder`/`BootNext`
/// through [`super::live_firmware_facts`] when it records `original_firmware` and
/// checks `boot-next-busy`; those checks run against the live values, never
/// against the basis.
pub fn basis_firmware(uefi: bool, secure_boot: bool) -> FirmwareFacts {
    FirmwareFacts {
        uefi,
        secure_boot,
        boot_current: None,
        boot_order: Vec::new(),
        boot_next: None,
    }
}

/// Resolve the disk the plan is computed against: the native layout,
/// cross-checked against the Storage cmdlets; the cmdlet-derived disk only
/// when the IOCTL path itself failed.
pub fn resolve_disk(observed: &Observed) -> Result<Disk, String> {
    let snapshot = observed.snapshot.as_ref().map_err(Clone::clone)?;
    match &observed.layout_disk {
        Ok(disk) => {
            storage::cross_check(disk, snapshot)?;
            Ok(disk.clone())
        }
        Err(_) => storage::disk_from_snapshot(snapshot),
    }
}

/// The C: volume with its shrink bounds.
pub fn resolve_system_volume(
    disk: &Disk,
    snapshot: &Snapshot,
    volume_space: Option<(u64, u64)>,
) -> Result<SystemVolume, String> {
    let guid = snapshot
        .system
        .guid
        .clone()
        .ok_or("the Windows partition has no GPT identity")?;
    if !disk.partitions.iter().any(|p| p.guid == guid) {
        return Err("the Windows partition is not on the system disk".into());
    }
    let supported_min_bytes = snapshot
        .system
        .size_min
        .ok_or("Windows did not report how far the system volume can shrink")?;
    let (total, free) = volume_space.ok_or("the system volume's free space is unknown")?;
    let used_bytes = total
        .checked_sub(free)
        .ok_or("the system volume reports more free than total space")?;
    if supported_min_bytes > snapshot.system.size {
        return Err("the reported shrink minimum exceeds the volume".into());
    }
    Ok(SystemVolume {
        partition_guid: guid,
        size_bytes: snapshot.system.size,
        supported_min_bytes,
        used_bytes,
    })
}

/// Apply §5 to what the Windows probe observed. `insufficient-space`,
/// `pending-operation` and `arch-mismatch` are decided by the shared core
/// (planner, record store, catalog) and never produced here.
pub fn assemble(observed: &Observed) -> HostFacts {
    let mut blockers = Vec::new();
    let mut push = |blocker: Blocker| push_unique(&mut blockers, blocker);
    let uefi = observed.uefi == Some(true);
    if !uefi {
        push(Blocker::NotUefi);
    }
    let secure_boot = uefi && secure_boot_enabled(observed.secure_boot_registry);
    if secure_boot {
        push(Blocker::SecureBootOn);
    }
    let bitlocker = observed
        .snapshot
        .as_ref()
        .map_or(BitlockerState::Unknown, |s| {
            storage::bitlocker_state(s.bitlocker_protection)
        });
    if matches!(
        bitlocker,
        BitlockerState::Locked | BitlockerState::Converting
    ) {
        push(Blocker::BitlockerLocked);
    }
    let bitlocker_protected = !matches!(bitlocker, BitlockerState::Unprotected);
    match &observed.snapshot {
        Ok(snapshot) => {
            if snapshot.disk.style != PartitionStyle::Gpt {
                push(Blocker::NotGpt);
            }
            if storage::unsupported_reason(snapshot).is_some() {
                push(Blocker::DiskUnsupported);
            }
        }
        Err(_) => push(Blocker::DiskUnsupported),
    }
    if hibernation_active(
        observed.hiberboot_enabled,
        observed.hibernate_enabled,
        observed.hiberfile_present,
    ) {
        push(Blocker::Hibernation);
    }
    if on_battery(observed.power) {
        push(Blocker::OnBattery);
    }
    let basis = basis(observed, uefi, secure_boot);
    if basis.is_none() && !blockers.contains(&Blocker::NotGpt) {
        push_unique(&mut blockers, Blocker::DiskUnsupported);
    }
    HostFacts {
        basis,
        bitlocker_protected,
        blockers,
    }
}

fn push_unique(blockers: &mut Vec<Blocker>, blocker: Blocker) {
    if !blockers.contains(&blocker) {
        blockers.push(blocker);
    }
}

fn basis(observed: &Observed, uefi: bool, secure_boot: bool) -> Option<Basis> {
    let snapshot = observed.snapshot.as_ref().ok()?;
    if snapshot.disk.style != PartitionStyle::Gpt {
        return None;
    }
    let disk = resolve_disk(observed).ok()?;
    let system_volume = resolve_system_volume(&disk, snapshot, observed.volume_space).ok()?;
    Some(Basis {
        linux_btrfs_volume: None,
        disk,
        system_volume: Some(system_volume),
        firmware: basis_firmware(uefi, secure_boot),
    })
}

/// Why [`assemble`] produced no basis, for logs and the helper's refusal text.
pub fn basis_error(observed: &Observed) -> Option<String> {
    let snapshot = match &observed.snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => return Some(error.clone()),
    };
    if let Some(reason) = storage::unsupported_reason(snapshot) {
        return Some(reason);
    }
    resolve_disk(observed)
        .and_then(|disk| resolve_system_volume(&disk, snapshot, observed.volume_space))
        .err()
}
