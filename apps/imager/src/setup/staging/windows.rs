//! Windows staging adapter (design spec §4 "Partition adapter" / "Firmware
//! adapter", §5 probe).
//!
//! - Disk facts come from metadata-only IOCTLs on `\\.\PhysicalDriveN`
//!   (`IOCTL_DISK_GET_DRIVE_LAYOUT_EX`, whose GPT header values give the
//!   usable LBA range, and `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`), which need no
//!   elevation, cross-checked against one read-only Storage-cmdlet snapshot.
//!   The elevated helper also reads the on-disk GPT and requires it to agree,
//!   including that Windows partition numbers equal GPT entry indices.
//! - Partition changes use `Resize-Partition` / `New-Partition` /
//!   `Remove-Partition`, each preceded by an identity re-check and followed by
//!   an exact read-back.
//! - Raw staging writes open `\\.\PhysicalDriveN` unbuffered and write-through,
//!   only inside a staging-typed partition that has no mounted volume.
//! - EFI variables go through `Get/SetFirmwareEnvironmentVariableExW` after
//!   enabling `SeSystemEnvironmentPrivilege`.
//!
//! The decoders and every policy decision are pure and compiled on all hosts
//! (`cfg(any(windows, test))`) so their fixtures run anywhere; only
//! `sys`/`adapter` and the remedy launchers touch Win32.

pub mod facts;
pub mod layout;
pub mod ops;
pub mod remedies;
pub mod scripts;
pub mod storage;

#[cfg(windows)]
mod adapter;
#[cfg(windows)]
mod sys;

#[cfg(windows)]
pub use adapter::{WindowsPlatform, probe_facts};

use super::plan::FirmwareFacts;
use super::platform::{IoResult, Platform};

/// Fill `BootCurrent`, `BootOrder` and `BootNext` from live firmware through
/// `platform` (on Windows: the elevated [`WindowsPlatform`]), keeping
/// `uefi`/`secure_boot` from `base`. The helper uses this for
/// `original_firmware` and the `boot-next-busy` check; the reviewed basis
/// always carries these fields empty on Windows ([`facts::basis_firmware`]).
pub fn live_firmware_facts<P: Platform + ?Sized>(
    platform: &P,
    base: &FirmwareFacts,
) -> IoResult<FirmwareFacts> {
    let boot_current = platform
        .get_efi_var("BootCurrent")?
        .map(|bytes| ops::decode_boot_number(&bytes))
        .transpose()?;
    let boot_order = platform
        .get_efi_var("BootOrder")?
        .map(|bytes| ops::decode_boot_order(&bytes))
        .transpose()?
        .unwrap_or_default();
    let boot_next = platform
        .get_efi_var("BootNext")?
        .map(|bytes| ops::decode_boot_number(&bytes))
        .transpose()?;
    Ok(FirmwareFacts {
        uefi: base.uefi,
        secure_boot: base.secure_boot,
        boot_current,
        boot_order,
        boot_next,
    })
}

#[cfg(test)]
mod tests;
