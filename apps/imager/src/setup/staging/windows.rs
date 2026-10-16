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

#[cfg(test)]
mod tests;
