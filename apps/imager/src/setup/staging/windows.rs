//! Windows [`Platform`] and probe facts. Placeholder so the shared core
//! compiles on Windows; the Windows adapter workstream owns this file and
//! replaces it. Expected surface (used by `probe::probe` and the helper):
//!
//! - `pub fn probe_facts() -> HostFacts` — unprivileged probe, calling
//!   `probe::add_shared_blockers` on its result;
//! - `pub struct WindowsPlatform` implementing `Platform`, with
//!   `pub fn elevated() -> Result<WindowsPlatform, String>` for the helper
//!   and `pub fn read_only() -> Result<WindowsPlatform, String>` for the UI.

use super::platform::{Disk, IoResult, NewPartition, Platform, SystemVolume};
use super::probe::{Blocker, HostFacts};

pub fn probe_facts() -> HostFacts {
    HostFacts::unsupported(Blocker::DiskUnsupported)
}

pub struct WindowsPlatform;

impl WindowsPlatform {
    pub fn elevated() -> Result<Self, String> {
        Err(MISSING.into())
    }
    pub fn read_only() -> Result<Self, String> {
        Err(MISSING.into())
    }
}

const MISSING: &str = "the Windows staging adapter is not implemented";

impl Platform for WindowsPlatform {
    fn system_disk(&self) -> IoResult<Disk> {
        Err(MISSING.into())
    }
    fn system_volume(&self) -> IoResult<Option<SystemVolume>> {
        Err(MISSING.into())
    }
    fn shrink_system_volume(&mut self, _: &str, _: u64) -> IoResult<()> {
        Err(MISSING.into())
    }
    fn grow_system_volume(&mut self, _: &str, _: u64) -> IoResult<()> {
        Err(MISSING.into())
    }
    fn create_partition(&mut self, _: &Disk, _: &NewPartition) -> IoResult<String> {
        Err(MISSING.into())
    }
    fn delete_partition(&mut self, _: &Disk, _: &str) -> IoResult<()> {
        Err(MISSING.into())
    }
    fn write_partition(&mut self, _: &Disk, _: &str, _: u64, _: &[u8]) -> IoResult<()> {
        Err(MISSING.into())
    }
    fn read_partition(&self, _: &Disk, _: &str, _: u64, _: &mut [u8]) -> IoResult<()> {
        Err(MISSING.into())
    }
    fn sync_disk(&mut self, _: &Disk) -> IoResult<()> {
        Err(MISSING.into())
    }
    fn get_efi_var(&self, _: &str) -> IoResult<Option<Vec<u8>>> {
        Err(MISSING.into())
    }
    fn set_efi_var(&mut self, _: &str, _: &[u8]) -> IoResult<()> {
        Err(MISSING.into())
    }
    fn delete_efi_var(&mut self, _: &str) -> IoResult<()> {
        Err(MISSING.into())
    }
}
