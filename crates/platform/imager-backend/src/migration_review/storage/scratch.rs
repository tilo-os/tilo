//! Explicit writable scratch custody. Filesystem magic alone is not a proof of
//! physical backing; the admitting broker retains its independently granted
//! storage/medium lease. No source path or TMPDIR selects this capability.
use super::super::super::{
    destination_security,
    migration::{PinnedDirectory, changed},
};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub use super::super::InventoryScratchCustody;

pub struct InventoryScratch {
    directory: Arc<PinnedDirectory>,
    mount: u64,
    allowance: u64,
    minimum_free: AtomicU64,
    restoration: Option<(u64, u64)>,
    charged: Mutex<u64>,
    retained: Mutex<u64>,
    custody: Option<Arc<dyn InventoryScratchCustody>>,
}
pub struct Allocation {
    pub file: File,
    logical_bytes: u64,
    _charge: Charge,
}
impl Allocation {
    pub fn grow(&mut self, length: u64) -> io::Result<()> {
        let scratch = &self._charge.scratch;
        scratch.validate()?;
        let previous = self.logical_bytes;
        if length < previous || self.file.metadata()?.len() != previous {
            return Err(changed("Retained metadata allocation length changed"));
        }
        let rounded = scratch.allocation_bytes(length)?;
        let additional = rounded
            .checked_sub(self._charge.bytes)
            .ok_or_else(|| changed("Retained allocation accounting regressed"))?;
        let mut charged = scratch
            .charged
            .lock()
            .map_err(|_| changed("Retained metadata quota poisoned"))?;
        let limit = scratch.quota_limit()?;
        let total = charged
            .checked_add(additional)
            .filter(|n| *n <= limit)
            .ok_or_else(|| changed("Retained metadata allowance exhausted"))?;
        scratch.require_free(additional)?;
        if length != previous {
            let start = i64::try_from(previous).map_err(io::Error::other)?;
            let count = i64::try_from(length - previous).map_err(io::Error::other)?;
            let error = unsafe { libc::posix_fallocate(self.file.as_raw_fd(), start, count) };
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
        }
        // Account even if a concurrent external writer invalidates the floor:
        // this owned allocation is still live until its descriptor is dropped.
        *charged = total;
        self._charge.bytes = rounded;
        self.logical_bytes = length;
        scratch.validate()?;
        scratch.require_free(0)
    }
}
/// Bounded metadata being written through an independently held private
/// destination. This is a space charge only, never path or write authority.
pub struct MetadataReservation {
    charge: Charge,
}
impl MetadataReservation {
    pub fn grow(&mut self, length: u64) -> io::Result<()> {
        let scratch = &self.charge.scratch;
        scratch.validate()?;
        let bytes = scratch.allocation_bytes(length)?;
        let additional = bytes
            .checked_sub(self.charge.bytes)
            .ok_or_else(|| changed("Durable metadata accounting regressed"))?;
        let mut charged = scratch
            .charged
            .lock()
            .map_err(|_| changed("Retained metadata quota poisoned"))?;
        let limit = scratch.quota_limit()?;
        let total = charged
            .checked_add(additional)
            .filter(|n| *n <= limit)
            .ok_or_else(|| changed("Retained metadata allowance exhausted"))?;
        scratch.require_free(additional)?;
        *charged = total;
        self.charge.bytes = bytes;
        Ok(())
    }
    /// Only exact held, verified private plan files may enter this accounting.
    /// Their bytes already consume physical space, so add equal charged and
    /// admitted amounts: available quota remains unchanged, not reset.
    pub fn retain_file(&mut self, file: &File) -> io::Result<()> {
        self.retain_existing(file, true)
    }
    /// The existing root journal is mutable and exclusively locked, unlike
    /// immutable segments. Legacy hardlink publication may retain an owned
    /// temporary alias after a crash. This accounts one actual inode, and
    /// neither removes aliases nor admits a path or checkpoint.
    pub fn retain_locked_journal(&mut self, file: &advisory_lock::FileLock) -> io::Result<()> {
        self.retain_existing(file, false)
    }
    fn retain_existing(&mut self, file: &File, immutable: bool) -> io::Result<()> {
        let scratch = &self.charge.scratch;
        scratch.validate()?;
        destination_security::check_private_file(file)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || immutable && metadata.nlink() != 1
            || metadata.dev() != scratch.directory.file.metadata()?.dev()
            || fs_native::snapshot_lease::mount_id(file)? != scratch.mount
        {
            return Err(changed(
                "Durable metadata is outside scratch filesystem custody",
            ));
        }
        if self.charge.bytes != 0 && !self.charge.existing {
            return Err(changed(
                "New and existing durable allocation charges cannot mix",
            ));
        }
        let bytes = scratch.allocation_bytes(metadata.len())?;
        scratch.require_free(0)?;
        let mut charged = scratch
            .charged
            .lock()
            .map_err(|_| changed("Retained metadata quota poisoned"))?;
        let mut retained = scratch
            .retained
            .lock()
            .map_err(|_| changed("Retained metadata quota poisoned"))?;
        let total = charged
            .checked_add(bytes)
            .ok_or_else(|| changed("Retained metadata charge overflow"))?;
        let existing = retained
            .checked_add(bytes)
            .ok_or_else(|| changed("Retained metadata admission overflow"))?;
        let owned = self
            .charge
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| changed("Durable allocation charge overflow"))?;
        *charged = total;
        *retained = existing;
        self.charge.bytes = owned;
        self.charge.existing = true;
        Ok(())
    }
    pub fn check(&self) -> io::Result<()> {
        self.charge.scratch.validate()?;
        self.charge.scratch.require_free(0)
    }
}
struct Charge {
    scratch: Arc<InventoryScratch>,
    bytes: u64,
    existing: bool,
}
impl Drop for Charge {
    fn drop(&mut self) {
        if let Ok(mut charged) = self.scratch.charged.lock() {
            *charged = charged.saturating_sub(self.bytes);
            if self.existing {
                if let Ok(mut retained) = self.scratch.retained.lock() {
                    *retained = retained.saturating_sub(self.bytes);
                }
            }
        }
    }
}
impl InventoryScratch {
    fn quota_limit(&self) -> io::Result<u64> {
        self.allowance
            .checked_add(
                *self
                    .retained
                    .lock()
                    .map_err(|_| changed("Retained metadata quota poisoned"))?,
            )
            .ok_or_else(|| changed("Retained metadata allowance overflow"))
    }
    pub fn validate_metadata_destination(&self, destination: &PinnedDirectory) -> io::Result<()> {
        self.validate()?;
        destination.validate_destination()?;
        destination_security::check_private_file(&destination.file)?;
        if destination.file.metadata()?.dev() != self.directory.file.metadata()?.dev()
            || fs_native::snapshot_lease::mount_id(&destination.file)? != self.mount
        {
            return Err(changed(
                "Adoption journal is outside scratch filesystem custody",
            ));
        }
        Ok(())
    }
    pub fn reserve_metadata(self: &Arc<Self>) -> io::Result<MetadataReservation> {
        self.validate()?;
        self.require_free(0)?;
        Ok(MetadataReservation {
            charge: Charge {
                scratch: self.clone(),
                bytes: 0,
                existing: false,
            },
        })
    }
    /// Peak retained original records plus validation merge inputs/output and
    /// both offset indexes. Individual runs still reserve their exact size.
    pub fn validation_allowance(records: u64, entries: u64) -> io::Result<u64> {
        records
            .checked_mul(3)
            .and_then(|runs| {
                entries
                    .checked_mul(16)
                    .and_then(|index| runs.checked_add(index))
            })
            .ok_or_else(|| changed("Inventory scratch allowance overflow"))
    }
    // Caller already holds an explicit writable namespace grant. This does not
    // infer worker credentials from the source owner or create new authority.
    pub fn review(directory: Arc<PinnedDirectory>, allowance: u64) -> io::Result<Arc<Self>> {
        if allowance == 0 {
            return Err(changed("Inventory scratch allowance missing"));
        }
        let value = Arc::new(Self {
            mount: fs_native::snapshot_lease::mount_id(&directory.file)?,
            directory,
            allowance,
            minimum_free: AtomicU64::new(0),
            restoration: None,
            charged: Mutex::new(0),
            retained: Mutex::new(0),
            custody: None,
        });
        value.validate()?;
        value.require_free(allowance)?;
        Ok(value)
    }
    /// Receiver scratch belongs to an already granted private workspace. Keep
    /// restoration and both atomic promotion journal copies out of its quota.
    pub fn review_for_restore(
        directory: Arc<PinnedDirectory>,
        stored_bytes: u64,
        entries: usize,
        metadata_bytes: usize,
        journal_bytes: u64,
    ) -> io::Result<Arc<Self>> {
        directory.validate_destination()?;
        destination_security::check_private_file(&directory.file)?;
        let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::fstatvfs(directory.file.as_raw_fd(), filesystem.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let filesystem = unsafe { filesystem.assume_init() };
        let block = filesystem.f_frsize;
        if block == 0 || filesystem.f_flag & libc::ST_RDONLY != 0 {
            return Err(changed("Archive scratch filesystem is not writable"));
        }
        let round = |bytes: u64| -> io::Result<u64> {
            bytes
                .checked_add(block - 1)
                .map(|n| n / block * block)
                .ok_or_else(|| changed("Archive restoration reserve overflow"))
        };
        let count = u64::try_from(entries).map_err(io::Error::other)?;
        // Stored archive bytes include data and extended metadata. Reserve one
        // additional allocation block per inode plus quarantine directory; sparse
        // logical holes are not copied-byte reservations.
        let minimum_free = round(stored_bytes)?
            .checked_add(
                count
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(block))
                    .ok_or_else(|| changed("Archive inode reserve overflow"))?,
            )
            .and_then(|n| n.checked_add(journal_bytes))
            .ok_or_else(|| changed("Archive restoration reserve overflow"))?;
        let available = filesystem
            .f_bavail
            .checked_mul(block)
            .ok_or_else(|| changed("Archive scratch capacity overflow"))?;
        let remainder = available
            .checked_sub(minimum_free)
            .filter(|n| *n > 0)
            .ok_or_else(|| changed("Archive workspace lacks restoration and index capacity"))?;
        // Path duplication, binary JSON encoding and sparse extent hashes expand
        // bounded authenticated metadata. This is a quota ceiling, never a
        // preallocation: each run reserves only its actual encoded bytes.
        let encoded_bound = u64::try_from(metadata_bytes)
            .map_err(io::Error::other)?
            .checked_mul(128)
            .and_then(|n| count.checked_mul(32768).and_then(|v| n.checked_add(v)))
            .ok_or_else(|| changed("Archive index bound overflow"))?;
        let allowance = remainder.min(Self::validation_allowance(encoded_bound, count)?);
        let mut scratch = Self::review(directory, allowance)?;
        let private = Arc::get_mut(&mut scratch)
            .ok_or_else(|| changed("Archive scratch unexpectedly shared"))?;
        private.minimum_free.store(minimum_free, Ordering::Release);
        private.restoration = Some((block, journal_bytes));
        scratch.require_free(0)?;
        Ok(scratch)
    }
    pub fn restoration_block(&self) -> Option<u64> {
        self.restoration.map(|(block, _)| block)
    }
    pub fn hold_authenticated_restoration(
        &self,
        data_bytes: u64,
        entries: usize,
    ) -> io::Result<()> {
        let (block, journal) = self
            .restoration
            .ok_or_else(|| changed("Archive restoration policy missing"))?;
        self.validate()?;
        let _guard = self
            .charged
            .lock()
            .map_err(|_| changed("Archive scratch quota poisoned"))?;
        let count = u64::try_from(entries).map_err(io::Error::other)?;
        let floor = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(block))
            .and_then(|n| n.checked_add(data_bytes))
            .and_then(|n| n.checked_add(journal))
            .ok_or_else(|| changed("Authenticated restoration reserve overflow"))?;
        self.minimum_free.store(floor, Ordering::Release);
        self.require_free(0)
    }
    pub fn validate_existing_output(&self, output: &PinnedDirectory) -> io::Result<()> {
        self.validate()?;
        output.validate_destination()?;
        destination_security::check_private_file(&output.file)?;
        if cap_fs_ext::MetadataExt::dev(&output.identity)
            == cap_fs_ext::MetadataExt::dev(&self.directory.identity)
            && cap_fs_ext::MetadataExt::ino(&output.identity)
                == cap_fs_ext::MetadataExt::ino(&self.directory.identity)
        {
            return Err(changed(
                "Existing archive output must be nested in held workspace",
            ));
        }
        if !output.contains_identity(&self.directory.identity)
            || cap_fs_ext::MetadataExt::dev(&output.identity)
                != cap_fs_ext::MetadataExt::dev(&self.directory.identity)
            || fs_native::snapshot_lease::mount_id(&output.file)? != self.mount
        {
            return Err(changed(
                "Existing archive output is outside held private workspace",
            ));
        }
        Ok(())
    }
    pub fn check_custody(&self) -> io::Result<()> {
        self.validate()
    }
    // Only after the authentic archive has independently verified its restored
    // bytes may receiver metadata reuse the already-realized data reservation.
    pub fn release_verified_restoration(&self) -> io::Result<()> {
        self.validate()?;
        if let Some((_, journal)) = self.restoration {
            self.minimum_free.store(journal, Ordering::Release);
        }
        self.require_free(0)
    }
    pub fn check_restoration(&self) -> io::Result<()> {
        self.validate()?;
        self.require_free(0)
    }
    pub fn review_with_custody(
        directory: Arc<PinnedDirectory>,
        allowance: u64,
        custody: Arc<dyn InventoryScratchCustody>,
    ) -> io::Result<Arc<Self>> {
        custody.check()?;
        let mut value = Self::review(directory, allowance)?;
        Arc::get_mut(&mut value)
            .ok_or_else(|| changed("Inventory storage unexpectedly shared"))?
            .custody = Some(custody);
        value.validate()?;
        Ok(value)
    }
    pub fn validate_source(&self, source: &PinnedDirectory) -> io::Result<()> {
        self.validate()?;
        source.validate()?;
        if self.directory.contains_identity(&source.identity)
            || source.contains_identity(&self.directory.identity)
        {
            return Err(changed("Inventory scratch overlaps source"));
        }
        Ok(())
    }
    // Only the caller's existing canonical installer custody may justify this
    // exception. SourceInventory owns its exact exclusion proof, not a path filter.
    pub(in crate::migration_review) fn validate_excluded_source(
        &self,
        source: &PinnedDirectory,
        excluded: &PinnedDirectory,
    ) -> io::Result<()> {
        self.validate()?;
        source.validate()?;
        excluded.validate()?;
        // contains_identity inspects this directory's retained ancestry. The
        // source must not be inside scratch; scratch is deliberately inside the
        // exact admitted exclusion, which is itself inside the source.
        if source.contains_identity(&self.directory.identity)
            || !excluded.contains_identity(&source.identity)
            || !self.directory.contains_identity(&excluded.identity)
        {
            return Err(changed(
                "Inventory scratch is outside approved installer custody",
            ));
        }
        Ok(())
    }
    pub(super) fn require_validation(&self, records: u64, entries: u64) -> io::Result<()> {
        self.validate()?;
        if Self::validation_allowance(records, entries)? > self.allowance {
            return Err(changed(
                "Inventory scratch lacks independent validation allowance",
            ));
        }
        Ok(())
    }
    fn validate(&self) -> io::Result<()> {
        if let Some(custody) = &self.custody {
            custody.check()?;
        }
        self.directory.validate_destination()?;
        destination_security::check_private_file(&self.directory.file)?;
        let facts = self.directory.file.metadata()?;
        if !facts.is_dir() || facts.mode() & 0o7777 != 0o700 {
            return Err(changed("Inventory scratch directory is not private"));
        }
        if fs_native::snapshot_lease::mount_id(&self.directory.file)? != self.mount {
            return Err(changed("Inventory scratch mount changed"));
        }
        let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(self.directory.file.as_raw_fd(), filesystem.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let filesystem = unsafe { filesystem.assume_init() };
        if !matches!(
            filesystem.f_type,
            libc::EXT4_SUPER_MAGIC | libc::XFS_SUPER_MAGIC | libc::BTRFS_SUPER_MAGIC
        ) {
            return Err(changed("Inventory scratch filesystem is unsupported"));
        }
        Ok(())
    }
    fn require_free(&self, bytes: u64) -> io::Result<()> {
        let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::fstatvfs(self.directory.file.as_raw_fd(), filesystem.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let filesystem = unsafe { filesystem.assume_init() };
        let available = filesystem
            .f_bavail
            .checked_mul(filesystem.f_frsize)
            .ok_or_else(|| changed("Inventory scratch capacity overflow"))?;
        let required = bytes
            .checked_add(self.minimum_free.load(Ordering::Acquire))
            .ok_or_else(|| changed("Inventory scratch restoration reserve overflow"))?;
        if filesystem.f_flag & libc::ST_RDONLY != 0 || required > available {
            return Err(changed("Inventory scratch capacity unavailable"));
        }
        Ok(())
    }
    fn allocation_bytes(&self, bytes: u64) -> io::Result<u64> {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::fstatvfs(self.directory.file.as_raw_fd(), stats.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let block = unsafe { stats.assume_init() }.f_frsize;
        if block == 0 {
            return Err(changed("Inventory allocation block unavailable"));
        }
        bytes
            .checked_add(block - 1)
            .map(|n| n / block * block)
            .ok_or_else(|| changed("Inventory allocation charge overflow"))
    }
    pub fn allocate(self: &Arc<Self>, bytes: u64) -> io::Result<Allocation> {
        self.validate()?;
        let rounded = self.allocation_bytes(bytes)?;
        self.require_free(rounded)?;
        let mut charged = self
            .charged
            .lock()
            .map_err(|_| changed("Inventory scratch quota poisoned"))?;
        let limit = self.quota_limit()?;
        let total = charged
            .checked_add(rounded)
            .filter(|value| *value <= limit)
            .ok_or_else(|| changed("Inventory scratch allowance exhausted"))?;
        // Re-read available space after acquiring the shared quota lock. A
        // concurrent run allocation must not consume the restoration floor.
        self.require_free(rounded)?;
        // O_TMPFILE creates a 0600 anonymous inode through the held directory.
        // No named-file race, ambient ancestor or temporary cleaner participates.
        let descriptor = unsafe {
            libc::openat(
                self.directory.file.as_raw_fd(),
                c".".as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        destination_security::check_private_file(&file)?;
        let facts = file.metadata()?;
        if !facts.is_file()
            || facts.nlink() != 0
            || facts.dev() != cap_fs_ext::MetadataExt::dev(&self.directory.identity)
        {
            return Err(changed("Inventory scratch file custody changed"));
        }
        // Reserve real blocks before the run is written. A concurrent writer
        // may exhaust free space; failure never becomes a source/target receipt.
        let length = i64::try_from(bytes).map_err(io::Error::other)?;
        if length != 0 {
            let error = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, length) };
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
        }
        self.validate()?;
        self.require_free(0)?;
        *charged = total;
        drop(charged);
        Ok(Allocation {
            file,
            logical_bytes: bytes,
            _charge: Charge {
                scratch: self.clone(),
                bytes: rounded,
                existing: false,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_reserve_counts_original_merge_and_indexes_with_overflow_refusal() {
        assert_eq!(InventoryScratch::validation_allowance(100, 2).unwrap(), 332);
        assert_eq!(InventoryScratch::validation_allowance(0, 0).unwrap(), 0);
        assert!(InventoryScratch::validation_allowance(u64::MAX / 3 + 1, 0).is_err());
        assert!(InventoryScratch::validation_allowance(0, u64::MAX / 16 + 1).is_err());
        assert!(InventoryScratch::validation_allowance(u64::MAX / 3, 1).is_err());
    }
    #[cfg(feature = "native-capture-qualification")]
    #[test]
    fn held_scratch_is_anonymous_reserved_private_and_shares_live_quota() {
        use crate::migration_review::storage::Storage;
        use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::atomic::AtomicBool};
        let root = PathBuf::from(
            std::env::var_os("TILO_FS_NATIVE_STORE")
                .expect("disposable supported writable native store required"),
        );
        let private = tempfile::Builder::new()
            .prefix("inventory-scratch-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(root)
            .unwrap();
        let pin = PinnedDirectory::open_absolute(private.path()).unwrap();
        let scratch = InventoryScratch::review(pin.clone(), 4096).unwrap();
        assert!(scratch.validate_source(&pin).is_err());
        let allocation = scratch.allocate(4096).unwrap();
        let facts = allocation.file.metadata().unwrap();
        assert_eq!(facts.mode() & 0o777, 0o600);
        assert_eq!(facts.nlink(), 0);
        assert_eq!(facts.len(), 4096);
        assert!(scratch.allocate(1).is_err());
        drop(allocation);
        assert_eq!(*scratch.charged.lock().unwrap(), 0);
        let scratch = InventoryScratch::review(pin.clone(), 64 * 1024).unwrap();
        let source = tempfile::Builder::new()
            .prefix("inventory-source-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(private.path().parent().unwrap())
            .unwrap();
        let source_pin = PinnedDirectory::open_absolute(source.path()).unwrap();
        scratch.validate_source(&source_pin).unwrap();
        let file = File::create(source.path().join("data")).unwrap();
        let metadata = cap_primitives::fs::Metadata::from_file(&file).unwrap();
        let cancel = AtomicBool::new(false);
        let mut original = Storage::with_scratch(scratch.clone());
        original
            .insert(
                PathBuf::from("data"),
                super::super::super::Fact::capture(&metadata).unwrap(),
                &cancel,
            )
            .unwrap();
        original.finish(&cancel).unwrap();
        let held = *scratch.charged.lock().unwrap();
        assert!(held > 8);
        let mut validation = original.empty_like();
        validation
            .insert(
                PathBuf::from("data"),
                super::super::super::Fact::capture(&metadata).unwrap(),
                &cancel,
            )
            .unwrap();
        validation.finish(&cancel).unwrap();
        assert_eq!(*scratch.charged.lock().unwrap(), held * 2);
        assert!(
            original.get(std::path::Path::new("data")).unwrap()
                == validation.get(std::path::Path::new("data")).unwrap()
        );
        drop(validation);
        assert_eq!(*scratch.charged.lock().unwrap(), held);
        drop(original);
        assert_eq!(*scratch.charged.lock().unwrap(), 0);
    }
    #[cfg(feature = "native-capture-qualification")]
    #[test]
    fn actual_original_inventory_spill_keeps_scratch_namespace_stable_during_anonymous_growth_and_validation()
     {
        use crate::migration_review::SourceInventory;
        use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::atomic::AtomicBool};
        let root = PathBuf::from(
            std::env::var_os("TILO_FS_NATIVE_STORE")
                .expect("disposable supported writable native store required"),
        );
        let private = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .prefix("anonymous-spill-live-")
            .tempdir_in(&root)
            .unwrap();
        let source = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .prefix("anonymous-spill-source-")
            .tempdir_in(&root)
            .unwrap();
        std::fs::create_dir_all(source.path().join(".var/app/test/data")).unwrap();
        for index in 0..1025 {
            std::fs::write(
                source.path().join(format!(".var/app/test/data/{index}")),
                b"real source bytes",
            )
            .unwrap();
        }
        std::fs::hard_link(
            source.path().join(".var/app/test/data/0"),
            source.path().join("source-only-hardlink"),
        )
        .unwrap();
        let pin = PinnedDirectory::open_absolute(private.path()).unwrap();
        let source_pin = PinnedDirectory::open_absolute(source.path()).unwrap();
        let scratch = InventoryScratch::review(pin.clone(), 64 * 1024 * 1024).unwrap();
        let stamp = |file: &File| {
            let m = file.metadata().unwrap();
            (
                m.dev(),
                m.ino(),
                m.nlink(),
                m.len(),
                m.mode(),
                m.uid(),
                m.gid(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        let before = stamp(&pin.file);
        let mut allocation = scratch.allocate(4096).unwrap();
        assert_eq!(allocation.file.metadata().unwrap().nlink(), 0);
        allocation.grow(8192).unwrap();
        assert_eq!(stamp(&pin.file), before);
        assert!(std::fs::read_dir(private.path()).unwrap().next().is_none());
        drop(allocation);
        let cancel = AtomicBool::new(false);
        let inventory = SourceInventory::capture_original_home_with_scratch(
            &source_pin,
            &[],
            scratch.clone(),
            &cancel,
        )
        .unwrap();
        assert_eq!(inventory.summary().unwrap().files, 1026);
        assert_eq!(
            inventory.facts.peak_buffer, 512,
            "real source walk must spill sorted metadata runs"
        );
        assert_eq!(
            stamp(&pin.file),
            before,
            "O_TMPFILE spill/merge cannot publish temporary directory entries"
        );
        assert!(std::fs::read_dir(private.path()).unwrap().next().is_none());
        inventory
            .validate_with_cancel(&source_pin, &cancel)
            .unwrap();
        assert_eq!(
            stamp(&pin.file),
            before,
            "validation preserves anonymous factory; no NamedTempFile fallback"
        );
        assert!(std::fs::read_dir(private.path()).unwrap().next().is_none());
        let held = inventory
            .facts
            .final_index
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        assert_eq!(held.records.metadata().unwrap().nlink(), 0);
        assert_eq!(held.offsets.metadata().unwrap().nlink(), 0);
        drop(held);
        drop(inventory);
        assert_eq!(*scratch.charged.lock().unwrap(), 0);
        assert_eq!(
            std::fs::read(source.path().join(".var/app/test/data/0")).unwrap(),
            b"real source bytes"
        );
    }
}
