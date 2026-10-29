//! The guided setup's destination side on the live installer medium: what it
//! reads from and does to the machine it installs.
//!
//! The installer's wizard is the imager app's (`setup::destination`); this is
//! its platform half. Nothing here draws or holds wizard state.
//!
//! * [`disks`]: target disks and partitions, from `lsblk`.
//! * [`install`] and [`backend`]: the privileged install, preflight and power
//!   helpers, behind the seam host tests replace.
//! * [`internal_handoff`]: the staged (USB-free) installation's handoff.
//! * [`media_profile`]: the profile the host side prepared on the medium.
//! * [`network`]: connectivity, Wi-Fi and captive portals, off the UI thread.
//! * [`online_accounts`]: device-private account sign-in and custody.
//! * [`power_readiness`]: the battery rule before installing.
//! * [`probes`]: what this computer proves about its boot chain.
//! * [`private_io`]: the unattended recipe and recovery key's private files.
//! * [`layouts`], [`timezones`]: keyboard layouts and time zones on offer.
//! * [`windows_hint_source`]: hints read from the medium's Windows block.

pub mod backend;
pub mod disks;
pub mod install;
pub mod internal_handoff;
pub mod layouts;
pub mod media_profile;
pub mod network;
pub mod online_accounts;
pub mod power_readiness;
pub mod private_io;
pub mod probes;
pub mod timezones;
pub mod windows_hint_source;

/// The installer's console: the `tilo-control` action protocol on the live
/// medium's private socket and virtio port.
pub use console_control as console;
pub use disks::{Disk, DiskPartition, parse_lsblk, read_disks};
/// The online accounts the installer hands to the installed system's first
/// boot, which its seeding helper imports.
pub use tilo_install_helpers::account_handoff;
