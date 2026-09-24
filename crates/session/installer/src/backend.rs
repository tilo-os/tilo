//! The installer's side-effecting seam: disk discovery, the privileged
//! install and preflight helpers, and the power helper.
//!
//! The live medium uses [`SystemBackend`], which runs the real
//! `lsblk` / `/usr/libexec/tilo-install` / `/usr/libexec/tilo-power`
//! programs. Host-side tests hand the wizard a recording fake instead, so the
//! whole flow — every step, the destructive confirmation, progress, the
//! recovery key and the power actions — runs with no disk touched.

use crate::disks::Disk;
use crate::install::{InstallOptions, PowerAction};

pub trait InstallBackend: Send + Sync {
    /// The disks the Disk step offers.
    fn read_disks(&self) -> Vec<Disk>;

    /// Run the install, streaming each output line to `on_line`. `Ok(true)`
    /// is a verified success, `Ok(false)` a completed failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the install helper cannot be started or its
    /// output cannot be read.
    fn run_install(
        &self,
        disk: &str,
        user: &str,
        password: &str,
        opts: &InstallOptions,
        on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool>;

    /// Read-only check of a prepared alongside root and EFI partition.
    ///
    /// # Errors
    ///
    /// Returns an error when the preflight helper cannot be started.
    fn run_alongside_preflight(
        &self,
        disk: &str,
        opts: &InstallOptions,
        on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool>;

    /// Reboot or power off. On success the machine is already going down.
    ///
    /// # Errors
    ///
    /// Returns an error when the power helper cannot be started.
    fn run_power(&self, action: PowerAction) -> std::io::Result<bool>;
}

/// The real helpers on the live medium.
pub struct SystemBackend;

impl InstallBackend for SystemBackend {
    fn read_disks(&self) -> Vec<Disk> {
        crate::disks::read_disks()
    }

    fn run_install(
        &self,
        disk: &str,
        user: &str,
        password: &str,
        opts: &InstallOptions,
        on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        crate::install::run_install(disk, user, password, opts, on_line)
    }

    fn run_alongside_preflight(
        &self,
        disk: &str,
        opts: &InstallOptions,
        on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        crate::install::run_alongside_preflight(disk, opts, on_line)
    }

    fn run_power(&self, action: PowerAction) -> std::io::Result<bool> {
        crate::install::run_power(action)
    }
}
