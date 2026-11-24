//! The guided setup's destination side on the live installer medium: what it
//! reads from and does to the machine it installs.
//!
//! The installer's wizard is the imager app's (`setup::destination`); this is
//! its platform half. Nothing here draws or holds wizard state.
//!
//! * [`disks`]: target disks and partitions, from `lsblk`.
//! * [`install`] and [`backend`]: the privileged install, preflight and power
//!   helpers, behind the boundary host tests replace.
//! * [`internal_handoff`]: the staged (USB-free) installation's handoff.
//! * [`media_profile`]: the profile the host side prepared on the medium.
//! * [`network`]: connectivity, Wi-Fi and captive portals, off the UI thread.
//! * [`power_readiness`]: the battery rule before installing.
//! * [`probes`]: what this computer proves about its boot chain.
//! * [`private_io`]: the unattended recipe and recovery key's private files.
//! * [`layouts`], [`timezones`]: keyboard layouts and time zones on offer.

pub mod backend;
pub mod disks;
pub mod install;
pub mod internal_handoff;
pub mod layouts;
pub mod media_profile;
pub mod network;
pub mod power_readiness;
pub mod private_io;
pub mod probes;
pub mod timezones;

/// The installer's console: action frames on the live medium's private socket
/// and virtio port.
pub use console_control as console;
pub use disks::{Disk, DiskPartition, parse_lsblk, read_disks};
