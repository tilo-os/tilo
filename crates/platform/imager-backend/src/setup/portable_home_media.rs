//! Whole-home export into one explicitly granted media filesystem. Only a
//! completed, independently verified ciphertext inode is published.
use super::{
    migration::{PinnedDirectory, changed, check_cancel},
    migration_durability as durable,
    migration_review::SourceInventory,
    nofollow::metadata_nofollow,
    portable_transfer::{self, ExportSummary, Limits, Purpose},
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions, OpenOptionsExt};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use zeroize::Zeroizing;
#[path = "portable_applications_media.rs"]
mod applications;
#[path = "browser_media.rs"]
mod browser;
mod captured;
mod original_registration;
pub use original_registration::{
    Registered as RegisteredOriginalSource, register_applications, register_staging_copy,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
const RESERVE: u64 = 256 * 1024;
#[derive(Debug)]
struct ReviewedHomeChanged(io::Error);
impl std::fmt::Display for ReviewedHomeChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Reviewed home changed; review the selected files again")
    }
}
impl std::error::Error for ReviewedHomeChanged {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
pub fn source_changed(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|error| error.is::<ReviewedHomeChanged>())
}
#[derive(Debug)]
struct ReviewedHomeUnavailable(io::Error);
impl std::fmt::Display for ReviewedHomeUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Reviewed home is unavailable; renew the selected files review")
    }
}
impl std::error::Error for ReviewedHomeUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
fn source_unavailable(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::Interrupted {
        return error;
    }
    io::Error::new(error.kind(), ReviewedHomeUnavailable(error))
}
pub fn source_review_error(error: &io::Error) -> Option<&'static str> {
    if source_changed(error) {
        Some("installer-source-changed")
    } else if error
        .get_ref()
        .is_some_and(|error| error.is::<ReviewedHomeUnavailable>())
    {
        Some("installer-source-unavailable")
    } else {
        None
    }
}
/// Device-local immutable source consent; no path or home data enters CRDT state.
pub struct HomeSource {
    original_only: bool,
    pin: Arc<PinnedDirectory>,
    inventory: Option<SourceInventory>,
    captured: Option<captured::Captured>,
    granted_path: Option<std::path::PathBuf>,
    original_roots: Option<Arc<super::GrantedHomeRoots>>,
    /// Nested subtrees the person excluded from this source. The inventory is
    /// built and revalidated with these applied, so the sealed archive and the
    /// installed import both honour them.
    exclusions: Vec<std::path::PathBuf>,
}
impl HomeSource {
    /// Recover source-bound permission candidates from this Home review refusal.
    /// This never creates a coherent epoch, broker declaration or export receipt.
    #[cfg(target_os = "linux")]
    pub fn mounted_root_set(error: &io::Error) -> Option<super::HomeRootSet> {
        super::migration_review::mounted_home_locations(error).map(super::HomeRootSet::new)
    }
    /// Verify original source authority without promising a coherent live copy.
    pub fn validate_offline_scope(&self, plan: &super::migration_handoff::Plan) -> io::Result<()> {
        self.pin.validate()?;
        let source = self
            .granted_path
            .as_ref()
            .ok_or_else(|| changed("Captured source cannot silently become offline original"))?;
        let current =
            super::migration_handoff::capture(source, &plan.operation_id, plan.categories)?;
        if current.home != plan.home {
            return Err(changed(
                "Offline source differs from selected original home",
            ));
        }
        self.pin.validate()
    }
    pub fn entry_count(&self) -> usize {
        self.captured.as_ref().map_or_else(
            || {
                self.inventory
                    .as_ref()
                    .map_or(0, SourceInventory::entry_count)
            },
            captured::Captured::entries,
        )
    }
    fn inventory(&self) -> io::Result<&SourceInventory> {
        self.inventory
            .as_ref()
            .ok_or_else(|| changed("Captured inventory stays in its worker"))
    }
    /// Capture when the approved source provides native immutable epochs.
    /// Unsupported filesystems retain the strict live-source export contract.
    pub fn review_for_preparation(home: &Path, cancel: &AtomicBool) -> io::Result<Arc<Self>> {
        Self::review_preparation(home, &[], cancel, None)
    }
    /// Review with the person's nested source exclusions. The canonical Home
    /// producer applies them to the inventory, which is what the sealed archive,
    /// its verification and the installed import consume.
    ///
    /// # Errors
    /// Refuses unsafe grants, changed sources and exclusions outside the Home.
    pub fn review_selected(
        home: &Path,
        exclusions: &[std::path::PathBuf],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        Self::review_preparation(home, exclusions, cancel, None)
    }
    fn review_preparation(
        home: &Path,
        exclusions: &[std::path::PathBuf],
        cancel: &AtomicBool,
        storage: Option<&dyn Fn() -> io::Result<Arc<super::migration_review::InventoryScratch>>>,
    ) -> io::Result<Arc<Self>> {
        let pin = PinnedDirectory::open_absolute(home)?;
        super::source_tree::validate_exclusions_under(&pin.file, exclusions)?;
        #[cfg(target_os = "linux")]
        super::migration_review::MountedHomeLocations::discover(&pin, home, cancel)?
            .require_single_root(cancel)?;
        // An immutable whole-home epoch cannot honour nested exclusions, so a
        // selected review uses the live inventory that can.
        if exclusions.is_empty()
            && let Some(captured) =
                captured::Captured::review(home, cancel).map_err(source_unavailable)?
        {
            pin.validate()?;
            return Ok(Arc::new(Self {
                original_only: false,
                pin,
                inventory: None,
                captured: Some(captured),
                granted_path: Some(home.to_path_buf()),
                original_roots: None,
                exclusions: Vec::new(),
            }));
        }
        check_cancel(cancel)?;
        match storage {
            Some(storage) => Self::review_live_with_working_storage_selected(
                home,
                exclusions,
                storage()?,
                cancel,
            ),
            None => Self::review_with_cancel_selected(home, exclusions, cancel),
        }
    }
    /// Media preparation may retain permission for a source that will only be
    /// inventoried after boot under its authenticated read-only projection.
    pub fn review_for_media_preparation(home: &Path, cancel: &AtomicBool) -> io::Result<Arc<Self>> {
        Self::review_media_preparation(home, &[], cancel, None)
    }
    /// The same media review with nested exclusions carried into the plan.
    ///
    /// # Errors
    /// Refuses unsafe grants, changed sources and exclusions outside the Home.
    pub fn review_selected_for_media_preparation(
        home: &Path,
        exclusions: &[std::path::PathBuf],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        Self::review_media_preparation(home, exclusions, cancel, None)
    }
    pub fn review_media_with_working_storage(
        home: &Path,
        cancel: &AtomicBool,
        storage: &dyn Fn() -> io::Result<Arc<super::migration_review::InventoryScratch>>,
    ) -> io::Result<Arc<Self>> {
        Self::review_media_preparation(home, &[], cancel, Some(storage))
    }
    /// # Errors
    /// Refuses unsafe grants, unavailable working storage and exclusions
    /// outside the Home.
    pub fn review_selected_media_with_working_storage(
        home: &Path,
        exclusions: &[std::path::PathBuf],
        cancel: &AtomicBool,
        storage: &dyn Fn() -> io::Result<Arc<super::migration_review::InventoryScratch>>,
    ) -> io::Result<Arc<Self>> {
        Self::review_media_preparation(home, exclusions, cancel, Some(storage))
    }
    fn review_media_preparation(
        home: &Path,
        exclusions: &[std::path::PathBuf],
        cancel: &AtomicBool,
        storage: Option<&dyn Fn() -> io::Result<Arc<super::migration_review::InventoryScratch>>>,
    ) -> io::Result<Arc<Self>> {
        check_cancel(cancel)?;
        let pin = PinnedDirectory::open_absolute(home)?;
        super::source_tree::validate_exclusions_under(&pin.file, exclusions)?;
        #[cfg(target_os = "linux")]
        super::migration_review::MountedHomeLocations::discover(&pin, home, cancel)?
            .require_single_root(cancel)?;
        #[cfg(target_os = "linux")]
        {
            let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: a held directory fd and complete writable statfs storage.
            if unsafe { libc::fstatfs(pin.file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful fstatfs initialized the output.
            let filesystem = unsafe { filesystem.assume_init() };
            // Only the canonical original reader's supported filesystem kinds.
            if matches!(
                filesystem.f_type,
                libc::EXT4_SUPER_MAGIC | libc::XFS_SUPER_MAGIC
            ) {
                pin.validate()?;
                check_cancel(cancel)?;
                return Ok(Arc::new(Self {
                    original_only: true,
                    pin,
                    inventory: None,
                    captured: None,
                    granted_path: Some(home.into()),
                    original_roots: None,
                    exclusions: exclusions.to_vec(),
                }));
            }
        }
        // Native immutable Btrfs capture remains the existing copy route.
        pin.validate()?;
        Self::review_preparation(home, exclusions, cancel, storage)
    }
    /// All mounted grants come from the shared platform permission generation.
    /// This retains source scope only; no contents, export or Ready is invented.
    /// # Errors
    /// Refuses mismatched, cancelled or substituted grants, and separately
    /// mounted descendants which have not received their own exact grant.
    pub fn review_granted_original_roots(
        home: &Path,
        roots: Arc<super::GrantedHomeRoots>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        Self::review_granted_original_roots_selected(home, roots, &[], cancel)
    }
    pub fn review_granted_original_roots_selected(
        home: &Path,
        roots: Arc<super::GrantedHomeRoots>,
        exclusions: &[std::path::PathBuf],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        let pin = PinnedDirectory::open_absolute(home)?;
        roots.with_roots(cancel, |granted| {
            use std::os::unix::fs::MetadataExt;
            let primary = granted
                .first()
                .ok_or_else(|| changed("Primary Home grant missing"))?;
            let a = primary.file().metadata()?;
            let b = pin.file.metadata()?;
            if primary.placement() != Path::new("")
                || (a.dev(), a.ino()) != (b.dev(), b.ino())
                || primary.mount_id() != fs_native::snapshot_lease::mount_id(&pin.file)?
            {
                return Err(changed("Mounted grants belong to another primary Home"));
            }
            pin.validate()
        })?;
        let source = Arc::new(Self {
            original_only: true,
            pin,
            inventory: None,
            captured: None,
            granted_path: Some(home.into()),
            original_roots: Some(roots),
            exclusions: exclusions.to_vec(),
        });
        source.validate_original_roots(cancel)?;
        Ok(source)
    }
    fn validate_original_roots(&self, cancel: &AtomicBool) -> io::Result<()> {
        let Some(roots) = &self.original_roots else {
            return Ok(());
        };
        let home = self
            .granted_path
            .as_ref()
            .ok_or_else(|| changed("Original Home grant missing"))?;
        roots.with_roots(cancel, |granted| {
            // The source caller performs this discovery under its actual DAC.
            // A separately approved child cannot silently authorize another
            // mount beneath it; no mounted contents are enumerated on refusal.
            for entry in &granted[1..] {
                if entry.kind() == super::RootKind::Directory {
                    let path = home.join(entry.placement());
                    let pin = PinnedDirectory::open_absolute(&path)?;
                    super::migration_review::MountedHomeLocations::discover(&pin, &path, cancel)?
                        .require_single_root(cancel)?;
                }
            }
            Ok(())
        })
    }
    pub fn original_source_geometry(
        &self,
        plan: &super::migration_handoff::Plan,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<Vec<u8>>> {
        self.validate_original_roots(cancel)?;
        let mut plans = crate::setup::source_geometry::original_source_plans(plan)?;
        if let Some(roots) = &self.original_roots {
            let home = self
                .granted_path
                .as_ref()
                .ok_or_else(|| changed("Original grant missing"))?;
            roots.with_roots(cancel, |granted| {
                let entries = granted[1..]
                    .iter()
                    .map(|root| {
                        let kind = match root.kind() {
                            super::RootKind::Directory => {
                                super::migration_handoff::EntryKind::Directory
                            }
                            super::RootKind::File => super::migration_handoff::EntryKind::File,
                        };
                        super::migration_handoff::capture_mounted_entry(
                            &home.join(root.placement()),
                            root.file(),
                            kind,
                        )
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                super::migration_handoff::append_mounted_geometry(
                    &mut plans,
                    &plan.operation_id,
                    &entries,
                )
            })?;
        }
        Ok(plans)
    }
    pub fn original_only(&self) -> bool {
        self.original_only
    }
    pub fn immutable_capture(&self) -> bool {
        self.captured.is_some()
    }

    /// The nested subtrees this review excludes, if any.
    pub fn exclusions(&self) -> &[std::path::PathBuf] {
        &self.exclusions
    }

    pub fn capture_status(&self) -> &'static str {
        if self.original_only {
            return "awaiting-original-boot";
        }
        self.captured.as_ref().map_or("live", |captured| {
            if captured.available() {
                "ready"
            } else {
                "released"
            }
        })
    }
    pub fn release_capture(&self) {
        if let Some(captured) = &self.captured {
            captured.release();
        }
    }

    /// Confined worker entry point. The native launcher supplies only the
    /// selected captured subtree; original-scope validation belongs to its broker.
    #[cfg(feature = "capture-worker")]
    pub fn review_captured(
        pin: Arc<PinnedDirectory>,
        custody: Option<&super::migration_review::CaptureCustody>,
        scratch: Arc<super::migration_review::InventoryScratch>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        pin.validate()?;
        let inventory =
            SourceInventory::capture_captured_home_with_scratch(&pin, custody, scratch, cancel)?;
        portable_transfer::validate_home_metadata(&pin, &inventory, cancel)?;
        if inventory.summary()?.special_files != 0 {
            return Err(changed(
                "Captured home cannot silently omit FIFO or device files",
            ));
        }
        let source = Arc::new(Self {
            original_only: false,
            pin,
            inventory: Some(inventory),
            captured: None,
            granted_path: None,
            original_roots: None,
            exclusions: Vec::new(),
        });
        source.validate_with_cancel(cancel)?;
        Ok(source)
    }
    /// Original-source read only. Authenticated boot admission and the signed
    /// launcher supply this projection; there is no immutable epoch fallback.
    #[cfg(feature = "capture-worker")]
    pub fn review_original_reader(
        pin: Arc<PinnedDirectory>,
        authority: &super::home_capture::OriginalReadClaim,
        scratch: Arc<super::migration_review::InventoryScratch>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        let roots = authority.validate_reader(&pin.file)?;
        check_cancel(cancel)?;
        let exclusions = authority.exclusion_paths()?;
        let inventory = if let Some(roots) = roots {
            SourceInventory::capture_projected_original_home_with_scratch(
                &pin,
                authority.omissions(),
                scratch,
                roots,
                cancel,
            )?
        } else {
            SourceInventory::capture_original_home_with_scratch(
                &pin,
                authority.omissions(),
                scratch,
                cancel,
            )?
        };
        let inventory = inventory.with_exclusions(&pin, &exclusions)?;
        portable_transfer::validate_home_metadata(&pin, &inventory, cancel)?;
        if inventory.summary()?.special_files != 0 {
            return Err(changed(
                "Original home cannot silently omit FIFO or device files",
            ));
        }
        let source = Arc::new(Self {
            original_only: false,
            pin,
            inventory: Some(inventory),
            captured: None,
            granted_path: None,
            original_roots: None,
            exclusions,
        });
        source.validate_with_cancel(cancel)?;
        authority.validate_reader(&source.pin.file)?;
        Ok(source)
    }
    /// # Errors
    /// Refuses redirected, unreadable or unsupported source entries.
    pub fn review(home: &Path) -> io::Result<Arc<Self>> {
        Self::review_with_cancel(home, &AtomicBool::new(false))
    }
    /// The preparation worker owns and passes this live cancellation token.
    pub fn review_with_cancel(home: &Path, cancel: &AtomicBool) -> io::Result<Arc<Self>> {
        Self::review_with_cancel_selected(home, &[], cancel)
    }
    pub fn review_with_cancel_selected(
        home: &Path,
        exclusions: &[std::path::PathBuf],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        check_cancel(cancel)?;
        let pin = PinnedDirectory::open_absolute(home)?;
        super::source_tree::validate_exclusions_under(&pin.file, exclusions)?;
        #[cfg(target_os = "linux")]
        super::migration_review::MountedHomeLocations::discover(&pin, home, cancel)?
            .require_single_root(cancel)?;
        let inventory = SourceInventory::capture_portable_home(&pin, home)?
            .with_exclusions(&pin, exclusions)?;
        portable_transfer::validate_home_metadata(&pin, &inventory, cancel)?;
        if inventory.summary()?.special_files != 0 {
            return Err(changed(
                "Whole-home export cannot silently omit FIFO or device files",
            ));
        }
        Ok(Arc::new(Self {
            original_only: false,
            pin,
            inventory: Some(inventory),
            captured: None,
            granted_path: Some(home.to_path_buf()),
            original_roots: None,
            exclusions: exclusions.to_vec(),
        }))
    }
    /// An explicitly admitted working namespace is retained with the inventory.
    /// The caller owns its backing grant; TMPDIR and arbitrary serialized paths
    /// never select it. Only existing canonical installer custody may overlap.
    #[cfg(target_os = "linux")]
    pub fn review_live_with_working_storage(
        home: &Path,
        scratch: Arc<super::migration_review::InventoryScratch>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        Self::review_live_with_working_storage_selected(home, &[], scratch, cancel)
    }
    pub fn review_live_with_working_storage_selected(
        home: &Path,
        exclusions: &[std::path::PathBuf],
        scratch: Arc<super::migration_review::InventoryScratch>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        check_cancel(cancel)?;
        let pin = PinnedDirectory::open_absolute(home)?;
        super::source_tree::validate_exclusions_under(&pin.file, exclusions)?;
        super::migration_review::MountedHomeLocations::discover(&pin, home, cancel)?
            .require_single_root(cancel)?;
        let inventory =
            SourceInventory::capture_portable_home_with_storage(&pin, home, scratch, cancel)?
                .with_exclusions(&pin, exclusions)?;
        portable_transfer::validate_home_metadata(&pin, &inventory, cancel)?;
        if inventory.summary()?.special_files != 0 {
            return Err(changed(
                "Whole-home export cannot silently omit FIFO or device files",
            ));
        }
        Ok(Arc::new(Self {
            original_only: false,
            pin,
            inventory: Some(inventory),
            captured: None,
            granted_path: Some(home.into()),
            original_roots: None,
            exclusions: exclusions.to_vec(),
        }))
    }
    /// Renew contents under the original folder grant. A captured source may
    /// never silently downgrade to a live traversal if its provider disappears.
    pub fn review_latest(&self, cancel: &AtomicBool) -> io::Result<Arc<Self>> {
        check_cancel(cancel)?;
        self.validate_original_grant()?;
        let path = self.granted_path.as_ref().ok_or_else(|| {
            source_unavailable(changed("Worker source is not an original folder grant"))
        })?;
        #[cfg(target_os = "linux")]
        let storage = self
            .inventory
            .as_ref()
            .and_then(SourceInventory::working_storage);
        #[cfg(target_os = "linux")]
        let latest = match storage {
            Some(storage) => Self::review_live_with_working_storage_selected(
                path,
                &self.exclusions,
                storage,
                cancel,
            ),
            None => Self::review_preparation(path, &self.exclusions, cancel, None),
        }
        .map_err(source_unavailable)?;
        #[cfg(not(target_os = "linux"))]
        let latest = Self::review_preparation(path, &self.exclusions, cancel, None)
            .map_err(source_unavailable)?;
        self.validate_original_grant()?;
        latest.validate_original_grant()?;
        if !super::nofollow::same_directory_identity(&self.pin.identity, &latest.pin.identity)
            || self.immutable_capture() && !latest.immutable_capture()
        {
            return Err(source_unavailable(changed(
                "Approved source or capture provider changed",
            )));
        }
        Ok(latest)
    }
    fn validate_original_grant(&self) -> io::Result<()> {
        self.pin.validate().map_err(|error| {
            if self.captured.is_some() {
                source_unavailable(error)
            } else {
                error
            }
        })
    }
    /// # Errors
    /// Refuses changes to the granted source identity or reviewed contents.
    pub fn validate(&self) -> io::Result<()> {
        self.validate_with_cancel(&AtomicBool::new(false))
    }
    /// Recheck the retained source while honoring explicit cancellation.
    /// # Errors
    /// Refuses cancelled work, expired captures and changed source authority or contents.
    pub fn validate_with_cancel(&self, cancel: &AtomicBool) -> io::Result<()> {
        check_cancel(cancel)?;
        if self.original_only {
            if self.original_roots.is_some() {
                self.validate_original_roots(cancel)?;
            }
            return self.pin.validate();
        }
        if let Some(captured) = &self.captured {
            self.pin.validate().map_err(source_unavailable)?;
            return captured.validate(cancel);
        }
        self.pin.validate()?;
        self.inventory()?
            .validate_with_cancel(&self.pin, cancel)
            .map_err(|error| {
                if super::migration_review::source_changed(&error) {
                    io::Error::new(error.kind(), ReviewedHomeChanged(error))
                } else {
                    error
                }
            })
    }
    /// # Errors
    /// Refuses overflowing source counts.
    pub fn summary(&self) -> io::Result<ExportSummary> {
        if self.original_only {
            return Err(changed("Original contents have not been inventoried"));
        }
        if let Some(captured) = &self.captured {
            return Ok(captured.summary());
        }
        let summary = self.inventory()?.summary()?;
        Ok(ExportSummary {
            files: summary.files,
            bytes: summary.bytes,
            links: summary.links,
            special_files: summary.special_files,
            runtime_sockets: summary.runtime_sockets,
        })
    }
    pub fn sealed_reservation(&self) -> io::Result<u64> {
        if self.original_only {
            return Err(changed(
                "Original archive budget requires the selected medium",
            ));
        }
        self.validate()?;
        if let Some(captured) = &self.captured {
            return Ok(captured.reservation());
        }
        let entries = u64::try_from(self.entry_count()).map_err(io::Error::other)?;
        // The canonical parser permits one local PAX extension per entry,
        // <=1MiB each, and <=512MiB cumulatively. This is a GLOBAL ceiling.
        let metadata = entries
            .checked_mul(1024 * 1024)
            .ok_or_else(|| changed("Archive reservation overflows"))?
            .min(512 * 1024 * 1024);
        // Each normal entry and its optional PAX record need a 512-byte header
        // and at most 511 bytes of padding; final GNU tar blocking adds 10KiB.
        let plaintext_bytes = super::portable_transfer::stored_bytes(&self.pin, self.inventory()?)?
            .checked_add(
                entries
                    .checked_mul(2048)
                    .ok_or_else(|| changed("Archive reservation overflows"))?,
            )
            .and_then(|bytes| bytes.checked_add(metadata))
            .and_then(|bytes| bytes.checked_add(10 * 1024))
            .ok_or_else(|| changed("Archive reservation overflows"))?;
        Limits {
            plaintext_bytes,
            entries: self.entry_count(),
            metadata_bytes: 1,
        }
        .sealed_bound()
        .ok_or_else(|| changed("Archive reservation overflows"))
    }
    /// Approximate archive footprint, including entry headers and block padding.
    /// Extended attributes can increase it; actual filesystem bounds govern export.
    /// # Errors
    /// Refuses an overflowing estimate.
    pub fn archive_estimate(&self) -> io::Result<u64> {
        let entries = u64::try_from(self.entry_count()).map_err(io::Error::other)?;
        super::portable_transfer::stored_bytes(&self.pin, self.inventory()?)?
            .checked_add(
                entries
                    .checked_mul(1536)
                    .ok_or_else(|| changed("Archive estimate overflows"))?,
            )
            .and_then(|bytes| bytes.checked_add(10 * 1024))
            .ok_or_else(|| changed("Archive estimate overflows"))
    }
    /// Renew a completed package after a later setup-delivery failure without
    /// replacing custody or encrypting another copy.
    /// # Errors
    /// Refuses changed grants, source, ciphertext, password or authenticated binding.
    pub fn validate_existing(
        &self,
        media: &MediaGrant,
        expected: &Expectation,
        setup_binding: &str,
        password: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.validate_existing_reporting(
            media,
            expected,
            setup_binding,
            password,
            cancel,
            &mut |_| Ok(()),
        )
    }
    pub fn validate_existing_reporting(
        &self,
        media: &MediaGrant,
        expected: &Expectation,
        setup_binding: &str,
        password: &[u8],
        cancel: &AtomicBool,
        observer: &mut portable_transfer::Observer<'_>,
    ) -> io::Result<()> {
        expected.validate()?;
        if self.original_only {
            return Err(changed(
                "Original-only permission does not authorize a live archive",
            ));
        }
        self.validate_original_grant()?;
        if let Some(captured) = &self.captured {
            return captured.validate_existing_reporting(
                media,
                expected,
                setup_binding,
                password,
                cancel,
                observer,
            );
        }
        self.validate()?;
        media.pin.validate_destination()?;
        if expected.setup_binding != setup_binding {
            return Err(changed(
                "Portable home differs from the prepared setup binding",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        options.nonblock(true);
        let mut file =
            cap_primitives::fs::open(&media.pin.file, Path::new(&expected.filename), &options)?;
        if !file.metadata()?.is_file()
            || file.metadata()?.len() != expected.sealed_bytes
            || ciphertext_digest(&mut file, cancel)? != expected.ciphertext_sha256
        {
            return Err(changed("Completed portable home ciphertext changed"));
        }
        let info = portable_transfer::verify_export_stream_reporting(
            &self.pin,
            self.inventory()?,
            &mut file,
            password,
            expected.limits,
            cancel,
            observer,
        )?;
        if info.purpose != Purpose::Home
            || info.setup_binding.as_deref() != Some(setup_binding)
            || info.sealed_bytes != expected.sealed_bytes
            || info.plaintext_bytes != expected.plaintext_bytes
            || info.entries != expected.entries
            || info.runtime_sockets != expected.runtime_sockets
            || ciphertext_digest(&mut file, cancel)? != expected.ciphertext_sha256
        {
            return Err(changed(
                "Completed portable home differs from its authenticated expectation",
            ));
        }
        let current =
            cap_primitives::fs::open(&media.pin.file, Path::new(&expected.filename), &options)?;
        if durable::identity(&current)? != durable::identity(&file)? {
            return Err(changed("Completed portable home publication was replaced"));
        }
        media.pin.validate_destination()?;
        self.validate()
    }
    /// Stream tar directly into sealed media storage, then verify against this
    /// exact reviewed source and atomically publish. Source changes need review.
    /// # Errors
    /// Refuses changed sources, inadequate storage, cancellation or publication collisions.
    pub fn export_to(
        &self,
        media: &MediaGrant,
        operation: &str,
        setup_binding: &str,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<Expectation> {
        self.export_to_reporting(
            media,
            operation,
            setup_binding,
            password,
            cancel,
            &mut |_| Ok(()),
        )
    }
    pub fn export_to_reporting(
        &self,
        media: &MediaGrant,
        operation: &str,
        setup_binding: &str,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
        observer: &mut portable_transfer::Observer<'_>,
    ) -> io::Result<Expectation> {
        check_cancel(cancel)?;
        if self.original_only {
            return Err(changed(
                "Original-only permission does not authorize a live archive",
            ));
        }
        self.validate_original_grant()?;
        if media.pin.contains_identity(&self.pin.identity) {
            return Err(changed("Media destination overlaps original approved home"));
        }
        if let Some(captured) = &self.captured {
            return captured.export_reporting(
                media,
                operation,
                setup_binding,
                password,
                cancel,
                observer,
            );
        }
        self.validate()?;
        let filename = filename(operation)?;
        if setup_binding.is_empty() || setup_binding.len() > 256 {
            return Err(changed(
                "Portable home requires the exact prepared setup binding",
            ));
        }
        media.pin.validate_destination()?;
        if media.pin.contains_identity(&self.pin.identity) {
            return Err(changed(
                "Media destination overlaps the reviewed source home",
            ));
        }
        match metadata_nofollow(&media.pin.file, Path::new(&filename)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(changed(
                    "Portable home package already exists; it was not replaced",
                ));
            }
        }
        let limits = media.limits()?;
        // Inventory sizes can count multiple names of the same hardlink.
        // The actual bounded archive stream decides whether storage suffices.
        let mut temporary = Temporary::create(&media.pin)?;
        let info = portable_transfer::export_stream_reporting(
            &self.pin,
            self.inventory()?,
            Purpose::Home,
            limits,
            &mut temporary.file,
            &mut portable_transfer::ExportWork {
                password: &password,
                cancel,
                setup_binding: Some(setup_binding),
                observer,
            },
        )
        .or_else(|error| {
            self.validate()?;
            Err(error)
        })?;
        self.validate()?;
        if info.sealed_bytes
            > limits
                .sealed_bound()
                .ok_or_else(|| changed("Invalid media limits"))?
        {
            return Err(changed("Portable home exceeds granted media allocation"));
        }
        let digest = ciphertext_digest(&mut temporary.file, cancel)?;
        self.validate()?;
        temporary.validate()?;
        check_cancel(cancel)?;
        let expectation = Expectation {
            schema: 1,
            operation: operation.into(),
            purpose: Purpose::Home,
            filename,
            setup_binding: setup_binding.into(),
            sealed_bytes: info.sealed_bytes,
            plaintext_bytes: info.plaintext_bytes,
            entries: info.entries,
            runtime_sockets: info.runtime_sockets,
            ciphertext_sha256: digest,
            limits,
        };
        expectation.validate()?;
        temporary.publish(&expectation.filename)?;
        Ok(expectation)
    }
}
/// Held filesystem grant, renewed before writing. It grants no other volumes.
pub struct MediaGrant {
    pin: Arc<PinnedDirectory>,
    sealed_reservation: Option<u64>,
}
/// Inspect limits on an already pinned private destination without exporting
/// a media grant or any elevated filesystem authority.
pub fn reviewed_directory_limits(pin: Arc<PinnedDirectory>) -> io::Result<Limits> {
    MediaGrant::from_reviewed_pin(pin)?.limits()
}
impl MediaGrant {
    /// Clone only the already held and re-attested selected output directory.
    /// The broker receives this capability, never an ambient path to reopen.
    pub fn clone_reviewed_directory(&self) -> io::Result<File> {
        self.pin.validate_destination()?;
        self.limits()?;
        let file = self.pin.file.try_clone()?;
        self.pin.validate_destination()?;
        Ok(file)
    }
    pub(in crate::setup) fn from_reviewed_pin(pin: Arc<PinnedDirectory>) -> io::Result<Self> {
        pin.validate_destination()?;
        let grant = Self {
            pin,
            sealed_reservation: None,
        };
        grant.limits()?;
        Ok(grant)
    }
    /// # Errors
    /// Refuses redirected, readonly or unavailable media grants.
    pub fn open(directory: &Path) -> io::Result<Self> {
        let pin = PinnedDirectory::open_absolute(directory)?;
        pin.validate_destination()?;
        let result = Self {
            pin,
            sealed_reservation: None,
        };
        result.limits()?;
        Ok(result)
    }
    #[cfg(any(feature = "capture-worker", test))]
    pub fn limit_original_reservation(mut self, bound: u64) -> io::Result<Self> {
        if bound == 0 {
            return Err(changed("Original reservation missing"));
        }
        self.sealed_reservation = Some(bound);
        self.limits()?;
        Ok(self)
    }
    /// Publish only bounded metadata after the caller's verified export. An
    /// existing identical receipt is reusable; another receipt is never replaced.
    /// # Errors
    /// Refuses changed ciphertext, mismatching existing receipts or publication failure.
    pub fn publish_expectation(
        &self,
        expected: &Expectation,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        expected.validate()?;
        check_cancel(cancel)?;
        self.pin.validate_destination()?;
        let receipt = format!("{}.receipt.json", expected.operation);
        let bytes = serde_json::to_vec(expected)?;
        if bytes.len() > 16 * 1024 {
            return Err(changed("Home receipt exceeds metadata bounds"));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        options.nonblock(true);
        let mut package =
            cap_primitives::fs::open(&self.pin.file, Path::new(&expected.filename), &options)?;
        if !package.metadata()?.is_file()
            || package.metadata()?.len() != expected.sealed_bytes
            || ciphertext_digest(&mut package, cancel)? != expected.ciphertext_sha256
        {
            return Err(changed("Home receipt cannot authorize changed ciphertext"));
        }
        match cap_primitives::fs::open(&self.pin.file, Path::new(&receipt), &options) {
            Ok(file) => {
                if !file.metadata()?.is_file() || file.metadata()?.len() > 16 * 1024 {
                    return Err(changed("Existing home receipt is invalid"));
                }
                let mut actual = Vec::new();
                file.take(16 * 1024 + 1).read_to_end(&mut actual)?;
                if actual != bytes {
                    return Err(changed(
                        "Existing home receipt differs; it was not replaced",
                    ));
                }
                self.pin.validate_destination()?;
                check_cancel(cancel)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut temporary = Temporary::create(&self.pin)?;
                temporary.file.write_all(&bytes)?;
                check_cancel(cancel)?;
                temporary.publish(&receipt)
            }
            Err(error) => Err(error),
        }
    }
    /// Publish the exact completed home/setup pairing for installation transport.
    /// This public receipt locates ciphertext; it grants no identity authority.
    /// # Errors
    /// Refuses redirected directories, differing existing selections or failed publication.
    pub fn publish_setup_selection(
        &self,
        setup: &super::media_setup::DeliveryReceipt,
        home: &Expectation,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        home.validate()?;
        check_cancel(cancel)?;
        self.pin.validate_destination()?;
        let metadata = metadata_nofollow(&self.pin.file, Path::new(super::media_setup::DIRECTORY))?;
        let directory = self.pin.open_child(
            std::ffi::OsStr::new(super::media_setup::DIRECTORY),
            &metadata,
        )?;
        directory.validate_destination()?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema":1,"prepared_setup":setup.expectation(),"home":home
        }))?;
        if bytes.len() > 18 * 1024 {
            return Err(changed("Home selection exceeds metadata bounds"));
        }
        let name = "home-selection.json";
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        match cap_primitives::fs::open(&directory.file, Path::new(name), &options) {
            Ok(file) => {
                if !file.metadata()?.is_file() || file.metadata()?.len() > 18 * 1024 {
                    return Err(changed("Existing home selection is invalid"));
                }
                let mut existing = Vec::new();
                file.take(18 * 1024 + 1).read_to_end(&mut existing)?;
                if existing != bytes {
                    return Err(changed("Existing home selection differs"));
                }
                directory.validate_destination()?;
                check_cancel(cancel)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut staged = Temporary::create(&directory)?;
                staged.file.write_all(&bytes)?;
                check_cancel(cancel)?;
                staged.publish(name)
            }
            Err(error) => Err(error),
        }
    }
    /// # Errors
    /// Refuses changed grants, readonly media and insufficient free capacity.
    pub fn limits(&self) -> io::Result<Limits> {
        self.pin.validate_destination()?;
        let free = capacity(&self.pin.file)?;
        let available = free
            .checked_sub(RESERVE)
            .ok_or_else(|| changed("Granted media has no space for transfer metadata"))?;
        let cap = self.sealed_reservation.unwrap_or(available).min(available);
        let low = payload_capacity_for_free_bytes(
            cap.checked_add(RESERVE)
                .ok_or_else(|| changed("Original reservation overflows"))?,
        )?;
        if low == 0 {
            return Err(changed("Granted media cannot hold a portable package"));
        }
        Ok(Limits {
            plaintext_bytes: low,
            entries: usize::try_from((cap / 512).clamp(1, 1_000_000)).map_err(io::Error::other)?,
            metadata_bytes: usize::try_from((cap / 4).clamp(1, 512 * 1024 * 1024))
                .map_err(io::Error::other)?,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    pub schema: u32,
    pub operation: String,
    pub purpose: Purpose,
    pub filename: String,
    pub setup_binding: String,
    pub sealed_bytes: u64,
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub runtime_sockets: u64,
    pub ciphertext_sha256: [u8; 32],
    pub limits: Limits,
}
impl Expectation {
    /// # Errors
    /// Refuses invalid operations, purposes, bindings and storage bounds.
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || self.purpose != Purpose::Home
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.filename != filename(&self.operation)?
            || self.entries == 0
            || self.entries > self.limits.entries
            || self.runtime_sockets > self.limits.entries as u64
            || self.plaintext_bytes == 0
            || self.plaintext_bytes > self.limits.plaintext_bytes
            || self.sealed_bytes == 0
            || self
                .limits
                .sealed_bound()
                .is_none_or(|bound| self.sealed_bytes > bound)
            || self.limits.entries > 1_000_000
            || self.limits.metadata_bytes == 0
            || self.limits.metadata_bytes > 512 * 1024 * 1024
        {
            return Err(changed(
                "Portable home expectation has invalid bounds or operation binding",
            ));
        }
        Ok(())
    }
}
fn filename(operation: &str) -> io::Result<String> {
    if operation.is_empty()
        || operation.len() > 96
        || !operation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(changed("Portable home operation is invalid"));
    }
    Ok(format!("home-{operation}.sealed"))
}
fn capacity(file: &File) -> io::Result<u64> {
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: live fd and writable complete native stat storage.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized storage.
    let stat = unsafe { stat.assume_init() };
    if stat.f_flag & libc::ST_RDONLY != 0 {
        return Err(changed("Granted media filesystem is readonly"));
    }
    let available = stat
        .f_bavail
        .checked_mul(stat.f_frsize)
        .ok_or_else(|| changed("Media free capacity overflows"))?;
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: live fd and writable complete native stat storage.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatfs initialized storage.
    let filesystem = unsafe { filesystem.assume_init() };
    // FAT cannot store one file larger than 4 GiB; large homes need the actual
    // separately provisioned data filesystem, never an EFI-only claim.
    Ok(if filesystem.f_type == 0x4d44 {
        available.min(u64::from(u32::MAX))
    } else {
        available
    })
}
fn ciphertext_digest(file: &mut File, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(
            buffer
                .get(..count)
                .ok_or_else(|| changed("Invalid digest bound"))?,
        );
    }
    Ok(digest.finalize().into())
}
struct Temporary {
    parent: Arc<PinnedDirectory>,
    name: String,
    file: File,
    identity: durable::Identity,
}
impl Temporary {
    fn create(parent: &Arc<PinnedDirectory>) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No)
            .mode(0o600);
        for _ in 0..128 {
            let name = format!(
                ".tilo-home-{}-{}.partial",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            match cap_primitives::fs::open(&parent.file, Path::new(&name), &options) {
                Ok(file) => {
                    let identity = durable::identity(&file)?;
                    return Ok(Self {
                        parent: Arc::clone(parent),
                        name,
                        file,
                        identity,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
        }
        Err(changed(
            "Cannot exclusively reserve ciphertext media storage",
        ))
    }
    fn validate(&self) -> io::Result<()> {
        self.parent.validate_destination()?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        options.nonblock(true);
        let file = cap_primitives::fs::open(&self.parent.file, Path::new(&self.name), &options)?;
        if !file.metadata()?.is_file() {
            return Err(changed("Ciphertext staging is not a regular file"));
        }
        if durable::identity(&file)? != self.identity
            || durable::identity(&self.file)? != self.identity
        {
            return Err(changed("Ciphertext staging inode was replaced"));
        }
        Ok(())
    }
    fn publish(&mut self, filename: &str) -> io::Result<()> {
        self.validate()?;
        durable::sync_file(&self.file)?;
        let from = CString::new(self.name.as_bytes()).map_err(io::Error::other)?;
        let to = CString::new(filename.as_bytes()).map_err(io::Error::other)?;
        // SAFETY: live same granted directory fd and terminated single names.
        if unsafe {
            libc::renameat2(
                self.parent.file.as_raw_fd(),
                from.as_ptr(),
                self.parent.file.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        self.name = filename.into();
        durable::sync_directory(&self.parent.file)?;
        // Successful publication transfers cleanup authority to the caller.
        self.name.clear();
        Ok(())
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.name.is_empty() && self.validate().is_ok() {
            let _ = cap_primitives::fs::remove_file(&self.parent.file, Path::new(&self.name));
            let _ = durable::sync_directory(&self.parent.file);
        }
    }
}
#[cfg(test)]
mod tests;

/// Shared prewrite/postformat payload bound; includes sealed framing and reserve.
pub fn payload_capacity_for_free_bytes(free: u64) -> io::Result<u64> {
    let available = free
        .checked_sub(RESERVE)
        .ok_or_else(|| changed("Media lacks transfer metadata reserve"))?;
    let mut low = 0u64;
    let mut high = available;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let limits = Limits {
            plaintext_bytes: middle,
            entries: 1,
            metadata_bytes: 1,
        };
        if limits
            .sealed_bound()
            .is_some_and(|bound| bound <= available)
        {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    if low == 0 {
        return Err(changed("Media cannot hold a portable package"));
    }
    Ok(low)
}

#[cfg(test)]
mod original_budget_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn authenticated_reservation_bounds_actual_archive_before_publication() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.path().join("source");
        let media = root.path().join("media");
        for path in [&source, &media] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &source);
        environment.set("XDG_STATE_HOME", source.join(".local/state"));
        let document = vec![73u8; 128 * 1024];
        std::fs::write(source.join("document"), &document).unwrap();
        let selected = HomeSource::review(&source).unwrap();
        let grant = MediaGrant::open(&media)
            .unwrap()
            .limit_original_reservation(32 * 1024)
            .unwrap();
        assert!(grant.limits().unwrap().sealed_bound().unwrap() <= 32 * 1024);
        assert!(
            selected
                .export_to(
                    &grant,
                    "bounded",
                    "setup-binding",
                    Zeroizing::new(b"synthetic-password".to_vec()),
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert!(!media.join("home-bounded.sealed").exists());
        assert_eq!(std::fs::read_dir(&media).unwrap().count(), 0);
        assert_eq!(std::fs::read(source.join("document")).unwrap(), document);
        // The same production sink succeeds with sufficient admitted budget.
        let grant = MediaGrant::open(&media)
            .unwrap()
            .limit_original_reservation(2 * 1024 * 1024)
            .unwrap();
        let expected = selected
            .export_to(
                &grant,
                "bounded",
                "setup-binding",
                Zeroizing::new(b"synthetic-password".to_vec()),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(expected.sealed_bytes <= 2 * 1024 * 1024);
        selected
            .validate_existing_reporting(
                &grant,
                &expected,
                "setup-binding",
                b"synthetic-password",
                &AtomicBool::new(false),
                &mut |_| Ok(()),
            )
            .unwrap();
    }
}
