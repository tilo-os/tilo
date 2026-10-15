//! Shared exclusive inventory creation; callers retain native output authority.
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
use crate::setup::{migration::PinnedDirectory, migration_durability, nofollow};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::Path,
    sync::Arc,
};

pub(super) struct InventoryScratch {
    parent: Arc<PinnedDirectory>,
    name: String,
    directory: File,
    identity: (u64, u64),
    uid: u32,
    gid: u32,
    mount: u64,
    allowance: u64,
}
impl InventoryScratch {
    pub(super) fn create(
        parent: Arc<PinnedDirectory>,
        name: String,
        caller: (u32, u32),
        reserve_bytes: u64,
        verify: impl Fn(&File) -> io::Result<()>,
    ) -> io::Result<Self> {
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || caller.0 == 0
        {
            return Err(refused(
                "Inventory storage requires the admitted original caller",
            ));
        }
        parent.validate_destination()?;
        if !name.strip_prefix("inventory-").is_some_and(|suffix| {
            uuid::Uuid::parse_str(suffix).is_ok_and(|id| !id.is_nil() && id.to_string() == suffix)
        }) {
            return Err(refused("Inventory attempt name is not canonical"));
        }
        if reserve_bytes == 0 {
            return Err(refused("Inventory output reservation is absent"));
        }
        // Exclusive creation: an existing name is not adopted or repaired.
        let child = parent.create_child(OsStr::new(&name))?;
        let directory = migration_durability::readable_directory(&child.file)?;
        let before = directory.metadata()?;
        verify(&directory)?;
        let mount = fs_native::snapshot_lease::mount_id(&directory)?;
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(directory.as_raw_fd(), &raw mut stats) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let free = stats
            .f_bavail
            .checked_mul(stats.f_frsize)
            .ok_or_else(|| refused("Inventory storage space overflows"))?;
        // Preserve the entire authenticated aggregate output reservation and
        // canonical framing/metadata allowance. Runs reserve real blocks later.
        let allowance = inventory_allowance(free, reserve_bytes)?;
        if unsafe { libc::fchown(directory.as_raw_fd(), caller.0, caller.1) } < 0 {
            return Err(io::Error::last_os_error());
        }
        directory.sync_all()?;
        migration_durability::sync_directory(&parent.file)?;
        let value = Self {
            parent,
            name,
            identity: (before.dev(), before.ino()),
            directory,
            uid: caller.0,
            gid: caller.1,
            mount,
            allowance,
        };
        value.check()?;
        verify(&value.directory)?;
        Ok(value)
    }
    /// Reopen only the inventory named and pinned by an actual protected recovery.
    pub(super) fn review_recovered(
        proof: &super::store::InstalledCopyRecovery,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Self> {
        let (_, parent, _, identity, allowance) = proof.scratch_parts(cancel)?;
        let attempt = proof.attempt();
        let scope = attempt.source().output_scratch_scope(cancel)?;
        let name = format!("inventory-{}", attempt.attempt());
        let metadata = nofollow::metadata_nofollow(&parent.file, Path::new(&name))?;
        let pin = parent.open_child(OsStr::new(&name), &metadata)?;
        let directory = migration_durability::readable_directory(&pin.file)?;
        let value = Self {
            parent,
            name,
            identity,
            uid: scope.caller.0,
            gid: scope.caller.1,
            mount: fs_native::snapshot_lease::mount_id(&directory)?,
            directory,
            allowance,
        };
        value.check()?;
        proof.check(cancel)?;
        Ok(value)
    }
    pub(super) fn check(&self) -> io::Result<()> {
        self.parent.validate_destination()?;
        let current = nofollow::metadata_nofollow(&self.parent.file, Path::new(&self.name))?;
        let held = self.directory.metadata()?;
        if !current.is_dir()
            || (
                cap_fs_ext::MetadataExt::dev(&current),
                cap_fs_ext::MetadataExt::ino(&current),
            ) != self.identity
            || !held.is_dir()
            || (held.dev(), held.ino()) != self.identity
            || held.uid() != self.uid
            || held.gid() != self.gid
            || held.mode() & 0o7777 != 0o700
            || fs_native::snapshot_lease::mount_id(&self.directory)? != self.mount
        {
            return Err(refused("Admitted inventory directory custody changed"));
        }
        Ok(())
    }
    pub(super) fn files(&self) -> io::Result<(File, u64)> {
        self.check()?;
        let file = self.directory.try_clone()?;
        self.check()?;
        Ok((file, self.allowance))
    }
}

fn inventory_allowance(free: u64, reserve_bytes: u64) -> io::Result<u64> {
    if reserve_bytes == 0 {
        return Err(refused("Inventory output reservation is absent"));
    }
    crate::setup::portable_home_media::payload_capacity_for_free_bytes(free)?
        .checked_sub(reserve_bytes)
        .filter(|v| *v != 0)
        .ok_or_else(|| refused("Output lacks independent inventory working space"))
}
#[cfg(test)]
mod tests {
    use super::inventory_allowance;
    #[test]
    fn inventory_budget_preserves_full_output_and_canonical_metadata_reservation() {
        let free = 1024 * 1024 * 1024;
        let capacity =
            crate::setup::portable_home_media::payload_capacity_for_free_bytes(free).unwrap();
        assert!(inventory_allowance(free, 0).is_err());
        assert!(inventory_allowance(free, capacity).is_err());
        assert!(inventory_allowance(free, capacity + 1).is_err());
        assert_eq!(inventory_allowance(free, capacity - 1).unwrap(), 1);
        assert_eq!(inventory_allowance(free, 12345).unwrap(), capacity - 12345);
    }
}
