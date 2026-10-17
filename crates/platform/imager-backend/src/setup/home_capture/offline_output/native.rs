//! Root-controller selected medium custody. No formatting or mount adoption.
//! An uncertain worker exit leaves the owned mount quarantined, never released.
mod boot;
use super::*;
use crate::setup::migration::PinnedDirectory;
use block_devices::{BlockDevice, Lsblk, udisks::Udisks};
pub use boot::{BootMedium, with_boot_medium};
use std::{ffi::OsStr, io::Read, os::fd::AsRawFd, path::PathBuf};
use tilo_files::native::atomic::{Privacy, StorageDirectory};
const COLUMNS: &str =
    "NAME,TYPE,MAJ:MIN,SIZE,START,PTUUID,PARTUUID,PARTTYPE,FSTYPE,UUID,RO,MOUNTPOINTS";

#[derive(Clone, PartialEq, Eq)]
struct Partition {
    name: String,
    number: String,
    uuid: String,
    mount: Option<PathBuf>,
}
fn selected(
    devices: &[BlockDevice],
    disk: &File,
    binding: &MediaPartitionBinding,
) -> io::Result<Partition> {
    let number = super::number(disk)?;
    let disks = devices
        .iter()
        .filter(|d| d.kind == "disk" && !d.read_only && d.maj_min.as_deref() == Some(&number))
        .collect::<Vec<_>>();
    let [disk] = disks.as_slice() else {
        return Err(refused("Original output disk is missing or ambiguous"));
    };
    let parts = disk
        .children
        .iter()
        .filter(|p| {
            p.partuuid
                .as_deref()
                .is_some_and(|v| v.eq_ignore_ascii_case(&binding.partition_guid))
        })
        .collect::<Vec<_>>();
    let [part] = parts.as_slice() else {
        return Err(refused("Original output partition is missing or ambiguous"));
    };
    let size = binding
        .last_lba
        .checked_sub(binding.first_lba)
        .and_then(|v| v.checked_add(1))
        .and_then(|v| v.checked_mul(binding.sector_bytes))
        .ok_or_else(|| refused("Original output geometry overflow"))?;
    if disk
        .ptuuid
        .as_deref()
        .is_none_or(|v| !v.eq_ignore_ascii_case(&binding.disk_guid))
        || binding.sector_bytes != 512
        || part.kind != "part"
        || part.read_only
        || !part.children.is_empty()
        || part.start != Some(binding.first_lba)
        || part.size != size
        || part.fstype.as_deref() != Some("ext4")
        || part
            .parttype
            .as_deref()
            .is_none_or(|v| !v.eq_ignore_ascii_case(crate::media_layout::DATA_PARTITION_TYPE))
    {
        return Err(refused(
            "Original output requires the exact formatted data partition",
        ));
    }
    let points = part.all_mountpoints();
    let mount = match points.as_slice() {
        [] => None,
        [one] if Path::new(one).is_absolute() => Some(PathBuf::from(one)),
        _ => return Err(refused("Original output mount is ambiguous")),
    };
    Ok(Partition {
        name: part.name.clone(),
        number: part
            .maj_min
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| refused("Original partition number missing"))?,
        uuid: part
            .uuid
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| refused("Original filesystem UUID missing"))?,
        mount,
    })
}
fn inventory() -> io::Result<Vec<BlockDevice>> {
    Lsblk::new(COLUMNS)
        .full_paths(true)
        .list()
        .ok_or_else(|| refused("Original output inventory unavailable"))
}
fn mount_id(file: &File) -> io::Result<u64> {
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID,
            &raw mut stat,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 {
        return Err(refused("Original output mount identity unavailable"));
    }
    Ok(stat.stx_mnt_id)
}
/// Count every alias, including read-only aliases: no foreign mount is adopted.
fn unique_mount(number: &str, id: u64) -> io::Result<()> {
    let mut bytes = Vec::new();
    File::open("/proc/self/mountinfo")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(refused("Original output mount inventory exceeds bounds"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| refused("Original output mount inventory invalid"))?;
    let mut found = 0usize;
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let current = fields
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| refused("Original output mount inventory invalid"))?;
        let _parent = fields
            .next()
            .ok_or_else(|| refused("Original output mount inventory invalid"))?;
        let device = fields
            .next()
            .ok_or_else(|| refused("Original output mount inventory invalid"))?;
        if device == number {
            if current != id {
                return Err(refused("Original output has a foreign mount alias"));
            }
            found += 1;
        }
    }
    if found != 1 {
        return Err(refused("Original output mount is not uniquely held"));
    }
    Ok(())
}
#[derive(serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Witness {
    schema: u32,
    binding: MediaPartitionBinding,
    filesystem_uuid: String,
}
fn witness(
    pin: &Arc<PinnedDirectory>,
    binding: &MediaPartitionBinding,
    uuid: &str,
) -> io::Result<Arc<PinnedDirectory>> {
    let metadata = crate::setup::nofollow::metadata_nofollow(
        &pin.file,
        Path::new(crate::setup::media_setup::DIRECTORY),
    )?;
    let private = pin.open_child(OsStr::new(crate::setup::media_setup::DIRECTORY), &metadata)?;
    let storage = StorageDirectory::from_file(
        crate::setup::migration_durability::readable_directory(&private.file)?,
    )?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    let file = storage.open_read(OsStr::new("filesystem-owner.json"), Privacy::OwnerOnly)?;
    if file.metadata()?.len() > 4096 {
        return Err(refused("Original output filesystem witness exceeds bounds"));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    let actual: Witness = serde_json::from_slice(&bytes)
        .map_err(|_| refused("Original output filesystem witness invalid"))?;
    if actual
        != (Witness {
            schema: 1,
            binding: binding.clone(),
            filesystem_uuid: uuid.into(),
        })
    {
        return Err(refused("Original output filesystem witness differs"));
    }
    Ok(private)
}
pub(in crate::setup::home_capture) fn review_filesystem_witness(
    private: &Arc<PinnedDirectory>,
    binding: &MediaPartitionBinding,
) -> io::Result<[u8; 16]> {
    private.validate_destination()?;
    let storage = StorageDirectory::from_file(
        crate::setup::migration_durability::readable_directory(&private.file)?,
    )?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    let file = storage.open_read(OsStr::new("filesystem-owner.json"), Privacy::OwnerOnly)?;
    if file.metadata()?.len() > 4096 {
        return Err(refused("Filesystem witness exceeds bounds"));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(refused("Filesystem witness exceeds bounds"));
    }
    let actual: Witness =
        serde_json::from_slice(&bytes).map_err(|_| refused("Filesystem witness invalid"))?;
    if actual.schema != 1 || &actual.binding != binding {
        return Err(refused("Filesystem witness differs"));
    }
    let uuid = uuid::Uuid::parse_str(&actual.filesystem_uuid)
        .map_err(|_| refused("Filesystem witness UUID invalid"))?;
    private.validate_destination()?;
    Ok(*uuid.as_bytes())
}
/// Cleanup guard only for construction, before any worker capability exists.
struct MountBuilding {
    daemon: Option<Udisks>,
    partition: String,
}
impl Drop for MountBuilding {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.unmount(&self.partition);
        }
    }
}
pub struct NativeMediumOutput {
    read: Arc<OfflineReadAdmission>,
    daemon: Udisks,
    disk: File,
    partition: File,
    root: Arc<PinnedDirectory>,
    private: Arc<PinnedDirectory>,
    output: File,
    selected: Partition,
    media: MediaPartitionBinding,
    binding: OriginalOutputBinding,
    cancellation: ServiceCancellation,
    inventory_scratch: Mutex<Option<super::inventory_scratch::MediumScratch>>,
}
impl NativeMediumOutput {
    /// Root-only caller obtains all facts from retained authenticated admission.
    /// No frontend mountpoint, selected partition path or deserialized grant.
    pub(super) fn mount(
        read: Arc<OfflineReadAdmission>,
        cancellation: ServiceCancellation,
    ) -> io::Result<(Arc<Self>, Store)> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused("Original output mount requires root controller"));
        }
        let cancel = AtomicBool::new(false);
        let plan = read.output_selection_declaration(&cancel)?;
        let disk = read.output_raw_medium(&cancel)?;
        // Refuse overlap before mounting or creating any target namespace.
        crate::setup::migration_handoff::validate_offline_source_disks(
            &read.output_source_plans(&cancel)?,
            disk.metadata()?.rdev(),
        )?;
        let before = selected(&inventory()?, &disk, plan.media())?;
        if before.mount.is_some() {
            return Err(refused("Original output will not adopt an existing mount"));
        }
        let daemon =
            Udisks::system().map_err(|_| refused("Original output mount service unavailable"))?;
        // The path is an exact current direct child of the held authenticated disk;
        // actual returned FD and superblock are checked independently below.
        let partition = daemon
            .open_read_only(&before.name)
            .map_err(|_| refused("Original output partition custody refused"))?;
        if super::number(&partition)? != before.number {
            return Err(refused("Original output partition descriptor differs"));
        }
        read.check_output_medium(&disk, &cancel)?;
        let path = PathBuf::from(
            daemon
                .mount_migration(&before.name)
                .map_err(|_| refused("Original output mount refused"))?,
        );
        let mut building = MountBuilding {
            daemon: Some(daemon),
            partition: before.name.clone(),
        };
        let mut expected = before.clone();
        expected.mount = Some(path.clone());
        if selected(&inventory()?, &disk, plan.media())? != expected {
            return Err(refused("Original output mounted partition changed"));
        }
        let root = PinnedDirectory::open_absolute(&path)?;
        let root_meta = root.file.metadata()?;
        if root_meta.uid() != 0
            || root_meta.dev() != partition.metadata()?.rdev()
            || root_meta.ino() != 2
        {
            return Err(refused(
                "Original output mounted filesystem identity differs",
            ));
        }
        let id = mount_id(&root.file)?;
        unique_mount(&before.number, id)?;
        let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(root.file.as_raw_fd(), &raw mut fs) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let safety = libc::ST_NODEV | libc::ST_NOSUID | libc::ST_NOEXEC;
        if fs.f_flag & safety != safety || fs.f_flag & libc::ST_RDONLY != 0 {
            return Err(refused("Original output mount flags differ"));
        }
        // Existing witness MUST be root private; no repair/chown/format fallback.
        let private = witness(&root, plan.media(), &before.uuid)?;
        let store_pin = match crate::setup::nofollow::metadata_nofollow(
            &private.file,
            Path::new("original-output"),
        ) {
            Ok(meta) => private.open_child(OsStr::new("original-output"), &meta)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                private.create_child(OsStr::new("original-output"))?
            }
            Err(e) => return Err(e),
        };
        let store = Store::open(store_pin.clone(), &plan.source().operation_id)?;
        let fs_uuid = uuid::Uuid::parse_str(&before.uuid)
            .map_err(|_| refused("Original filesystem UUID invalid"))?
            .into_bytes();
        use sha2::Digest;
        let stable = plan.canonical_digest()?;
        let stage_binding = super::super::store::StageBinding {
            declaration: stable,
            filesystem: fs_uuid,
            uid: plan.requested_caller().uid(),
            gid: plan.requested_caller().primary_gid(),
        };
        let (attempt, output) = if plan.is_applications_only() {
            store.prepare_original_applications_output_stage(&store_pin, stage_binding)?
        } else {
            store.prepare_original_output_stage(&store_pin, stage_binding)?
        };
        let output_meta = output.metadata()?;
        let binding = OriginalOutputBinding {
            read_claim_digest: read.output_read_claim_digest(&cancel)?,
            medium_claim_digest: sha2::Sha256::digest(serde_json::to_vec(plan.media())?).into(),
            operation: plan.source().operation_id.clone(),
            attempt,
            source: {
                let m = read.output_original_home(&cancel)?.metadata()?;
                (m.dev(), m.ino())
            },
            output: (output_meta.dev(), output_meta.ino()),
            original_uid: plan.requested_caller().uid(),
            disk_rdev: disk.metadata()?.rdev(),
            partition_rdev: partition.metadata()?.rdev(),
            mount_id: id,
            mount_root: (root_meta.dev(), root_meta.ino()),
            filesystem_uuid: fs_uuid,
            reserve_bytes: plan.sealed_reservation(),
        };
        let daemon = building
            .daemon
            .take()
            .ok_or_else(|| refused("Original output mount lease missing"))?;
        let medium = Arc::new(Self {
            read,
            daemon,
            disk,
            partition,
            root,
            private,
            output,
            selected: expected,
            media: plan.media().clone(),
            binding,
            cancellation,
            inventory_scratch: Mutex::new(None),
        });
        medium.check()?;
        Ok((medium, store))
    }
    fn check_mount_custody(&self) -> io::Result<()> {
        self.read.check_output_medium_for_cleanup(&self.disk)?;
        if selected(&inventory()?, &self.disk, &self.media)? != self.selected {
            return Err(refused("Original output mount custody changed"));
        }
        self.root.validate_destination()?;
        self.private.validate_destination()?;
        witness(&self.root, &self.media, &self.selected.uuid)?;
        if mount_id(&self.root.file)? != self.binding.mount_id
            || mount_id(&self.output)? != self.binding.mount_id
        {
            return Err(refused("Original output mount identity changed"));
        }
        unique_mount(&self.selected.number, self.binding.mount_id)
    }
    fn output_directory(&self) -> io::Result<Arc<PinnedDirectory>> {
        self.check_mount_custody()?;
        let metadata = crate::setup::nofollow::metadata_nofollow(
            &self.private.file,
            Path::new("original-output"),
        )?;
        let parent = self
            .private
            .open_child(OsStr::new("original-output"), &metadata)?;
        let name = format!("stage-{}", self.binding.attempt);
        let metadata = crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(&name))?;
        let pin = parent.open_child(OsStr::new(&name), &metadata)?;
        let actual = pin.file.metadata()?;
        if (actual.dev(), actual.ino()) != self.binding.output
            || actual.uid() != 0
            || actual.gid() != 0
            || actual.mode() & 0o7777 != 0o700
            || mount_id(&pin.file)? != self.binding.mount_id
        {
            return Err(refused("Verified original output directory differs"));
        }
        pin.validate_destination()?;
        self.check_mount_custody()?;
        Ok(pin)
    }
    fn completed_directory(
        &self,
        store: &Store,
        expected: &crate::setup::portable_home_media::Expectation,
    ) -> io::Result<Arc<PinnedDirectory>> {
        self.check()?;
        let output = self.output_directory()?;
        let plan = self.read.output_declaration(&AtomicBool::new(false))?;
        use sha2::Digest;
        let binding = super::super::store::StageBinding {
            declaration: sha2::Sha256::digest(serde_json::to_vec(&plan)?).into(),
            filesystem: self.binding.filesystem_uuid,
            uid: plan.requested_caller().uid(),
            gid: plan.requested_caller().primary_gid(),
        };
        store.check_verified_original_output(
            &self.binding.attempt,
            &output.file,
            expected,
            &binding,
        )?;
        let metadata = crate::setup::nofollow::metadata_nofollow(
            &self.private.file,
            Path::new("original-output"),
        )?;
        let parent = self
            .private
            .open_child(OsStr::new("original-output"), &metadata)?;
        let pin = store.completed_original_setup_directory(&parent, &self.binding.attempt)?;
        if mount_id(&pin.file)? != self.binding.mount_id {
            return Err(refused("Completed setup generation mount changed"));
        }
        self.check()?;
        Ok(pin)
    }
    fn completed_directory_paired(
        &self,
        store: &Store,
        expected: &super::super::store::OriginalOutputExpectations<'_>,
    ) -> io::Result<Arc<PinnedDirectory>> {
        self.check()?;
        expected.validate()?;
        let cancel = AtomicBool::new(self.cancellation.is_cancelled());
        if expected.reserve_bytes != self.binding.reserve_bytes
            || self.read.output_flatpak_required(&cancel)? != expected.applications.is_some()
            || self.read.output_flatpak_operation(&cancel)?.as_deref()
                != expected.applications.map(|apps| apps.operation.as_str())
        {
            return Err(refused("Completed setup paired selection differs"));
        }
        let output = self.output_directory()?;
        let plan = self.read.output_declaration(&AtomicBool::new(false))?;
        use sha2::Digest;
        let binding = super::super::store::StageBinding {
            declaration: sha2::Sha256::digest(serde_json::to_vec(&plan)?).into(),
            filesystem: self.binding.filesystem_uuid,
            uid: plan.requested_caller().uid(),
            gid: plan.requested_caller().primary_gid(),
        };
        store.check_verified_original_output_paired(
            &self.binding.attempt,
            &output.file,
            expected,
            &binding,
        )?;
        let metadata = crate::setup::nofollow::metadata_nofollow(
            &self.private.file,
            Path::new("original-output"),
        )?;
        let parent = self
            .private
            .open_child(OsStr::new("original-output"), &metadata)?;
        let pin = store.completed_original_setup_directory(&parent, &self.binding.attempt)?;
        if mount_id(&pin.file)? != self.binding.mount_id {
            return Err(refused("Completed setup generation mount changed"));
        }
        self.check()?;
        Ok(pin)
    }
    /// Only the canonical supervisor can mint ConfirmedShutdown. Last mount lease
    /// must be owned after every admission/Store clone has been dropped. This
    /// confirms cleanup, never verifies ciphertext or publishes installation.
    pub fn finish(
        self: Arc<Self>,
        exit: &launcher::owned_service::ConfirmedShutdown,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if !exit.matches(key, work, super::super::runtime::PACKAGE) {
            return Err(refused("Original output namespace shutdown is unconfirmed"));
        }
        self.check_mount_custody()?;
        let owned = Arc::try_unwrap(self)
            .map_err(|_| refused("Original output still has live custody leases"))?;
        let Self {
            daemon,
            selected,
            read,
            disk,
            partition,
            root,
            private,
            output,
            inventory_scratch,
            ..
        } = owned;
        // Close all held paths only after actual namespace death. UDisks must
        // report successful owned unmount before cleanup is acknowledged.
        drop((inventory_scratch, disk, partition, root, private, output));
        let source_retirement = read.retire();
        // Attempt the independently owned output cleanup even if a source
        // remains quarantined. Success requires BOTH observed retirements.
        let output_retirement = daemon
            .unmount(&selected.name)
            .map_err(|_| refused("Original output owned unmount failed"));
        source_retirement?;
        output_retirement
    }
}
impl ServiceAuthority for NativeMediumOutput {
    fn check(&self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.read.revalidate(&AtomicBool::new(false))?;
        self.check_mount_custody()?;
        if let Some(scratch) = self
            .inventory_scratch
            .lock()
            .map_err(|_| refused("Original inventory storage lock failed"))?
            .as_ref()
        {
            let _ = scratch.files()?;
        }
        Ok(())
    }
}
impl OwnedMediumOutput for NativeMediumOutput {
    fn inventory_scratch(&self) -> io::Result<(File, u64)> {
        self.check_mount_custody()?;
        let mut held = self
            .inventory_scratch
            .lock()
            .map_err(|_| refused("Original inventory storage lock failed"))?;
        if held.is_none() {
            let plan = self
                .read
                .output_selection_declaration(&AtomicBool::new(false))?;
            *held = Some(super::inventory_scratch::MediumScratch::create(
                self.private.clone(),
                &self.binding,
                plan.requested_caller().primary_gid(),
            )?);
        }
        let result = held
            .as_ref()
            .ok_or_else(|| refused("Original inventory storage missing"))?
            .files()?;
        self.check_mount_custody()?;
        Ok(result)
    }
    fn admit_application_output(&self, proof: &OriginalApplicationsOutput) -> io::Result<()> {
        let cancel = AtomicBool::new(false);
        self.check()?;
        proof.validate_for(&self.read, &cancel)?;
        // Uses the same approved source/projection admission as both supervised
        // namespaces. Never launch or substitute ambient OSTree/GPG tools here.
        proof.verify_signed_capsule()?;
        self.check_mount_custody()?;
        proof.validate_for(&self.read, &cancel)
    }
    fn completed_directory_paired(
        &self,
        store: &Store,
        expected: &super::super::store::OriginalOutputExpectations<'_>,
    ) -> io::Result<Arc<PinnedDirectory>> {
        NativeMediumOutput::completed_directory_paired(self, store, expected)
    }
    fn output_directory(&self) -> io::Result<Arc<PinnedDirectory>> {
        NativeMediumOutput::output_directory(self)
    }
    fn completed_directory(
        &self,
        store: &Store,
        expected: &crate::setup::portable_home_media::Expectation,
    ) -> io::Result<Arc<PinnedDirectory>> {
        NativeMediumOutput::completed_directory(self, store, expected)
    }
    fn committed_selection_directory(&self) -> io::Result<Arc<PinnedDirectory>> {
        self.check()?;
        let actual = witness(&self.root, &self.media, &self.selected.uuid)?;
        let before = self.private.file.metadata()?;
        let after = actual.file.metadata()?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino())
            || mount_id(&actual.file)? != self.binding.mount_id
        {
            return Err(refused("Committed selection private directory changed"));
        }
        actual.validate_destination()?;
        self.check()?;
        Ok(actual)
    }
    fn publish_verified_receipt(
        &self,
        expected: &crate::setup::portable_home_media::Expectation,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        let pair = super::super::store::OriginalOutputExpectations {
            home: expected,
            applications: None,
            reserve_bytes: self.binding.reserve_bytes,
        };
        self.publish_verified_receipt_paired(&pair, exit, key, work)
    }
    fn publish_verified_receipt_paired(
        &self,
        expected: &super::super::store::OriginalOutputExpectations<'_>,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if !exit.matches(key, work, super::super::runtime::PACKAGE) {
            return Err(refused("Original publication requires verifier quiescence"));
        }
        self.check()?;
        expected.validate()?;
        let cancel = AtomicBool::new(self.cancellation.is_cancelled());
        if self.read.output_flatpak_required(&cancel)? != expected.applications.is_some()
            || self.read.output_flatpak_operation(&cancel)?.as_deref()
                != expected.applications.map(|apps| apps.operation.as_str())
        {
            return Err(refused(
                "Original application selection differs from paired output",
            ));
        }
        if expected.reserve_bytes != self.binding.reserve_bytes {
            return Err(refused("Original publication exceeds admitted budget"));
        }
        let held = self.output.metadata()?;
        if (held.dev(), held.ino()) != self.binding.output
            || ![0, self.binding.original_uid].contains(&held.uid())
            || held.uid() == 0 && held.gid() != 0
            || held.mode() & 0o7777 != 0o700
        {
            return Err(refused("Original publication stage changed"));
        }
        // Explicitly retire this fresh worker stage after the whole verifier's
        // actual namespace is dead. No live writer authority survives ownership
        // transfer, and unrelated/user-owned directories are never adopted.
        if held.uid() != 0 && unsafe { libc::fchown(self.output.as_raw_fd(), 0, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        self.output.sync_all()?;
        self.check_mount_custody()?;
        let metadata = crate::setup::nofollow::metadata_nofollow(
            &self.private.file,
            Path::new("original-output"),
        )?;
        let parent = self
            .private
            .open_child(OsStr::new("original-output"), &metadata)?;
        let name = format!("stage-{}", self.binding.attempt);
        let metadata = crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(&name))?;
        let pin = parent.open_child(OsStr::new(&name), &metadata)?;
        let actual = pin.file.metadata()?;
        if (actual.dev(), actual.ino()) != self.binding.output
            || actual.uid() != 0
            || actual.gid() != 0
            || actual.mode() & 0o7777 != 0o700
        {
            return Err(refused("Original publication retired stage changed"));
        }
        let grant = crate::setup::portable_home_media::MediaGrant::from_reviewed_pin(pin)?;
        grant.publish_expectation(expected.home, &cancel)?;
        if let Some(applications) = expected.applications {
            // The same independent paired verifier's whole namespace is dead.
            // Publication rehashes the actual sealed application bytes before
            // issuing its separate canonical receipt; it never decrypts here.
            grant.publish_applications_expectation(applications, &cancel)?;
        }
        self.check_mount_custody()
    }
    fn publish_verified_applications_receipt(
        &self,
        expected: &crate::setup::flatpak_transfer::portable::Expectation,
        exit: &launcher::owned_service::QuiescentExit,
        key: &launcher::owned_service::ServiceLeaseKey,
        work: &Path,
    ) -> io::Result<()> {
        if !exit.matches(key, work, super::super::runtime::PACKAGE) {
            return Err(refused("Original publication requires verifier quiescence"));
        }
        self.check()?;
        expected.validate()?;
        let cancel = AtomicBool::new(self.cancellation.is_cancelled());
        let plan = self.read.output_applications_declaration(&cancel)?;
        if self.read.output_flatpak_operation(&cancel)?.as_deref()
            != Some(expected.operation.as_str())
            || expected.setup_binding != plan.setup_binding()
            || expected.sealed_bytes > self.binding.reserve_bytes
        {
            return Err(refused("Applications-only publication selection differs"));
        }
        let held = self.output.metadata()?;
        if (held.dev(), held.ino()) != self.binding.output
            || ![0, self.binding.original_uid].contains(&held.uid())
            || held.uid() == 0 && held.gid() != 0
            || held.mode() & 0o7777 != 0o700
        {
            return Err(refused("Original publication stage changed"));
        }
        // Explicitly retire this fresh worker stage after the whole verifier's
        // actual namespace is dead. No live writer authority survives ownership
        // transfer, and unrelated/user-owned directories are never adopted.
        if held.uid() != 0 && unsafe { libc::fchown(self.output.as_raw_fd(), 0, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        self.output.sync_all()?;
        self.check_mount_custody()?;
        let metadata = crate::setup::nofollow::metadata_nofollow(
            &self.private.file,
            Path::new("original-output"),
        )?;
        let parent = self
            .private
            .open_child(OsStr::new("original-output"), &metadata)?;
        let name = format!("stage-{}", self.binding.attempt);
        let metadata = crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(&name))?;
        let pin = parent.open_child(OsStr::new(&name), &metadata)?;
        let actual = pin.file.metadata()?;
        if (actual.dev(), actual.ino()) != self.binding.output
            || actual.uid() != 0
            || actual.gid() != 0
            || actual.mode() & 0o7777 != 0o700
        {
            return Err(refused("Original publication retired stage changed"));
        }
        let grant = crate::setup::portable_home_media::MediaGrant::from_reviewed_pin(pin)?;
        grant.publish_applications_expectation(expected, &cancel)?;
        self.check_mount_custody()
    }
    fn completed_applications_directory(
        &self,
        store: &Store,
        expected: &crate::setup::flatpak_transfer::portable::Expectation,
    ) -> io::Result<Arc<PinnedDirectory>> {
        self.check()?;
        let cancel = AtomicBool::new(self.cancellation.is_cancelled());
        let plan = self.read.output_applications_declaration(&cancel)?;
        if self.read.output_flatpak_operation(&cancel)?.as_deref()
            != Some(expected.operation.as_str())
            || expected.setup_binding != plan.setup_binding()
        {
            return Err(refused("Applications-only completed selection differs"));
        }
        let output = self.output_directory()?;
        let binding = super::super::store::StageBinding {
            declaration: plan.canonical_digest()?,
            filesystem: self.binding.filesystem_uuid,
            uid: plan.requested_caller().uid(),
            gid: plan.requested_caller().primary_gid(),
        };
        store.check_verified_original_applications(
            &self.binding.attempt,
            &output.file,
            expected,
            self.binding.reserve_bytes,
            &binding,
        )?;
        let metadata = crate::setup::nofollow::metadata_nofollow(
            &self.private.file,
            Path::new("original-output"),
        )?;
        let parent = self
            .private
            .open_child(OsStr::new("original-output"), &metadata)?;
        let pin = store.completed_original_setup_directory(&parent, &self.binding.attempt)?;
        if mount_id(&pin.file)? != self.binding.mount_id {
            return Err(refused("Applications-only completed mount changed"));
        }
        self.check()?;
        Ok(pin)
    }
    fn files(&self) -> io::Result<OriginalOutputFiles> {
        self.check()?;
        Ok(OriginalOutputFiles {
            source: self.read.output_original_home(&AtomicBool::new(false))?,
            output: self.output.try_clone()?,
            disk: self.disk.try_clone()?,
            partition: self.partition.try_clone()?,
            mount_root: self.root.file.try_clone()?,
        })
    }
    fn mount_binding(&self) -> io::Result<OriginalOutputBinding> {
        self.check()?;
        Ok(self.binding.clone())
    }
}
// Deliberately no Drop unmount: this value can outlive callback return and cannot
// establish namespace death. Explicit finish requires canonical ConfirmedShutdown
// and last lease ownership. Uncertain attempts
// retain their operation record and mount; they are not advertised as complete.

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_output_native_selection_refuses_regular_descriptor_and_wrong_actual_mount() {
        let file = tempfile::tempfile().unwrap();
        let binding = MediaPartitionBinding {
            schema: 1,
            target_stable_id: "fixture".into(),
            disk_guid: uuid::Uuid::new_v4().to_string(),
            partition_guid: uuid::Uuid::new_v4().to_string(),
            number: 1,
            first_lba: 2048,
            last_lba: 4095,
            sector_bytes: 512,
            profile_offset: 0,
        };
        assert!(selected(&[], &file, &binding).is_err());
        let root = File::open("/").unwrap();
        let id = mount_id(&root).unwrap();
        let metadata = root.metadata().unwrap();
        let number = format!(
            "{}:{}",
            libc::major(metadata.dev()),
            libc::minor(metadata.dev())
        );
        assert!(unique_mount(&number, id.checked_add(1).unwrap()).is_err());
    }
}
